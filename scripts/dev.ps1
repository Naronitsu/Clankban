# Runs Clankban from source for testing, beside the installed app.
#
# The dev app uses its own API port and its own data folder, so your real board is not touched.
# Run it from the project root:
#
#   .\scripts\dev.ps1           start the dev app and keep its data
#   .\scripts\dev.ps1 -Seed     add a demo board (sprints, tags, relations, comments, a view)
#   .\scripts\dev.ps1 -Reset    delete the dev data first, then start empty
#
# To point an agent (Claude Code) at the dev app, set CLANKBAN_API_URL to the dev address
# in the MCP server's environment, for example http://127.0.0.1:4188.
# Close the dev window, or press Ctrl+C here, to stop it.

param(
    [int]$Port = 4188,
    [string]$DataDir = (Join-Path $env:USERPROFILE 'clankban-dev'),
    [switch]$Seed,
    [switch]$Reset
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'src-tauri'
$api = "http://127.0.0.1:$Port"

if ($Port -eq 4187) {
    throw 'Port 4187 is the installed app. Pick another port for the dev app.'
}
if ($Reset -and (Test-Path $DataDir)) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset the dev data in $DataDir"
}
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null

function Invoke-DevApi($method, $path, $body) {
    $json = $null
    if ($null -ne $body) { $json = $body | ConvertTo-Json -Depth 8 }
    return Invoke-RestMethod -Method $method -Uri ($api + $path) -ContentType 'application/json' -Body $json
}

function Add-DemoBoard {
    $boards = (Invoke-DevApi 'GET' '/api/workspaces' $null).workspaces
    if ($boards | Where-Object { $_.id -eq 'demo-board' }) {
        Write-Host 'The demo board is already there; leaving it as it is.'
        return
    }
    Invoke-DevApi 'POST' '/api/workspaces' @{ name = 'Demo board' } | Out-Null
    $board = 'demo-board'

    $sprint = Invoke-DevApi 'POST' '/api/sprints' @{ workspace = $board; name = 'Sprint 1'; lengthDays = 14 }
    Invoke-DevApi 'PATCH' "/api/sprints/$($sprint.id)" @{ status = 'active' } | Out-Null

    $design = Invoke-DevApi 'POST' '/api/tasks' @{
        workspace = $board
        title = 'Design the sign-in page'
        priority = 'high'
        status = 'progress'
        tags = @('important')
        sprint = $sprint.id
        links = @('https://example.com/spec')
        description = @'
Check the **shared** button first.

- Keep the form short
- Use the spec link below

```js
const signIn = await api.login(user);
```

See https://example.com/spec
'@
    }
    Invoke-DevApi 'POST' "/api/tasks/$($design.id)/comments" @{ body = 'First pass is done. The spec link has the copy.' } | Out-Null

    $palette = Invoke-DevApi 'POST' '/api/tasks' @{
        workspace = $board
        title = 'Pick a colour palette'
        priority = 'medium'
        status = 'review'
        tags = @('on hold')
    }
    Invoke-DevApi 'POST' '/api/tasks' @{
        workspace = $board
        title = 'Check contrast on the new palette'
        parentId = $design.id
        standalone = $false
        standaloneNote = 'Needs the palette first'
        blockedBy = @($palette.id)
        status = 'backlog'
    } | Out-Null
    Invoke-DevApi 'POST' '/api/tasks' @{
        workspace = $board
        title = 'Write the sign-in API tests'
        priority = 'high'
        status = 'backlog'
        blockedBy = @($design.id)
        relatesTo = @($palette.id)
        sprint = $sprint.id
    } | Out-Null
    Invoke-DevApi 'POST' '/api/tasks' @{ workspace = $board; title = 'Fix the old login typo'; priority = 'low'; status = 'done' } | Out-Null

    Invoke-DevApi 'POST' '/api/views' @{
        workspace = $board
        name = 'Sprint 1, high priority'
        layout = 'list'
        filters = @{ priority = 'high'; sprint = "$($sprint.id)" }
    } | Out-Null
    Write-Host 'Added the demo board: Sprint 1 (running), tags, a blocked subtask, a comment, a link, and a saved view.'
}

Write-Host "Dev app: $api   data: $DataDir"
Push-Location $src
try {
    $env:CLANKBAN_PORT = "$Port"
    $env:CLANKBAN_DATA_DIR = $DataDir
    $dev = Start-Process -FilePath 'cargo' -ArgumentList 'tauri', 'dev' -NoNewWindow -PassThru

    if ($Seed) {
        # The first build takes a while, so wait for the API to answer.
        $ready = $false
        for ($i = 0; $i -lt 180 -and -not $ready; $i++) {
            if ($dev.HasExited) { throw 'The dev app stopped before its API came up.' }
            try {
                Invoke-DevApi 'GET' '/api/tasks' $null | Out-Null
                $ready = $true
            } catch {
                Start-Sleep -Seconds 2
            }
        }
        if (-not $ready) { throw "The dev API at $api did not answer in time." }
        Add-DemoBoard
    }

    $dev.WaitForExit()
} finally {
    Pop-Location
}
