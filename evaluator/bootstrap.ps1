#requires -Version 7.2
# PwDirEnv evaluator bootstrap（SPEC §17）。
# 由 native core 以 `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File` 启动。
# 协议结果只写入 ResultPath 指向的临时文件（写完 atomic rename），绝不与 stdout/stderr 混用（SPEC §7.5）。
param(
    # RC 文件绝对路径（trust 已由 native core 在启动前校验通过）
    [Parameter(Mandatory = $true)]
    [string]$RcPath,

    # RC 所属项目 root，evaluator 执行 RC 前的 cwd
    [Parameter(Mandatory = $true)]
    [string]$ProjectRoot,

    # native core 创建的结果文件路径，bootstrap 写入结构化 JSON result
    [Parameter(Mandatory = $true)]
    [string]$ResultPath
)

# M0 骨架：SPEC §17 规定的 10 步执行序列，M2 填充实现。
# 1.  $ErrorActionPreference = 'Stop'
# 2.  初始化 internal watch/source collections（$script:__PwDirEnvState）
# 3.  加载 stdlib（stdlib.ps1）
# 4.  Set-Location -LiteralPath $ProjectRoot
# 5.  dot-source RC
# 6.  枚举 Process environment 作为 resulting environment
# 7.  构造 JSON result（environment + watches + sourced files + diagnostics）
# 8.  写临时 result（当前用户专用目录、不可预测名称）
# 9.  atomic move 至 ResultPath
# 10. exit 0
#
# 捕获异常时（M2）：写结构化 error result（含 exception type/message/script stack trace），exit 非 0。
exit 0
