#requires -Version 7.2

param(

    [Parameter(Mandatory = $true)]
    [string]$RcPath,

    [Parameter(Mandatory = $true)]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [string]$ResultPath,

    [Parameter(Mandatory = $true)]
    [string]$StdlibPath
)

$ErrorActionPreference = 'Stop'

function Resolve-PwDirEnvLiteralScript {
    <#
    .SYNOPSIS
        Resolve scripts literally before dot-sourcing so wildcard characters remain part of the filename.
    #>
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-Item -LiteralPath $Path).FullName
}

function Write-PwDirEnvResult {
    <#
    .SYNOPSIS
        Write UTF-8 JSON without BOM through a same-directory temporary file, separate from diagnostic streams.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][object]$Payload
    )

    $dir = Split-Path -Parent $Path
    $tmp = Join-Path -Path $dir -ChildPath ([IO.Path]::GetRandomFileName() + '.tmp')
    $json = $Payload | ConvertTo-Json -Depth 20 -Compress

    [IO.File]::WriteAllText($tmp, $json, [Text.UTF8Encoding]::new($false))

    [IO.File]::Move($tmp, $Path, $true)
}

try {

    $script:__PwDirEnvState = @{
        ProjectRoot  = $ProjectRoot
        CurrentSourcePath = $RcPath
        Watches      = @()
        SourcedFiles = @()
    }

    # Keep the writer reference if the RC defines a function with the same name.
    $__PwDirEnvResultWriterFn = ${function:Write-PwDirEnvResult}

    . (Resolve-PwDirEnvLiteralScript -Path $StdlibPath)

    Set-Location -LiteralPath $ProjectRoot

    . (Resolve-PwDirEnvLiteralScript -Path $RcPath)

    $vars = [ordered]@{}
    foreach ($entry in [System.Environment]::GetEnvironmentVariables('Process').GetEnumerator() | Sort-Object -Property Key) {
        $vars[[string]$entry.Key] = [string]$entry.Value
    }

    # Exclude the variable injected by the evaluator's ExecutionPolicy flag.
    $policyArtifactKeys = @(
        $vars.Keys | Where-Object { [string]$_ -ieq 'PSExecutionPolicyPreference' }
    )
    foreach ($artifactKey in $policyArtifactKeys) {
        $null = $vars.Remove([string]$artifactKey)
    }

    $payload = [ordered]@{
        schema        = 1
        ok            = $true
        environment   = [ordered]@{ vars = $vars }
        watches       = @($script:__PwDirEnvState.Watches)
        sourced_files = @($script:__PwDirEnvState.SourcedFiles)
        diagnostics   = @()
    }

    & $__PwDirEnvResultWriterFn -Path $ResultPath -Payload $payload

    exit 0
}
catch {

    $errorPayload = [ordered]@{
        schema = 1
        ok     = $false
        error  = [ordered]@{
            kind      = 'evaluation-failed'
            message   = [string]$_.Exception.Message
            exception = [string]$_.Exception.GetType().FullName
            stack     = [string]$_.ScriptStackTrace
        }
    }
    try {
        & $__PwDirEnvResultWriterFn -Path $ResultPath -Payload $errorPayload
    }
    catch {

    }
    exit 1
}
