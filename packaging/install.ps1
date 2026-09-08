#requires -Version 7.0
<#
.SYNOPSIS
    PwDirEnv 安装脚本（M7）：构建 + 组装 dist 布局 + 拷贝到 InstallRoot。

.DESCRIPTION
    - 默认安装到 $env:LOCALAPPDATA\Programs\PwDirEnv；
    - -SkipBuild 跳过 `cargo build --release`（使用现有 target\release 产物）；
    - 标准布局（winget 友好，pwdir doctor 的 module 检查按此布局探测）：
        <InstallRoot>\bin\pwdir.exe
        <InstallRoot>\modules\PwDirEnv\PwDirEnv.psd1
        <InstallRoot>\modules\PwDirEnv\PwDirEnv.psm1
        <InstallRoot>\skills\pwdirenv\SKILL.md   # Agent Skills 格式配套技能
    - 不自动修改系统/用户 PATH —— 只打印后续步骤，由用户决定（最小侵入）。
#>
param(
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'Programs\PwDirEnv'),
    [switch]$SkipBuild
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath

if (-not $SkipBuild) {
    Write-Host '==> cargo build --release'
    Push-Location $repoRoot
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
    }
    finally { Pop-Location }
}

$exe = Join-Path $repoRoot 'target\release\pwdir.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "pwdir.exe not found at $exe — build first (run without -SkipBuild)"
}

# ---- 1) 组装 dist\pwdir 标准布局 -----------------------------------------
$distBin = Join-Path $repoRoot 'dist\pwdir\bin'
$distModule = Join-Path $repoRoot 'dist\pwdir\modules\PwDirEnv'
$distSkillsRoot = Join-Path $repoRoot 'dist\pwdir\skills'
New-Item -ItemType Directory -Force -Path $distBin, $distModule, $distSkillsRoot | Out-Null
Copy-Item -LiteralPath $exe -Destination (Join-Path $distBin 'pwdir.exe') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Destination $distModule -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psm1') -Destination $distModule -Force
$skillSrcDir = Join-Path $repoRoot 'skills\pwdirenv'
if (Test-Path -LiteralPath (Join-Path $skillSrcDir 'SKILL.md') -PathType Leaf) {
    Copy-Item -LiteralPath $skillSrcDir -Destination $distSkillsRoot -Recurse -Force
}

# ---- 2) 拷贝到 InstallRoot ------------------------------------------------
$installBin = Join-Path $InstallRoot 'bin'
$installModule = Join-Path $InstallRoot 'modules\PwDirEnv'
$installSkillsRoot = Join-Path $InstallRoot 'skills'
New-Item -ItemType Directory -Force -Path $installBin, $installModule, $installSkillsRoot | Out-Null
Copy-Item -LiteralPath (Join-Path $distBin 'pwdir.exe') -Destination $installBin -Force
Copy-Item -LiteralPath (Join-Path $distModule 'PwDirEnv.psd1') -Destination $installModule -Force
Copy-Item -LiteralPath (Join-Path $distModule 'PwDirEnv.psm1') -Destination $installModule -Force
$distSkillDir = Join-Path $distSkillsRoot 'pwdirenv'
if (Test-Path -LiteralPath (Join-Path $distSkillDir 'SKILL.md') -PathType Leaf) {
    Copy-Item -LiteralPath $distSkillDir -Destination $installSkillsRoot -Recurse -Force
}

# ---- 3) 验证 + 汇报 --------------------------------------------------------
$installedExe = Join-Path $installBin 'pwdir.exe'
$installedPsd1 = Join-Path $installModule 'PwDirEnv.psd1'
$installedSkill = Join-Path $installSkillsRoot 'pwdirenv\SKILL.md'
foreach ($artifact in @($installedExe, $installedPsd1, (Join-Path $installModule 'PwDirEnv.psm1'), $installedSkill)) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
        throw "installation incomplete: $artifact is missing"
    }
}
& $installedExe --version
if ($LASTEXITCODE -ne 0) { throw 'installed pwdir.exe failed to start' }

Write-Host ''
Write-Host "PwDirEnv installed to $InstallRoot"
Write-Host ''
$skillDst = Join-Path $InstallRoot 'skills\pwdirenv'
Write-Host 'Next steps (not applied automatically):'
Write-Host "  1) Add '$installBin' to PATH, or set:  `$env:PWDIRENV_EXE = '$installedExe'"
Write-Host "  2) Import-Module '$installedPsd1'"
Write-Host "  3) & '$installedExe' hook install --yes"
Write-Host "  4) Install the agent skill (Agent Skills format, pick the dirs you use):"
Write-Host "       Copy-Item -Recurse '$skillDst' `"$HOME\.claude\skills\`"           # Claude Code"
Write-Host "       Copy-Item -Recurse '$skillDst' `"$HOME\.agents\skills\`"           # Codex / generic"
Write-Host "       Copy-Item -Recurse '$skillDst' `"$HOME\.config\opencode\skills\`"  # opencode"
