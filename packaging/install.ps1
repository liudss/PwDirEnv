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
New-Item -ItemType Directory -Force -Path $distBin, $distModule | Out-Null
Copy-Item -LiteralPath $exe -Destination (Join-Path $distBin 'pwdir.exe') -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Destination $distModule -Force
Copy-Item -LiteralPath (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psm1') -Destination $distModule -Force

# ---- 2) 拷贝到 InstallRoot ------------------------------------------------
$installBin = Join-Path $InstallRoot 'bin'
$installModule = Join-Path $InstallRoot 'modules\PwDirEnv'
New-Item -ItemType Directory -Force -Path $installBin, $installModule | Out-Null
Copy-Item -LiteralPath (Join-Path $distBin 'pwdir.exe') -Destination $installBin -Force
Copy-Item -LiteralPath (Join-Path $distModule 'PwDirEnv.psd1') -Destination $installModule -Force
Copy-Item -LiteralPath (Join-Path $distModule 'PwDirEnv.psm1') -Destination $installModule -Force

# ---- 3) 验证 + 汇报 --------------------------------------------------------
$installedExe = Join-Path $installBin 'pwdir.exe'
$installedPsd1 = Join-Path $installModule 'PwDirEnv.psd1'
foreach ($artifact in @($installedExe, $installedPsd1, (Join-Path $installModule 'PwDirEnv.psm1'))) {
    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
        throw "installation incomplete: $artifact is missing"
    }
}
& $installedExe --version
if ($LASTEXITCODE -ne 0) { throw 'installed pwdir.exe failed to start' }

Write-Host ''
Write-Host "PwDirEnv installed to $InstallRoot"
Write-Host ''
Write-Host 'Next steps (not applied automatically):'
Write-Host "  1) Add '$installBin' to PATH, or set:  `$env:PWDIRENV_EXE = '$installedExe'"
Write-Host "  2) Import-Module '$installedPsd1'"
Write-Host "  3) & '$installedExe' hook install --yes"
