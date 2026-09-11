//! Native discovery, trust, environment diff, cache, evaluation, and execution. See docs/architecture.md.

pub mod cache;
pub mod diff;
pub mod dirs;
pub mod discovery;
pub mod doctor;
pub mod evaluator;
pub mod exec;
pub mod export;
pub mod fingerprint;
pub mod pathutil;
pub mod trust;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PwDirError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
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
