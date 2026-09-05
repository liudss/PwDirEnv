//! Evaluator launcher：spawn 隔离 pwsh 子进程（M2 实现，ADR-002）。

/// TODO(M2): spawn `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File bootstrap.ps1`、
/// 临时结果文件通道、超时 kill 进程树、结构化 diagnostics 收集。
pub fn placeholder() -> u32 {
    0
}
