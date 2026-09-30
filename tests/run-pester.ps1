#requires -Version 7.2
# Run in a fresh pwsh -NoProfile process to avoid installed-module conflicts.
$ErrorActionPreference = 'Stop'
Import-Module Pester -MinimumVersion 5.0 -MaximumVersion 5.999 -ErrorAction Stop
$config = New-PesterConfiguration
$config.Run.Path = Join-Path $PSScriptRoot 'PwDirEnv.Tests.ps1'
$config.Run.PassThru = $true
$config.Output.Verbosity = 'Detailed'
$config.TestResult.Enabled = $true
$config.TestResult.OutputPath = Join-Path $PSScriptRoot '../testResults.xml'
$result = Invoke-Pester -Configuration $config
if ($result.TotalCount -eq 0 -or $result.FailedCount -gt 0 -or
    $result.SkippedCount -gt 0 -or $result.NotRunCount -gt 0 -or $result.Result -ne 'Passed') {
    throw 'Pester must discover and pass every test without skips.'
}
