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
    notes     = "Clankban $Version"
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
$latest | ConvertTo-Json -Depth 6 | Set-Content -Path $latestPath -Encoding utf8

# Portable single-file build, for people who do not want the installer.
$portableName = "Clankban-$Version-portable.exe"
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\clankban.exe') (Join-Path $outDir $portableName) -Force

Write-Host ''
Write-Host "Release files for $Version are in $outDir"
Get-ChildItem $outDir | ForEach-Object { Write-Host "  $($_.Name)" }
Write-Host ''
Write-Host 'To publish (not run by this script):'
Write-Host "  gh release create v$Version `"$outDir\$installerName`" `"$outDir\$installerName.sig`" `"$latestPath`" `"$outDir\$portableName`" --repo $Repo --title `"Clankban $Version`" --notes `"Clankban $Version`""
