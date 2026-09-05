//! pwdir-core：PwDirEnv native 核心逻辑。
//!
//! 模块边界（PLAN §1.1）：本 crate 负责发现 / trust / diff / export 编排 / evaluator 启动 /
//! dotenv 解析，不解析 CLI 参数，不直接面向用户输出。

pub mod cache;
pub mod diff;
pub mod dirs;
pub mod discovery;
pub mod dotenv;
pub mod evaluator;
pub mod export;
pub mod fingerprint;
pub mod pathutil;
pub mod trust;

use thiserror::Error;

/// 统一错误类型。变体对应 SPEC §14.1 的退出码映射（RcNotFound → 2，Io → 3）。
#[derive(Debug, Error)]
pub enum PwDirError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// RC 未找到。payload 是面向用户的完整句子（如 SPEC §14.1 要求的精确提示），
    /// CLI 原样输出到 stderr，因此这里不再叠加 "rc file not found:" 前缀。
    #[error("{0}")]
    RcNotFound(String),
    #[error("rc file is not allowed: {0}")]
    NotAllowed(String),
    #[error("evaluation failed: {0}")]
    EvaluationFailed(String),
    #[error("evaluator timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("not implemented: {0}")]
    NotImplemented(String),
}
