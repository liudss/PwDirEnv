//! User-local product directories under LOCALAPPDATA; no silent fallback to another trust store.

use std::fs;
use std::path::{Path, PathBuf};

use crate::PwDirError;

const SUBDIRS: [&str; 3] = ["allow", "cache", "state"];

pub fn pwdir_data_root_with(base: Option<&Path>) -> Result<PathBuf, PwDirError> {
    let root = match base {
        Some(explicit) => explicit.to_path_buf(),
        None => match std::env::var_os("LOCALAPPDATA") {
            Some(value) if !value.is_empty() => local_app_data_root(Path::new(&value)),
            _ => match std::env::var_os("USERPROFILE") {
                Some(home) if !home.is_empty() => PathBuf::from(home).join(".pwdir"),
                _ => {
                    return Err(PwDirError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "neither LOCALAPPDATA nor USERPROFILE is set; cannot resolve PwDirEnv data root",
                    )))
                }
            },
        },
    };
    for sub in SUBDIRS {
        fs::create_dir_all(root.join(sub))?;
    }
    Ok(root)
}

fn local_app_data_root(local_app_data: &Path) -> PathBuf {
    local_app_data.join("PwDirEnv")
}

pub fn pwdir_data_root() -> Result<PathBuf, PwDirError> {
    pwdir_data_root_with(None)
}

pub fn allow_dir() -> Result<PathBuf, PwDirError> {
    Ok(pwdir_data_root()?.join("allow"))
}

pub fn cache_dir() -> Result<PathBuf, PwDirError> {
    Ok(pwdir_data_root()?.join("cache"))
}

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
