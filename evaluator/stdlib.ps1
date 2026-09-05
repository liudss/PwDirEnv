#requires -Version 7.2
# PwDirEnv stdlib（SPEC §10）：在 RC dot-source 之前由 bootstrap.ps1 加载，为 RC 提供 direnv 风格 helper。
# 内部 metadata 变量使用 __PwDirEnvState 命名空间，避免与 RC 用户变量冲突（SPEC §17）。

# 兜底初始化：bootstrap 步骤 2 已初始化 $script:__PwDirEnvState（dot-source 共享同一 script scope），
# 这里再防一次独立加载 / 未初始化场景，保证 helper 始终有可写的集合。
if ($null -eq $script:__PwDirEnvState) {
    $script:__PwDirEnvState = @{
        Watches      = @()
        SourcedFiles = @()
    }
}

# TODO(M5): 以下 helper 将在 M5 实现（当前为骨架占位，函数体随 M5 交付）：
#
#   PATH_add <path>          以 ProjectRoot 为基准解析绝对路径（允许路径不存在），
#                            prepend 到 $env:PATH；等价路径（大小写不敏感）去重后移到最前；
#                            分隔符用 [IO.Path]::PathSeparator。
#   dotenv [path]            解析 .env 并设置环境变量（KEY=value / 引号 / export 前缀 / 注释）；
#                            文件不存在时报错，使 evaluation 失败。
#   dotenv_if_exists [path]  同 dotenv，但文件不存在时静默跳过。
#   watch_file <path> [...]  注册文件依赖；相对路径相对于 ProjectRoot。
#   watch_dir <path> [...]   注册目录依赖。
#   source_env <path>        在当前 evaluator scope dot-source 指定 .ps1；
#                            自动纳入 watch 依赖与 evaluation fingerprint（SPEC §10.1）。
#   source_up [filename]     从父目录向上搜索最近的 .envrc.ps1（不含当前目录）并 source_env。
