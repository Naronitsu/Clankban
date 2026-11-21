# Builds a signed Clankban release and writes the files GitHub needs.
#
# Usage (from the project root):
#   .\scripts\release.ps1 -Version 0.1.1
#
# It does NOT publish. It prints the `gh release create` command to run once you
# have checked the output in release\v<version>\.
#
# Requirements:
#   - The version is set to the same value in src-tauri\Cargo.toml and src-tauri\tauri.conf.json.
#   - CHANGELOG.md has a "## <version>" section. It becomes the update notes and the release notes.
#   - The updater private key exists at the path given by -KeyPath (created with
#     `cargo tauri signer generate`). Keep it out of the repository.

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Repo = 'Naronitsu/Clankban',
    [string]$KeyPath = (Join-Path $env:USERPROFILE '.tauri\clankban.key')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'src-tauri'

if (-not (Test-Path $KeyPath)) { throw "Signing key not found at $KeyPath" }

$conf = Get-Content (Join-Path $src 'tauri.conf.json') -Raw | ConvertFrom-Json
if ($conf.version -ne $Version) { throw "tauri.conf.json is version $($conf.version), expected $Version" }
$cargoVersion = (Select-String -Path (Join-Path $src 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVersion -ne $Version) { throw "Cargo.toml is version $cargoVersion, expected $Version" }

# The release notes are the version's section in CHANGELOG.md. Without one, do not release.
$lines = (Get-Content (Join-Path $root 'CHANGELOG.md') -Raw) -split "`r?`n"
$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match "^## $([regex]::Escape($Version))(\s|$)") { $start = $i; break }
}
if ($start -lt 0) { throw "CHANGELOG.md has no '## $Version' section. Write the release notes first." }
$body = @()
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^## ') { break }
    $body += $lines[$i]
}
$notes = ($body -join "`n").Trim()

# Build into its own folder so a running Clankban does not block the build.
$env:TAURI_SIGNING_PRIVATE_KEY = $KeyPath
$env:CARGO_TARGET_DIR = Join-Path $src 'target-release'
Push-Location $src
try { cargo tauri build } finally { Pop-Location }

$bundle = Join-Path $env:CARGO_TARGET_DIR 'release\bundle\nsis'
$installer = Get-ChildItem $bundle -Filter "*_${Version}_x64-setup.exe" | Select-Object -First 1
if (-not $installer) { throw "Installer for $Version not found in $bundle" }
$sigPath = "$($installer.FullName).sig"
if (-not (Test-Path $sigPath)) { throw "Signature not found: $sigPath" }

$signature = (Get-Content $sigPath -Raw).Trim()
$installerName = $installer.Name
$latest = [ordered]@{
    version   = $Version
    notes     = $notes
    pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            signature = $signature
            url       = "https://github.com/$Repo/releases/download/v$Version/$installerName"
        }
    }
}

$outDir = Join-Path $root "release\v$Version"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Copy-Item $installer.FullName $outDir -Force
Copy-Item $sigPath $outDir -Force
$latestPath = Join-Path $outDir 'latest.json'
[System.IO.File]::WriteAllText($latestPath, ($latest | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))

# Portable single-file build, for people who do not want the installer.
$portableName = "Clankban-$Version-portable.exe"
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\clankban.exe') (Join-Path $outDir $portableName) -Force

$notesPath = Join-Path $outDir 'release-notes.md'
[System.IO.File]::WriteAllText($notesPath, $notes + "`n", (New-Object System.Text.UTF8Encoding($false)))

Write-Host ''
Write-Host "Release files for $Version are in $outDir"
Get-ChildItem $outDir | ForEach-Object { Write-Host "  $($_.Name)" }
Write-Host ''
Write-Host 'To publish (not run by this script):'
Write-Host "  gh release create v$Version `"$outDir\$installerName`" `"$outDir\$installerName.sig`" `"$latestPath`" `"$outDir\$portableName`" --repo $Repo --title `"Clankban $Version`" --notes-file `"$notesPath`""
