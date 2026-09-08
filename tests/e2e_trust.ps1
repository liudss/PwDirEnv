#requires -Version 7.2
# M1 手动 E2E：discovery + trust 全流程演示与验证。
#
# 流程：建临时目录树 → 写 .envrc.ps1 → pwdir allow → status=allowed
#       → 改一个 byte → status=blocked → pwdir deny → status=none
#       → 退出码矩阵验证（allow 成功 0 / 无 rc allow 2 / IO 3 / deny 幂等 0）。
#
# 隔离：整个脚本把 $env:LOCALAPPDATA 指向临时目录，trust record 不写入真实用户目录。
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$pwdir = Join-Path $repoRoot 'target\debug\pwdir.exe'
if (-not (Test-Path -LiteralPath $pwdir)) {
    throw "pwdir.exe not found at $pwdir — run 'cargo build --workspace' first"
}

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("pwdir-e2e-trust-" + [guid]::NewGuid().ToString('N'))
$project = Join-Path $tempRoot 'project'
$child = Join-Path $project 'child'
New-Item -ItemType Directory -Path $child -Force | Out-Null

$rc = Join-Path $project '.envrc.ps1'
Set-Content -LiteralPath $rc -Value "`$env:E2E_FOO = 'bar'" -NoNewline

# 隔离 trust 存储：pwdir 子进程继承该环境
$savedLocalAppData = $env:LOCALAPPDATA
$env:LOCALAPPDATA = $tempRoot

function Get-PwDirStatusJson {
    $raw = (& $pwdir status --json) -join "`n"
    return ($raw | ConvertFrom-Json)
}

$failed = $false
try {
    # 1. 从子目录 allow（验证向上发现）→ exit 0
    Push-Location $child
    $out = (& $pwdir allow) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "allow failed (exit $LASTEXITCODE): $out" }
    if ($out -notlike 'pwdir: allowed *') { throw "unexpected allow output: $out" }
    Write-Output "  [1] allow (from child dir): exit 0, output: $out"

    # 2. status（人类输出 + JSON）显示 allowed，且发现的是 project 下的 RC
    $human = (& $pwdir status) -join "`n"
    if ($human -notmatch 'trust status:\s+allowed') { throw "human status not allowed: $human" }
    $status = Get-PwDirStatusJson
    if ($status.trust_status -ne 'allowed') { throw "expected allowed, got $($status.trust_status)" }
    if ($status.discovered_rc.rc_path -ne $rc) {
        throw "discovered rc mismatch: $($status.discovered_rc.rc_path) != $rc"
    }
    if (-not $status.rc_sha256 -or $status.rc_sha256.Length -ne 64) {
        throw "rc_sha256 missing or not 64 hex chars: $($status.rc_sha256)"
    }
    if ($status.dirs.allow -ne (Join-Path $tempRoot 'PwDirEnv\allow')) {
        throw "dirs.allow not isolated: $($status.dirs.allow)"
    }
    Write-Output "  [2] status: allowed (rc=$($status.discovered_rc.rc_path), sha256=$($status.rc_sha256.Substring(0,12))...)"

    # 3. 改一个 byte → blocked（内容 hash 失效，M1 验收核心）
    Set-Content -LiteralPath $rc -Value "`$env:E2E_FOO = 'baz'" -NoNewline
    $status = Get-PwDirStatusJson
    if ($status.trust_status -ne 'blocked') { throw "expected blocked after byte change, got $($status.trust_status)" }
    Write-Output "  [3] status after byte change: blocked"

    # 4. deny → none；deny 幂等（无 record 再 deny 仍 exit 0）
    (& $pwdir deny) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "deny failed (exit $LASTEXITCODE)" }
    $status = Get-PwDirStatusJson
    if ($status.trust_status -ne 'none') { throw "expected none after deny, got $($status.trust_status)" }
    (& $pwdir deny) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "idempotent deny failed (exit $LASTEXITCODE)" }
    Write-Output "  [4] deny (x2, idempotent): exit 0, status: none"

    # 5. 无 rc 的目录 allow → exit 2 + 精确 stderr
    $bare = Join-Path $tempRoot 'bare'
    New-Item -ItemType Directory -Path $bare -Force | Out-Null
    Push-Location $bare
    $errOut = (& $pwdir allow 2>&1) -join "`n"
    $codeNoRc = $LASTEXITCODE
    Pop-Location
    if ($codeNoRc -ne 2) { throw "expected exit 2 for allow without rc, got $codeNoRc" }
    if ($errOut -ne 'pwdir: no .envrc.ps1 found in this or any parent directory') {
        throw "unexpected no-rc stderr: $errOut"
    }
    Write-Output "  [5] allow without rc: exit 2, stderr: $errOut"

    # 6. 显式不存在的 rc 文件路径 → IO error → exit 3
    $ghost = Join-Path $tempRoot 'ghost\.envrc.ps1'
    $errOut = (& $pwdir allow $ghost 2>&1) -join "`n"
    $codeIo = $LASTEXITCODE
    if ($codeIo -ne 3) { throw "expected exit 3 for IO error, got $codeIo" }
    Write-Output "  [6] allow missing explicit file: exit 3, stderr: $errOut"

    # 7. 无 rc 的目录 deny → exit 0（nothing to deny）
    Push-Location $bare
    $out = (& $pwdir deny) -join "`n"
    $codeDenyBare = $LASTEXITCODE
    Pop-Location
    if ($codeDenyBare -ne 0) { throw "expected exit 0 for deny without rc, got $codeDenyBare" }
    if ($out -ne 'pwdir: nothing to deny') { throw "unexpected deny output: $out" }
    Write-Output "  [7] deny without rc: exit 0, output: $out"

    Write-Output 'E2E TRUST OK'
} catch {
    Write-Output "E2E TRUST FAILED: $_"
    $failed = $true
} finally {
    Pop-Location -ErrorAction Ignore
    $env:LOCALAPPDATA = $savedLocalAppData
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction Ignore
}

if ($failed) { exit 1 } else { exit 0 }
