$appPath = Join-Path $PSScriptRoot 'ui\index.html'
Start-Process $appPath
Write-Host "Opened Clankban locally from $appPath"
