# Daedalus non-destructive PowerShell shell integration (OSC 7 + OSC 133)
if (-not $global:__daedalus_ps_initialized) {
    $global:__daedalus_ps_initialized = $true
    $global:__daedalus_ps_first_prompt = $true
    $global:__daedalus_ps_orig_prompt = $function:prompt

    function global:prompt {
        $__daedalus_ok = $?
        $__daedalus_last_ec = $global:LASTEXITCODE
        $__daedalus_esc = [char]0x1b
        $__daedalus_bel = [char]0x07

        if (-not $global:__daedalus_ps_first_prompt) {
            $__daedalus_ec = if (-not $__daedalus_ok) {
                if ($null -ne $__daedalus_last_ec -and $__daedalus_last_ec -ne 0) { $__daedalus_last_ec } else { 1 }
            } else {
                0
            }
            [Console]::Write("${__daedalus_esc}]133;D;${__daedalus_ec}${__daedalus_bel}")
        }
        $global:__daedalus_ps_first_prompt = $false

        $__daedalus_host = if ($env:COMPUTERNAME) { $env:COMPUTERNAME } elseif ($env:HOSTNAME) { $env:HOSTNAME } else { 'localhost' }
        $__daedalus_raw_path = ($PWD.ProviderPath -replace '\\', '/').TrimStart('/')
        $__daedalus_segments = $__daedalus_raw_path -split '/' | ForEach-Object {
            if ($_ -match '^[A-Za-z]:$') { $_ } else { [System.Uri]::EscapeDataString($_) }
        }
        $__daedalus_encoded_path = '/' + ($__daedalus_segments -join '/')
        [Console]::Write("${__daedalus_esc}]7;file://${__daedalus_host}${__daedalus_encoded_path}${__daedalus_bel}")
        [Console]::Write("${__daedalus_esc}]133;A${__daedalus_bel}")

        $__daedalus_prompt_text = if ($null -ne $global:__daedalus_ps_orig_prompt) {
            & $global:__daedalus_ps_orig_prompt
        } else {
            "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
        }
        "${__daedalus_prompt_text}${__daedalus_esc}]133;B${__daedalus_bel}"
    }

    Import-Module PSReadLine -ErrorAction SilentlyContinue
    if (Get-Module -Name PSReadLine) {
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            param($key, $arg)
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine($key, $arg)
            [Console]::Write("$([char]0x1b)]133;C$([char]0x07)")
        }
    }
}
