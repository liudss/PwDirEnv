//! 数据目录解析（M1，SPEC §6.3）：`%LOCALAPPDATA%\PwDirEnv\{allow,cache,state}`。
//!
//! 设计为**可注入**（`*_with(base)`）以便测试：单测传临时目录，完全不动全局
//! 环境变量；对外便捷函数内部调用注入版本（base = None 时从环境解析）。

use std::fs;
use std::path::{Path, PathBuf};

use crate::PwDirError;

/// 返回数据根前必须就位的子目录（SPEC §6.3）。
const SUBDIRS: [&str; 3] = ["allow", "cache", "state"];

/// 解析 PwDirEnv 数据根目录并确保 allow/cache/state 子目录存在。
///
/// - `base` 给定（测试注入）时直接使用，不读环境变量；
/// - 否则优先 `%LOCALAPPDATA%`；缺失或为空时回退 `%USERPROFILE%\.pwdir`
///   （SPEC §6.3 "回退到合理用户目录"，`doctor` 将在 M7 报告该回退状态）。
pub fn pwdir_data_root_with(base: Option<&Path>) -> Result<PathBuf, PwDirError> {
    let root = match base {
        Some(explicit) => explicit.to_path_buf(),
        None => match std::env::var_os("LOCALAPPDATA") {
            // LOCALAPPDATA 是共享父目录；所有 PwDirEnv 状态必须收敛在自身
            // 子目录中，不能在其根下创建 allow/cache/state。
            Some(value) if !value.is_empty() => local_app_data_root(Path::new(&value)),
            _ => match std::env::var_os("USERPROFILE") {
                Some(home) if !home.is_empty() => PathBuf::from(home).join(".pwdir"),
                // 两个变量都缺失/为空：无法定位合理的用户目录，显式报错
                _ => {
                    return Err(PwDirError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "neither LOCALAPPDATA nor USERPROFILE is set; cannot resolve PwDirEnv data root",
                    )))
                }
            },
        },
    };
    // 返回前创建子目录：调用方（trust store / cache）无需各自处理"目录不存在"
    for sub in SUBDIRS {
        fs::create_dir_all(root.join(sub))?;
    }
    Ok(root)
}

fn local_app_data_root(local_app_data: &Path) -> PathBuf {
    local_app_data.join("PwDirEnv")
}

/// 生产入口：从环境解析数据根目录。
pub fn pwdir_data_root() -> Result<PathBuf, PwDirError> {
    pwdir_data_root_with(None)
}

/// trust record 目录（`...\PwDirEnv\allow`）。
pub fn allow_dir() -> Result<PathBuf, PwDirError> {
    Ok(pwdir_data_root()?.join("allow"))
}

/// fingerprint / watch 元数据缓存目录（M6 使用）。
pub fn cache_dir() -> Result<PathBuf, PwDirError> {
    Ok(pwdir_data_root()?.join("cache"))
}

/// 会话无关状态目录（M6+ 使用）。
pub fn state_dir() -> Result<PathBuf, PwDirError> {
    Ok(pwdir_data_root()?.join("state"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_base_creates_allow_cache_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = pwdir_data_root_with(Some(tmp.path())).expect("data root");

        assert_eq!(root, tmp.path());
        assert!(tmp.path().join("allow").is_dir());
        assert!(tmp.path().join("cache").is_dir());
        assert!(tmp.path().join("state").is_dir());
    }

    #[test]
    fn injected_base_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = pwdir_data_root_with(Some(tmp.path())).expect("first");
        let second = pwdir_data_root_with(Some(tmp.path())).expect("second");
        assert_eq!(first, second);
    }

    #[test]
    fn local_app_data_uses_product_subdirectory() {
        assert_eq!(
            local_app_data_root(Path::new(r"C:\Users\alice\AppData\Local")),
            PathBuf::from(r"C:\Users\alice\AppData\Local\PwDirEnv")
        );
    }
}
