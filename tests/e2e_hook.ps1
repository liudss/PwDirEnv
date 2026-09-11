#requires -Version 7.2

#

#

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$pwdir = Join-Path $repoRoot 'target\debug\pwdir.exe'
if (-not (Test-Path -LiteralPath $pwdir)) {
    throw "pwdir.exe not found at $pwdir — run 'cargo build --workspace' first"
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("pwdir-e2e-hook-" + [guid]::NewGuid().ToString('N'))
$dirA = Join-Path $tempRoot 'A'
$dirAChild = Join-Path $dirA 'child'
$dirB = Join-Path $tempRoot 'B'
$dirOut = Join-Path $tempRoot 'outside'
New-Item -ItemType Directory -Force -Path $dirAChild, $dirB, $dirOut | Out-Null

$rcA = Join-Path $dirA '.envrc.ps1'
$rcB = Join-Path $dirB '.envrc.ps1'

Set-Content -LiteralPath $rcA -Value @'
$env:PWDIR_E2E = 'A'
$env:PWDIR_E2E_A_ONLY = 'aaa'
'@
Set-Content -LiteralPath $rcB -Value @'
$env:PWDIR_E2E = 'B'
$env:PWDIR_E2E_B_ONLY = 'bbb'
'@

$savedLocalAppData = $env:LOCALAPPDATA
$appDataRoot = Join-Path $tempRoot 'appdata'
New-Item -ItemType Directory -Force -Path $appDataRoot | Out-Null
$env:LOCALAPPDATA = $appDataRoot

function Write-Ok { param([string]$Message) Write-Output ("  [ok] " + $Message) }
function Get-LogText { param($Records) (@($Records) | ForEach-Object { "$_" }) -join "`n" }

$failed = $false
try {

    (& $pwdir allow $dirA) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "allow A failed (exit $LASTEXITCODE)" }
    (& $pwdir allow $dirB) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "allow B failed (exit $LASTEXITCODE)" }
    Write-Ok 'pwdir allow A/B (trust store isolated to temp LOCALAPPDATA)'

    Set-Location -LiteralPath $dirOut

    function global:prompt { 'my> ' }

    Import-Module (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Force
    Enable-PwDirEnv -PwDirPath $pwdir

    if ((prompt) -ne 'my> ') { throw "prompt not preserved after Enable: '$(prompt)'" }
    $status = Get-PwDirEnvStatus
    if (-not $status.Enabled) { throw 'status.Enabled should be true after Enable' }
    if ($status.PwDirExe -ne $pwdir) { throw "status.PwDirExe mismatch: $($status.PwDirExe)" }
    Write-Ok 'prompt preserved after Enable-PwDirEnv; status.Enabled=true'

    Invoke-PwDirEnvHook
    if ($null -ne $env:PWDIR_E2E) { throw "step a: PWDIR_E2E should be absent, got '$($env:PWDIR_E2E)'" }
    Write-Ok 'a. outside: PWDIR_E2E absent (baseline kept)'

    Set-Location -LiteralPath $dirA
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step b: expected 'A', got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_A_ONLY -ne 'aaa') { throw "step b: expected A_ONLY='aaa', got '$($env:PWDIR_E2E_A_ONLY)'" }
    $status = Get-PwDirEnvStatus
    if ($status.ActiveRc -ne $rcA) { throw "step b: ActiveRc mismatch: $($status.ActiveRc)" }
    Write-Ok "b. cd A: PWDIR_E2E=A + A_ONLY applied, ActiveRc=$rcA"

    Set-Location -LiteralPath $dirAChild
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step c: expected 'A', got '$($env:PWDIR_E2E)'" }
    $status = Get-PwDirEnvStatus
    if ($status.ActiveRc -ne $rcA) { throw 'step c: ActiveRc should stay A (upward discovery)' }
    Write-Ok 'c. cd A\child: still A (inheritance via upward discovery)'

    Set-Location -LiteralPath $dirB
    $logsB = Get-LogText (Invoke-PwDirEnvHook 6>&1)
    if ($env:PWDIR_E2E -ne 'B') { throw "step d: expected 'B', got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_B_ONLY -ne 'bbb') { throw "step d: expected B_ONLY='bbb', got '$($env:PWDIR_E2E_B_ONLY)'" }
    if ($null -ne $env:PWDIR_E2E_A_ONLY) { throw "step d: A_ONLY not restored before applying B: '$($env:PWDIR_E2E_A_ONLY)'" }
    if ($logsB -notmatch 'unloading') { throw "step d: missing 'unloading' log: $logsB" }
    if ($logsB -notmatch 'loading') { throw "step d: missing 'loading' log: $logsB" }
    if ($logsB -notmatch '\+PWDIR_E2E_B_ONLY') { throw "step d: missing export summary for B keys: $logsB" }
    Write-Ok "d. cd B: A restored then B applied ($(($logsB -split "`n") -join ' | '))"

    $env:PWDIR_E2E = 'manual'
    Set-Location -LiteralPath $dirOut
    Invoke-PwDirEnvHook
    if ($null -ne $env:PWDIR_E2E) { throw "step e: manual change must be dropped on unload, got '$($env:PWDIR_E2E)'" }
    if ($null -ne $env:PWDIR_E2E_B_ONLY) { throw 'step e: B_ONLY must be removed on unload' }
    $status = Get-PwDirEnvStatus
    if ($null -ne $status.ActiveRc) { throw 'step e: ActiveRc should be null after leaving all projects' }
    Write-Ok 'e. manual change + leave: managed keys restored to absent'

    Set-Location -LiteralPath $dirA
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step f: expected 'A', got '$($env:PWDIR_E2E)'" }
    Write-Ok 'f. cd A again: second load works'

    $childScript = @"
`$InformationPreference = 'SilentlyContinue'
Import-Module '$($repoRoot.Replace("'", "''"))\powershell\PwDirEnv\PwDirEnv.psd1' -Force
Enable-PwDirEnv -PwDirPath '$($pwdir.Replace("'", "''"))'
Invoke-PwDirEnvHook 6>`$null
`$s = Get-PwDirEnvStatus
if (`$s.ActiveRc -ne '$($rcA.Replace("'", "''"))') { throw "child ActiveRc mismatch: `$(`$s.ActiveRc)" }
if (`$env:PWDIR_E2E -ne 'A') { throw "child did not inherit A environment: `$env:PWDIR_E2E" }
Set-Location -LiteralPath '$($dirOut.Replace("'", "''"))'
Invoke-PwDirEnvHook 6>`$null
if (`$null -ne `$env:PWDIR_E2E -or `$null -ne `$env:PWDIR_E2E_A_ONLY) { throw 'child did not restore inherited baseline on leave' }
"@
    $encodedChild = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($childScript))
    $childLogs = Get-LogText (& pwsh -NoLogo -NoProfile -NonInteractive -EncodedCommand $encodedChild 6>&1)
    if ($LASTEXITCODE -ne 0) { throw "nested child pwsh failed (exit $LASTEXITCODE): $childLogs" }
    if ($childLogs -match 'pwdir: loading') { throw "nested child unexpectedly reloaded RC: $childLogs" }
    Write-Ok 'f2. nested pwsh inherited context: no reload; leaving restored baseline'

    Set-Content -LiteralPath $rcA -Value @'
$env:PWDIR_E2E = 'A-tampered'
$env:PWDIR_E2E_A_ONLY = 'aaa'
'@
    $logsG = Get-LogText (Invoke-PwDirEnvHook 6>&1)
    if ($env:PWDIR_E2E -ne 'A') { throw "step g: last-known-good violated, got '$($env:PWDIR_E2E)'" }
    if ($logsG -notmatch 'blocked') { throw "step g: expected blocked log, got: $logsG" }
    Write-Ok "g. tampered RC -> blocked, last-known-good 'A' kept ($(($logsG -split "`n") -join ' | '))"

    (& $pwdir allow $dirA) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 're-allow after tamper failed' }
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A-tampered') { throw "step g2: expected 'A-tampered' after re-allow, got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_A_ONLY -ne 'aaa') { throw "step g2: A_ONLY should stay 'aaa', got '$($env:PWDIR_E2E_A_ONLY)'" }
    Write-Ok "g2. re-allow tampered RC -> same-rc reload applies new content"

    Disable-PwDirEnv
    if ($null -ne $env:PWDIR_E2E) { throw "step h: PWDIR_E2E should be absent after Disable, got '$($env:PWDIR_E2E)'" }
    if ($null -ne $env:PWDIR_E2E_A_ONLY) { throw 'step h: A_ONLY should be absent after Disable' }
    if ((prompt) -ne 'my> ') { throw "step h: prompt not restored after Disable: '$(prompt)'" }
    $status = Get-PwDirEnvStatus
    if ($status.Enabled) { throw 'step h: Enabled should be false after Disable' }
    Write-Ok 'h. Disable-PwDirEnv: env restored to baseline, original prompt restored'

    Write-Output 'E2E HOOK OK'
}
catch {
    Write-Output "E2E HOOK FAILED: $_"
    $failed = $true
}
finally {

    Set-Location -LiteralPath $repoRoot -ErrorAction Ignore
    Disable-PwDirEnv -ErrorAction Ignore
    $env:LOCALAPPDATA = $savedLocalAppData
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction Ignore
}
if ($failed) { exit 1 } else { exit 0 }
