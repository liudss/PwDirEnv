#Requires -Version 7.2

Set-StrictMode -Version Latest

$script:ModuleVersion = '0.1.0'
$script:ProtocolSchema = 1

# Restore before module removal also deletes its global prompt wrapper.
$MyInvocation.MyCommand.ScriptBlock.Module.OnRemove = {
    if ($script:PwDirState.Enabled) {
        Restore-PwDirEnvironment
        if ($null -ne $script:PwDirState.OriginalPrompt) {
            Set-Item -LiteralPath 'function:global:prompt' -Value $script:PwDirState.OriginalPrompt
        }
        elseif (Get-Item -LiteralPath 'function:global:prompt' -ErrorAction Ignore) {
            Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
        }
        $script:PwDirState.Enabled = $false
        $script:PwDirState.OriginalPrompt = $null
    }
}

$script:PwDirState = @{
    ActiveRc       = $null
    Baseline       = @{}      # managed key -> [pscustomobject]@{ Existed=[bool]; Value=[string] }

    Applied        = @{}
    Fingerprint    = $null    # Reserved; native status reports disk cache freshness.
    HookRunning    = $false
    Enabled        = $false
    OriginalPrompt = $null
    PwDirExe       = $null
    LastStderr     = ''
}

$script:AbsentMarker = [Guid]::NewGuid().ToString()

$script:PromptMarker = '__PwDirEnv_prompt_wrapper__'

$script:ContextVariable = 'PWDIRENV_CONTEXT'
$script:ContextSchema = 1

# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------

function Resolve-PwDirExe {
    <#
    .SYNOPSIS
        Resolve an explicit path, PWDIRENV_EXE, PATH, or a development build, in that order.
    #>

    param([string]$PwDirPath)

    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($PwDirPath)) {

        $candidates += $PwDirPath
    }
    elseif (-not [string]::IsNullOrWhiteSpace($env:PWDIRENV_EXE)) {
        $candidates += $env:PWDIRENV_EXE
    }
    else {
        $cmd = Get-Command -Name 'pwdir.exe' -CommandType Application -ErrorAction Ignore
        if ($cmd) { $candidates += @($cmd)[0].Source }

        $candidates += (Join-Path $PSScriptRoot '..\..\target\debug\pwdir.exe')
        $candidates += (Join-Path $PSScriptRoot '..\..\target\release\pwdir.exe')
    }

    foreach ($candidate in $candidates) {
        if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).ProviderPath
        }
    }
    throw "PwDirEnv: pwdir.exe not found. Install or set -PwDirPath / `$env:PWDIRENV_EXE, or run 'cargo build --workspace'."
}

function Get-PwDirCurrentFileSystemPath {
    <#
    .SYNOPSIS
        Return a usable filesystem location, or null so the hook can unload.
    #>

    try {
        $loc = Get-Location
    }
    catch {

        return $null
    }
    if ($loc.Provider.Name -ne 'FileSystem') { return $null }
    $path = $loc.ProviderPath
    if (-not [IO.Directory]::Exists($path)) { return $null }
    return $path
}

function Invoke-PwDirNative {
    <#
    .SYNOPSIS
        Parse export JSON as data. Force bypasses shared cache for a session that needs a full patch.
    #>

    param([switch]$Force)

    $exe = $script:PwDirState.PwDirExe
    if ([string]::IsNullOrEmpty($exe) -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw "pwdir.exe is missing at '$exe'. Install or set -PwDirPath / `$env:PWDIRENV_EXE."
    }

    $savedEap = $ErrorActionPreference
    $nativeVar = Get-Variable -Name 'PSNativeCommandUseErrorActionPreference' -ErrorAction Ignore
    $savedNative = if ($nativeVar) { $nativeVar.Value } else { $null }
    $ErrorActionPreference = 'Continue'
    if ($nativeVar) { $PSNativeCommandUseErrorActionPreference = $false }
    try {
        $nativeArgs = @('export', '--format', 'json')
        if ($Force) { $nativeArgs += '--force' }
        $output = & $exe @nativeArgs 2>&1
    }
    finally {
        $ErrorActionPreference = $savedEap
        if ($nativeVar) { $PSNativeCommandUseErrorActionPreference = $savedNative }
    }

    $stdout = [System.Collections.Generic.List[string]]::new()
    $stderr = [System.Collections.Generic.List[string]]::new()
    foreach ($item in @($output)) {
        if ($item -is [System.Management.Automation.ErrorRecord]) {
            $stderr.Add([string]$item)
        }
        else {
            $stdout.Add([string]$item)
        }
    }

    $stderrText = ($stderr -join "`n").Trim()
    $script:PwDirState.LastStderr = (($script:PwDirState.LastStderr + "`n" + $stderrText).Trim())
    if ($script:PwDirState.LastStderr.Length -gt 8192) {
        $script:PwDirState.LastStderr = $script:PwDirState.LastStderr.Substring($script:PwDirState.LastStderr.Length - 8192)
    }

    $jsonText = ($stdout -join "`n").Trim()
    if ([string]::IsNullOrWhiteSpace($jsonText)) {
        throw "pwdir export produced no JSON (exit $LASTEXITCODE). stderr: $stderrText"
    }
    $result = $jsonText | ConvertFrom-Json
    if ($result.schema -ne $script:ProtocolSchema) {
        throw "unsupported export protocol schema $($result.schema) (expected $script:ProtocolSchema)"
    }
    return $result
}

function Remove-PwDirProcessEnvVar {
    <#
    .SYNOPSIS
        Use NullString for true deletion: PowerShell can marshal ordinary null as an empty string.
    #>

    param([string]$Name)
    [Environment]::SetEnvironmentVariable($Name, [NullString]::Value, 'Process')
}

function Export-PwDirContext {
    <#
    .SYNOPSIS
        Encode managed restoration values for nested shells. Base64 is not encryption; never log this payload.
    #>

    if ([string]::IsNullOrEmpty($script:PwDirState.ActiveRc)) {
        Remove-PwDirProcessEnvVar $script:ContextVariable
        return
    }

    $baseline = [ordered]@{}
    foreach ($key in @($script:PwDirState.Baseline.Keys)) {
        $entry = $script:PwDirState.Baseline[$key]
        $value = if ($entry.Existed) { [string]$entry.Value } else { $null }
        $baseline[$key] = [ordered]@{ existed = [bool]$entry.Existed; value = $value }
    }
    $applied = [ordered]@{}
    foreach ($key in @($script:PwDirState.Applied.Keys)) {
        $value = $script:PwDirState.Applied[$key]
        $exists = ($value -ne $script:AbsentMarker)
        $applied[$key] = [ordered]@{
            exists = $exists
            value = if ($exists) { [string]$value } else { $null }
        }
    }
    $payload = [ordered]@{
        schema = $script:ContextSchema
        rc = [string]$script:PwDirState.ActiveRc
        baseline = $baseline
        applied = $applied
    }
    $json = $payload | ConvertTo-Json -Depth 8 -Compress
    $bytes = [Text.Encoding]::UTF8.GetBytes($json)
    [Environment]::SetEnvironmentVariable(
        $script:ContextVariable,
        [Convert]::ToBase64String($bytes),
        'Process'
    )
}

function Import-PwDirContext {
    <#
    .SYNOPSIS
        Best-effort restoration of inherited context. It is not a trust credential; native export still checks authorization.
    #>

    $raw = [Environment]::GetEnvironmentVariable($script:ContextVariable, 'Process')
    if ([string]::IsNullOrWhiteSpace($raw)) { return }
    try {
        $json = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($raw))
        $payload = $json | ConvertFrom-Json
        if ($payload.schema -ne $script:ContextSchema -or [string]::IsNullOrWhiteSpace([string]$payload.rc)) {
            return
        }

        foreach ($prop in @($payload.baseline.PSObject.Properties)) {
            $entry = $prop.Value
            $existed = [bool]$entry.existed
            $script:PwDirState.Baseline[[string]$prop.Name] = [pscustomobject]@{
                Existed = $existed
                Value = if ($existed) { [string]$entry.value } else { $null }
            }
        }
        foreach ($prop in @($payload.applied.PSObject.Properties)) {
            $entry = $prop.Value
            $script:PwDirState.Applied[[string]$prop.Name] = if ([bool]$entry.exists) {
                [string]$entry.value
            } else {
                $script:AbsentMarker
            }
        }
        $script:PwDirState.ActiveRc = [string]$payload.rc
    }
    catch {

    }
}

function Apply-PwDirPatch {
    <#
    .SYNOPSIS
        Apply only Process-scope changes. Capture each original value once and never evaluate native output as code.
    #>

    param($Result)

    foreach ($prop in $Result.patch.set.PSObject.Properties) {
        $key = [string]$prop.Name
        if ([string]::IsNullOrEmpty($key)) { continue }
        $value = [string]$prop.Value
        if (-not $script:PwDirState.Baseline.ContainsKey($key)) {

            $current = [Environment]::GetEnvironmentVariable($key, 'Process')
            $script:PwDirState.Baseline[$key] = [pscustomobject]@{
                Existed = ($null -ne $current)
                Value   = $current
            }
        }

        [Environment]::SetEnvironmentVariable($key, $value, 'Process')
        $script:PwDirState.Applied[$key] = $value
    }

    foreach ($name in @($Result.patch.unset)) {
        $key = [string]$name
        if ([string]::IsNullOrEmpty($key)) { continue }
        if (-not $script:PwDirState.Baseline.ContainsKey($key)) {
            $current = [Environment]::GetEnvironmentVariable($key, 'Process')
            $script:PwDirState.Baseline[$key] = [pscustomobject]@{
                Existed = ($null -ne $current)
                Value   = $current
            }
        }
        Remove-PwDirProcessEnvVar $key
        $script:PwDirState.Applied[$key] = $script:AbsentMarker
    }

    $script:PwDirState.ActiveRc = $Result.rc
}

function Restore-PwDirEnvironment {
    <#
    .SYNOPSIS
        Restore original existence and values for all managed keys, discarding manual changes to those keys.
    #>

    foreach ($key in @($script:PwDirState.Baseline.Keys)) {
        $entry = $script:PwDirState.Baseline[$key]
        if ($entry.Existed) {

            [Environment]::SetEnvironmentVariable($key, $entry.Value, 'Process')
        }
        else {

            Remove-PwDirProcessEnvVar $key
        }
    }
    $script:PwDirState.Baseline.Clear()
    $script:PwDirState.Applied.Clear()
    $script:PwDirState.ActiveRc = $null
    Export-PwDirContext
}

function Write-PwDirLog {
    <#
    .SYNOPSIS
        Write diagnostics. Callers must avoid including environment values in ordinary patch summaries.
    #>

    param(
        [string]$Level = 'info',
        [string]$Message = ''
    )
    $line = "pwdir: $Message"
    switch ($Level) {
        'error' { Write-Host $line -ForegroundColor Red }
        'warn'  { Write-Host $line -ForegroundColor Yellow }
        default { Write-Host $line }
    }
}

function Write-PwDirExportSummary {
    <#
    .SYNOPSIS
        Summarize added, modified, and removed variable names without their values.
    #>

    param($Result)

    $parts = @()
    foreach ($prop in $Result.patch.set.PSObject.Properties) {
        $entry = $script:PwDirState.Baseline[$prop.Name]
        $mark = if ($entry -and $entry.Existed) { '~' } else { '+' }
        $parts += "$mark$($prop.Name)"
    }
    foreach ($name in @($Result.patch.unset)) {
        $parts += "-$name"
    }
    if ($parts.Count -gt 0) {
        Write-PwDirLog info ('export ' + ($parts -join ' '))
    }
}

# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------

function Enable-PwDirEnv {
    <#
    .SYNOPSIS
        Enable directory environments in this session and wrap the existing global prompt. Repeated calls are harmless.
    #>

    [CmdletBinding()]
    param(

        [string]$PwDirPath = ''
    )

    if ($script:PwDirState.Enabled) {
        Write-PwDirLog info 'already enabled'
        return
    }

    $script:PwDirState.PwDirExe = Resolve-PwDirExe -PwDirPath $PwDirPath

    Import-PwDirContext

    $existing = Get-Item -LiteralPath 'function:prompt' -ErrorAction Ignore
    if ($existing -and ("$($existing.ScriptBlock)" -like "*$script:PromptMarker*")) {
        if ($null -eq $script:PwDirState.OriginalPrompt) {
            $mirror = Get-Variable -Name 'PwDirEnvSavedPrompt' -Scope Global -ErrorAction Ignore
            if ($mirror) { $script:PwDirState.OriginalPrompt = $mirror.Value }
        }
    }
    elseif ($existing) {
        $script:PwDirState.OriginalPrompt = $existing.ScriptBlock
        $global:PwDirEnvSavedPrompt = $script:PwDirState.OriginalPrompt
    }
    else {

        $mirror = Get-Variable -Name 'PwDirEnvSavedPrompt' -Scope Global -ErrorAction Ignore
        if ($mirror) { $script:PwDirState.OriginalPrompt = $mirror.Value }
    }

    function global:prompt {
        $__pwdirLastExitCode = Get-Variable -Name LASTEXITCODE -Scope Global -ValueOnly -ErrorAction Ignore
        $__pwdirWrapper = $script:PromptMarker
        try {
            Invoke-PwDirEnvHook
        }
        finally {
            $global:LASTEXITCODE = $__pwdirLastExitCode
        }
        if ($null -ne $script:PwDirState.OriginalPrompt) {
            & $script:PwDirState.OriginalPrompt
        }
        else {
            "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
        }
    }

    $script:PwDirState.Enabled = $true
}

function Disable-PwDirEnv {
    <#
    .SYNOPSIS
        Restore managed environment variables and the saved prompt in this session.
    #>

    [CmdletBinding()]
    param()

    if (-not $script:PwDirState.Enabled) {
        Write-PwDirLog info 'already disabled'
        return
    }

    Restore-PwDirEnvironment

    if ($null -ne $script:PwDirState.OriginalPrompt) {
        Set-Item -LiteralPath 'function:global:prompt' -Value $script:PwDirState.OriginalPrompt
    }
    else {
        Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
    }
    $script:PwDirState.OriginalPrompt = $null
    $script:PwDirState.Enabled = $false
    Write-PwDirLog info 'disabled'
}

function Invoke-PwDirEnvHook {
    <#
    .SYNOPSIS
        Check native export at each prompt. Restore before switching projects; keep last-known-good values on same-project failures.
    #>

    [CmdletBinding()]
    param()

    if ($script:PwDirState.HookRunning) { return }
    if (-not $script:PwDirState.Enabled) { return }
    $script:PwDirState.HookRunning = $true
    try {
        $cwd = Get-PwDirCurrentFileSystemPath
        if ($null -eq $cwd) {

            if ($null -ne $script:PwDirState.ActiveRc) {
                Restore-PwDirEnvironment
                Write-PwDirLog info 'current directory unavailable; unloaded managed environment'
            }
            return
        }

        $result = Invoke-PwDirNative
        $newRc = $result.rc
        $oldRc = $script:PwDirState.ActiveRc
        $rcChanged = ($newRc -ne $oldRc)

        if ($rcChanged) {

            if ($null -ne $oldRc) {
                Restore-PwDirEnvironment
                Write-PwDirLog info "unloading $oldRc"
            }
            if ($null -ne $newRc) {

                $needsForcedExport = ($null -ne $oldRc) -or ($result.action -eq 'noop')
                if ($needsForcedExport) {
                    $result = Invoke-PwDirNative -Force
                    $newRc = $result.rc
                }
            }
        }
        if ($null -eq $newRc) { return }

        switch ($result.action) {
            'apply' {
                if ($rcChanged) { Write-PwDirLog info "loading $newRc" }
                Apply-PwDirPatch $result
                Export-PwDirContext
                Write-PwDirExportSummary $result
            }
            'noop' {
                if ($rcChanged) {
                    Write-PwDirLog info "loading $newRc"

                    $script:PwDirState.ActiveRc = $newRc
                    Export-PwDirContext
                }
            }
            'error' {

                $detail = (@($result.diagnostics) | ForEach-Object { $_.message }) -join '; '
                if ($rcChanged) {
                    Write-PwDirLog error "evaluation failed; keeping baseline environment$(if ($detail) { ": $detail" })"
                }
                else {
                    Write-PwDirLog error "evaluation failed; keeping previous environment$(if ($detail) { ": $detail" })"
                }
            }
            'blocked' {

                if ($rcChanged) {
                    Write-PwDirLog warn ".envrc.ps1 is blocked; run 'pwdir allow' (previous environment restored)"
                }
                else {
                    Write-PwDirLog warn ".envrc.ps1 is blocked; run 'pwdir allow' (keeping last-known-good environment)"
                }
            }
            'unload' {

                if ($null -ne $script:PwDirState.ActiveRc) {
                    Restore-PwDirEnvironment
                    Write-PwDirLog info 'unloaded managed environment'
                }
            }
            default {
                Write-PwDirLog warn "unknown action '$($result.action)'; ignored"
            }
        }
    }
    catch {

        Write-PwDirLog error ('hook failed: ' + $_.Exception.Message)
    }
    finally {
        $script:PwDirState.HookRunning = $false
    }
}

function Get-PwDirEnvStatus {
    <#
    .SYNOPSIS
        Return this session state without exposing managed variable values.
    #>

    [CmdletBinding()]
    param()

    [PSCustomObject]@{
        Version       = $script:ModuleVersion
        Enabled       = $script:PwDirState.Enabled
        PwDirExe      = $script:PwDirState.PwDirExe
        ActiveRc      = $script:PwDirState.ActiveRc
        ManagedKeys   = @($script:PwDirState.Applied.Keys)
        HookRunning   = $script:PwDirState.HookRunning
        BaselineCount = $script:PwDirState.Baseline.Count
        Fingerprint   = $script:PwDirState.Fingerprint
    }
}

Export-ModuleMember -Function @(
    'Enable-PwDirEnv',
    'Disable-PwDirEnv',
    'Invoke-PwDirEnvHook',
    'Get-PwDirEnvStatus'
)
