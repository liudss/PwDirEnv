//! dotenv 解析（SPEC §11 / ADR-007）。
//!
//! ADR-007：首版 dotenv 解析器用 PowerShell 实现在 `evaluator/stdlib.ps1`
//! （`Read-PwDirDotEnv` / `ConvertFrom-PwDirDotEnvLine`），是对 SPEC §11「优先在
//! Rust core 提供 parser」的有意偏离。理由：stdlib helper 运行在隔离 evaluator
//! 进程内、直接读写进程 env；走 native parser 需要把 pwdir.exe 路径传入 evaluator
//! 并为每次 dotenv 调用跨进程回传 KV，而 dotenv 调用低频，复杂度不划算。
//! 若 M6+ fingerprint 需要在 native 侧复用解析结果，回迁 Rust 时必须保持语义一致
//! 并补齐等价 fixture 测试。
