#requires -Version 7.2

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$modulePath = Join-Path -Path $PSScriptRoot -ChildPath '..\powershell\PwDirEnv\PwDirEnv.psd1'
Import-Module $modulePath -Force

$status = Get-PwDirEnvStatus
if ($null -eq $status) {
    throw 'Get-PwDirEnvStatus returned nothing'
}
if (-not $status.Version) {
    throw 'Get-PwDirEnvStatus returned no Version'
}

Write-Output "SMOKE OK (PwDirEnv $($status.Version))"
