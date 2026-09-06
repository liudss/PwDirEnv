#requires -Version 7.2
# M4 真 E2E（SPEC §24.6 release blocker）：真实 pwdir.exe + 真实 module 的全流程验收。
#
# 断言序列：baseline → A → A\child（继承）→ B（先卸 A 再装 B）→ outside 恢复
#           → blocked last-known-good → 同 rc reload → Disable 完整恢复 + prompt 保留。
#
# 隔离：LOCALAPPDATA 指向临时目录，trust record 不写入真实用户目录。
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$pwdir = Join-Path $repoRoot 'target\debug\pwdir.exe'
if (-not (Test-Path -LiteralPath $pwdir)) {
    throw "pwdir.exe not found at $pwdir — run 'cargo build --workspace' first"
}

# ---- 准备临时目录树 -------------------------------------------------------
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("pwdir-e2e-hook-" + [guid]::NewGuid().ToString('N'))
$dirA = Join-Path $tempRoot 'A'
$dirAChild = Join-Path $dirA 'child'
$dirB = Join-Path $tempRoot 'B'
$dirOut = Join-Path $tempRoot 'outside'
New-Item -ItemType Directory -Force -Path $dirAChild, $dirB, $dirOut | Out-Null

$rcA = Join-Path $dirA '.envrc.ps1'
$rcB = Join-Path $dirB '.envrc.ps1'
# A/B 共享 PWDIR_E2E（切换语义），另设各自独有 key（验证恢复是否真正发生）
Set-Content -LiteralPath $rcA -Value @'
$env:PWDIR_E2E = 'A'
$env:PWDIR_E2E_A_ONLY = 'aaa'
'@
Set-Content -LiteralPath $rcB -Value @'
$env:PWDIR_E2E = 'B'
$env:PWDIR_E2E_B_ONLY = 'bbb'
'@

# ---- trust 隔离：pwdir 子进程（含 evaluator 孙进程）继承该环境 ------------
$savedLocalAppData = $env:LOCALAPPDATA
$appDataRoot = Join-Path $tempRoot 'appdata'
New-Item -ItemType Directory -Force -Path $appDataRoot | Out-Null
$env:LOCALAPPDATA = $appDataRoot

function Write-Ok { param([string]$Message) Write-Output ("  [ok] " + $Message) }
function Get-LogText { param($Records) (@($Records) | ForEach-Object { "$_" }) -join "`n" }

$failed = $false
try {
    # 授权 A 与 B
    (& $pwdir allow $dirA) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "allow A failed (exit $LASTEXITCODE)" }
    (& $pwdir allow $dirB) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "allow B failed (exit $LASTEXITCODE)" }
    Write-Ok 'pwdir allow A/B (trust store isolated to temp LOCALAPPDATA)'

    # 预置到无 rc 目录，避免后续 prompt/hook 被仓库目录噪声干扰
    Set-Location -LiteralPath $dirOut

    # 自定义 prompt：必须定义在 global 作用域（与 $PROFILE 场景一致，模块才能看到并保留它）
    function global:prompt { 'my> ' }

    Import-Module (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Force
    Enable-PwDirEnv -PwDirPath $pwdir

    # prompt 保留（启用后立即验证：wrapper 先跑 hook 再回调原 prompt）
    if ((prompt) -ne 'my> ') { throw "prompt not preserved after Enable: '$(prompt)'" }
    $status = Get-PwDirEnvStatus
    if (-not $status.Enabled) { throw 'status.Enabled should be true after Enable' }
    if ($status.PwDirExe -ne $pwdir) { throw "status.PwDirExe mismatch: $($status.PwDirExe)" }
    Write-Ok 'prompt preserved after Enable-PwDirEnv; status.Enabled=true'

    # a) outside 初始：无 rc → 环境保持 baseline
    Invoke-PwDirEnvHook
    if ($null -ne $env:PWDIR_E2E) { throw "step a: PWDIR_E2E should be absent, got '$($env:PWDIR_E2E)'" }
    Write-Ok 'a. outside: PWDIR_E2E absent (baseline kept)'

    # b) 进入 A → 应用 A
    Set-Location -LiteralPath $dirA
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step b: expected 'A', got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_A_ONLY -ne 'aaa') { throw "step b: expected A_ONLY='aaa', got '$($env:PWDIR_E2E_A_ONLY)'" }
    $status = Get-PwDirEnvStatus
    if ($status.ActiveRc -ne $rcA) { throw "step b: ActiveRc mismatch: $($status.ActiveRc)" }
    Write-Ok "b. cd A: PWDIR_E2E=A + A_ONLY applied, ActiveRc=$rcA"

    # c) A\child：向上发现同一 rc → 继承，不重复切换
    Set-Location -LiteralPath $dirAChild
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step c: expected 'A', got '$($env:PWDIR_E2E)'" }
    $status = Get-PwDirEnvStatus
    if ($status.ActiveRc -ne $rcA) { throw 'step c: ActiveRc should stay A (upward discovery)' }
    Write-Ok 'c. cd A\child: still A (inheritance via upward discovery)'

    # d) 切到 B：先按 A 的 baseline 恢复，再从干净环境 export B（SPEC §8.4）
    Set-Location -LiteralPath $dirB
    $logsB = Get-LogText (Invoke-PwDirEnvHook 6>&1)
    if ($env:PWDIR_E2E -ne 'B') { throw "step d: expected 'B', got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_B_ONLY -ne 'bbb') { throw "step d: expected B_ONLY='bbb', got '$($env:PWDIR_E2E_B_ONLY)'" }
    if ($null -ne $env:PWDIR_E2E_A_ONLY) { throw "step d: A_ONLY not restored before applying B: '$($env:PWDIR_E2E_A_ONLY)'" }
    if ($logsB -notmatch 'unloading') { throw "step d: missing 'unloading' log: $logsB" }
    if ($logsB -notmatch 'loading') { throw "step d: missing 'loading' log: $logsB" }
    if ($logsB -notmatch '\+PWDIR_E2E_B_ONLY') { throw "step d: missing export summary for B keys: $logsB" }
    Write-Ok "d. cd B: A restored then B applied ($(($logsB -split "`n") -join ' | '))"

    # e) B 中手动改值 → 离开 → 恢复为「不存在」（SPEC §8.5：PwDirEnv owns managed keys）
    $env:PWDIR_E2E = 'manual'
    Set-Location -LiteralPath $dirOut
    Invoke-PwDirEnvHook
    if ($null -ne $env:PWDIR_E2E) { throw "step e: manual change must be dropped on unload, got '$($env:PWDIR_E2E)'" }
    if ($null -ne $env:PWDIR_E2E_B_ONLY) { throw 'step e: B_ONLY must be removed on unload' }
    $status = Get-PwDirEnvStatus
    if ($null -ne $status.ActiveRc) { throw 'step e: ActiveRc should be null after leaving all projects' }
    Write-Ok 'e. manual change + leave: managed keys restored to absent'

    # f) 二次加载 A
    Set-Location -LiteralPath $dirA
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A') { throw "step f: expected 'A', got '$($env:PWDIR_E2E)'" }
    Write-Ok 'f. cd A again: second load works'

    # f2) 子 pwsh 继承 PWDIRENV_CONTEXT：同一 RC 的首次 hook 只走 native
    # fingerprint fast path，不应再次 force evaluator / 输出 loading；随后离开项目
    # 仍须按继承的 baseline 卸载环境（direnv DIRENV_DIFF 等价语义）。
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

    # g) 篡改 A 的 RC（改一个 byte）→ trust 失效 → blocked → last-known-good 保持，不抛错
    Set-Content -LiteralPath $rcA -Value @'
$env:PWDIR_E2E = 'A-tampered'
$env:PWDIR_E2E_A_ONLY = 'aaa'
'@
    $logsG = Get-LogText (Invoke-PwDirEnvHook 6>&1)
    if ($env:PWDIR_E2E -ne 'A') { throw "step g: last-known-good violated, got '$($env:PWDIR_E2E)'" }
    if ($logsG -notmatch 'blocked') { throw "step g: expected blocked log, got: $logsG" }
    Write-Ok "g. tampered RC -> blocked, last-known-good 'A' kept ($(($logsG -split "`n") -join ' | '))"

    # g2) 重新 allow 篡改后的内容 → 同 rc 内 reload → 应用新值（内容变化被授权后生效）
    (& $pwdir allow $dirA) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 're-allow after tamper failed' }
    Invoke-PwDirEnvHook
    if ($env:PWDIR_E2E -ne 'A-tampered') { throw "step g2: expected 'A-tampered' after re-allow, got '$($env:PWDIR_E2E)'" }
    if ($env:PWDIR_E2E_A_ONLY -ne 'aaa') { throw "step g2: A_ONLY should stay 'aaa', got '$($env:PWDIR_E2E_A_ONLY)'" }
    Write-Ok "g2. re-allow tampered RC -> same-rc reload applies new content"

    # h) Disable → 环境恢复 baseline + 原 prompt 恢复
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
    # 清理：先离开临时目录（Windows 不允许删除 CWD），再禁用模块、还原隔离环境、删除临时树
    Set-Location -LiteralPath $repoRoot -ErrorAction Ignore
    Disable-PwDirEnv -ErrorAction Ignore
    $env:LOCALAPPDATA = $savedLocalAppData
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction Ignore
}
if ($failed) { exit 1 } else { exit 0 }
