param(
    [Parameter(Mandatory = $true)][string]$InstallDirectory,
    [Parameter(Mandatory = $true)][string]$Version
)

$ErrorActionPreference = 'Stop'
$root = Join-Path ([IO.Path]::GetTempPath()) "daedalus-msi-smoke-$([Guid]::NewGuid())"
$project = Join-Path $root 'installed-smoke'
$profile = Join-Path $root 'profile'
$daemonLog = Join-Path $root 'daemon.log'
$daemonErrors = Join-Path $root 'daemon-errors.log'
$uiLog = Join-Path $root 'ui.log'
$uiErrors = Join-Path $root 'ui-errors.log'
$state = Join-Path $root 'state.json'
$token = [Guid]::NewGuid().ToString('N')
$daemon = $null
$ui = $null
$workspaceId = $null
$previous = @{}
foreach ($key in @('DAEDALUS_SOCKET', 'DAEDALUS_AUTH_TOKEN', 'DAEDALUS_RUNTIME_DIR',
        'DAEDALUS_STATE_FILE', 'DAEDALUS_EVENTS_SOCKET', 'APPDATA', 'LOCALAPPDATA',
        'DAEDALUS_CONFIG', 'NO_COLOR', 'RUST_LOG')) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key)
}
$daemonExe = Join-Path $InstallDirectory 'daedalusd.exe'
$uiExe = Join-Path $InstallDirectory 'daedalus-ui.exe'
$cliExe = Join-Path $InstallDirectory 'daedalus-ctl.exe'

function Invoke-SmokeCli {
    param([string[]]$Arguments)
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $cliExe
    $info.Arguments = $Arguments -join ' '
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (!$process.WaitForExit(3000)) {
            $process.Kill()
            [void]$process.WaitForExit(3000)
            throw 'The installed CLI did not reply within three seconds.'
        }
        [pscustomobject]@{
            Code = $process.ExitCode
            Output = $stdout.GetAwaiter().GetResult()
            ErrorOutput = $stderr.GetAwaiter().GetResult()
        }
    } finally {
        $process.Dispose()
    }
}

function Wait-Smoke {
    param([string]$Description, [scriptblock]$Check)
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $result = & $Check
        if ($result) { return $result }
        Start-Sleep -Milliseconds 150
    } while ($timer.Elapsed.TotalSeconds -lt 25)
    throw "Timed out waiting for $Description."
}

function Start-SmokeUi {
    Start-Process -FilePath $uiExe -ArgumentList "--dir `"$project`" --agent shell --no-auto-spawn" `
        -PassThru -RedirectStandardOutput $uiLog -RedirectStandardError $uiErrors
}

function Stop-FixtureProcess {
    param($Process)
    if ($Process -and !$Process.HasExited) {
        if (!$Process.CloseMainWindow() -or !$Process.WaitForExit(3000)) {
            Stop-Process -Id $Process.Id -ErrorAction SilentlyContinue
            [void]$Process.WaitForExit(3000)
        }
    }
}

try {
    New-Item -ItemType Directory -Path $project, $profile -Force | Out-Null
    $env:APPDATA = $profile
    $env:LOCALAPPDATA = $profile
    $env:DAEDALUS_CONFIG = Join-Path $profile 'config.toml'
    $env:DAEDALUS_RUNTIME_DIR = Join-Path $root 'runtime'
    $env:DAEDALUS_STATE_FILE = $state
    $env:DAEDALUS_EVENTS_SOCKET = Join-Path $root 'events.sock'
    $env:DAEDALUS_SOCKET = 'tcp://127.0.0.1:0'
    $env:DAEDALUS_AUTH_TOKEN = $token
    $env:NO_COLOR = '1'
    $env:RUST_LOG = 'info'
    $daemon = Start-Process -FilePath $daemonExe -PassThru -NoNewWindow `
        -RedirectStandardOutput $daemonLog -RedirectStandardError $daemonErrors
    $env:DAEDALUS_SOCKET = Wait-Smoke 'the installed daemon endpoint' {
        if ($daemon.HasExited) { throw 'The installed daemon exited during startup.' }
        $text = Get-Content -LiteralPath $daemonLog -Raw -ErrorAction SilentlyContinue
        if ($text -match 'daedalusd listening[^\r\n]*(tcp://127\.0\.0\.1:[1-9][0-9]*)') {
            $Matches[1]
        }
    }

    $env:DAEDALUS_AUTH_TOKEN = 'invalid-smoke-token'
    $denied = Invoke-SmokeCli -Arguments @('ping')
    if ($denied.Code -eq 0 -or $denied.ErrorOutput -notmatch 'auth') {
        throw 'The installed daemon did not reject a mismatched token.'
    }
    $env:DAEDALUS_AUTH_TOKEN = $token
    $ready = Invoke-SmokeCli -Arguments @('ping')
    if ($ready.Code -ne 0 -or $ready.Output -notlike "daedalusd v$Version (*") {
        throw ("The authenticated handshake failed (exit {0}). Expected v{1}. Output: {2} {3}" -f
            $ready.Code, $Version, $ready.Output, $ready.ErrorOutput)
    }

    $ui = Start-SmokeUi
    $workspace = Wait-Smoke 'workspace and panes from the installed UI' {
        if ($ui.HasExited) { throw 'The installed UI exited during workspace startup.' }
        $list = Invoke-SmokeCli -Arguments @('workspace', 'list', '--json')
        if ($list.Code -ne 0) { throw 'The installed CLI could not list workspaces.' }
        $match = @($list.Output | ConvertFrom-Json | Where-Object { $_.name -eq 'installed-smoke' })
        if ($match.Count -eq 1 -and $match[0].pane_ids.Count -eq 3) { $match[0] }
    }
    $workspaceId = $workspace.id
    $panes = @($workspace.pane_ids | Sort-Object)
    Stop-FixtureProcess $ui
    $ui = Start-SmokeUi
    [void](Wait-Smoke 'reconnected UI window' {
        if ($ui.HasExited) { throw 'The installed UI exited on reconnect.' }
        $ui.Refresh()
        $ui.MainWindowHandle -ne [IntPtr]::Zero
    })
    $list = Invoke-SmokeCli -Arguments @('workspace', 'list', '--json')
    $after = @($list.Output | ConvertFrom-Json)
    if ($list.Code -ne 0 -or $after.Count -ne 1 -or $after[0].id -ne $workspaceId -or
        (Compare-Object $panes @($after[0].pane_ids | Sort-Object))) {
        throw 'Reconnect did not preserve the original workspace and pane IDs.'
    }
    Write-Output "Installed Windows workbench v$Version created and reconnected to the same workspace and panes."
} catch {
    foreach ($path in @($daemonLog, $daemonErrors, $uiLog, $uiErrors)) {
        if (Test-Path -LiteralPath $path) { Get-Content -LiteralPath $path -Tail 25 }
    }
    throw
} finally {
    Stop-FixtureProcess $ui
    if ($workspaceId -and $daemon -and !$daemon.HasExited) {
        try { [void](Invoke-SmokeCli -Arguments @('workspace', 'close', $workspaceId)) }
        catch { Write-Warning 'The smoke workspace did not close cleanly.' }
    }
    Stop-FixtureProcess $daemon
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key])
    }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
