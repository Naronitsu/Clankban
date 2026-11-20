# Builds a signed Clankban release and writes the files GitHub needs.
#
# Usage (from the project root):
#   .\scripts\release.ps1 -Version 0.2.0
#
# It does NOT publish. It prints the `gh release create` command to run once you have
# checked the output in release\v<version>\.
#
# Requirements:
#   - The version is the same in app\Cargo.toml.
#   - The signing key exists at -KeyPath (created with `cargo tauri signer generate`).
#     Keep it out of the repository.
#   - NSIS is installed. The default location is the one the Tauri CLI downloaded.

param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Repo = 'Naronitsu/Clankban',
    [string]$KeyPath = (Join-Path $env:USERPROFILE '.tauri\clankban.key'),
    [string]$Makensis = (Join-Path $env:LOCALAPPDATA 'tauri\NSIS\Bin\makensis.exe')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$app = Join-Path $root 'app'
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

if (-not (Test-Path $KeyPath)) { throw "Signing key not found at $KeyPath" }
if (-not (Test-Path $Makensis)) { throw "makensis not found at $Makensis. Install NSIS or pass -Makensis." }

$cargoVersion = (Select-String -Path (Join-Path $app 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVersion -ne $Version) { throw "app\Cargo.toml is version $cargoVersion, expected $Version" }

# 1. Release binary.
Push-Location $app
try { cargo build --release } finally { Pop-Location }
$exe = Join-Path $app 'target\release\clankban.exe'
if (-not (Test-Path $exe)) { throw "Release build not found at $exe" }

# 2. Installer.
$outDir = Join-Path $root "release\v$Version"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$installerName = "Clankban_${Version}_x64-setup.exe"
$installerPath = Join-Path $outDir $installerName
& $Makensis "/DVERSION=$Version" "/DBINARY=$exe" "/DOUTFILE=$installerPath" (Join-Path $app 'installer\clankban.nsi')
if ($LASTEXITCODE -ne 0) { throw "makensis failed with exit code $LASTEXITCODE" }

# 3. Signature (minisign, base64 in the .sig file, as the updater expects).
$env:TAURI_SIGNING_PRIVATE_KEY_PATH = $KeyPath
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ''
cargo tauri signer sign $installerPath
if ($LASTEXITCODE -ne 0) { throw "signing failed with exit code $LASTEXITCODE" }
$sigPath = "$installerPath.sig"
if (-not (Test-Path $sigPath)) { throw "Signature not written: $sigPath" }

# 4. Manifest the in-app updater reads. Written without a byte-order mark.
$latest = [ordered]@{
    version   = $Version
    notes     = "Clankban $Version"
    pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            signature = (Get-Content $sigPath -Raw).Trim()
            url       = "https://github.com/$Repo/releases/download/v$Version/$installerName"
        }
    }
}
$latestPath = Join-Path $outDir 'latest.json'
[System.IO.File]::WriteAllText($latestPath, ($latest | ConvertTo-Json -Depth 6), $utf8NoBom)

# 5. Portable single-file build.
$portableName = "Clankban-$Version-portable.exe"
Copy-Item $exe (Join-Path $outDir $portableName) -Force

Write-Host ''
Write-Host "Release files for $Version are in $outDir"
Get-ChildItem $outDir | ForEach-Object { Write-Host "  $($_.Name)" }
Write-Host ''
Write-Host 'To publish (not run by this script):'
Write-Host "  gh release create v$Version `"$installerPath`" `"$sigPath`" `"$latestPath`" `"$(Join-Path $outDir $portableName)`" --repo $Repo --title `"Clankban $Version`" --notes `"Clankban $Version`""
