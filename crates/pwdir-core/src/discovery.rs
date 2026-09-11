//! Find only the nearest .envrc.ps1 while walking toward the filesystem root.

use std::path::{Path, PathBuf};

use pwdir_protocol::{DiscoveredRc, RC_FILENAME};

use crate::PwDirError;

pub fn find_rc(start: &Path) -> Result<Option<DiscoveredRc>, PwDirError> {
    let mut dir = start;
    loop {
        let candidate = dir.join(RC_FILENAME);
        if candidate.is_file() {
            return Ok(Some(DiscoveredRc {
                project_root: dir.to_path_buf(),
                rc_path: candidate,
            }));
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return Ok(None),
        }
    }
}

pub fn current_dir_fs_or_none() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_rc(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join(RC_FILENAME);
        fs::write(&path, contents).expect("write rc fixture");
        path
    }

    #[test]
    fn finds_rc_in_start_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_rc(tmp.path(), "$env:A = '1'");

        let found = find_rc(tmp.path())
            .expect("find_rc ok")
            .expect("rc discovered");
        assert_eq!(found.rc_path, tmp.path().join(RC_FILENAME));
        assert_eq!(found.project_root, tmp.path());
    }

    #[test]
    fn finds_rc_in_parent_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let child = tmp.path().join("child");
        fs::create_dir(&child).expect("mkdir child");
        write_rc(tmp.path(), "$env:A = '1'");

        let found = find_rc(&child).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found.rc_path, tmp.path().join(RC_FILENAME));
        assert_eq!(found.project_root, tmp.path());
    }

    #[test]
    fn finds_rc_multiple_levels_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let deep = tmp.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).expect("mkdirs deep");
        write_rc(tmp.path(), "$env:A = '1'");

        let found = find_rc(&deep).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found.project_root, tmp.path());
    }

    #[test]
    fn returns_none_when_no_rc_anywhere() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("x").join("y");
        fs::create_dir_all(&nested).expect("mkdirs nested");

        assert!(find_rc(&nested).expect("find_rc ok").is_none());
    }

    #[test]
    fn terminates_at_drive_root_without_loop() {
        let tmp = std::env::temp_dir();
        let root = tmp
            .ancestors()
            .last()
            .expect("absolute path always has a root")
            .to_path_buf();
        let found = find_rc(&root).expect("find_rc ok");
        if let Some(rc) = found {
            assert_eq!(rc.rc_path, root.join(RC_FILENAME));
        }
    }

    #[test]
    fn nonexistent_start_does_not_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ghost = tmp.path().join("does").join("not").join("exist");
        assert!(find_rc(&ghost).expect("find_rc ok").is_none());
    }

    #[test]
    fn nearest_rc_wins_when_nested() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).expect("mkdirs");
        write_rc(&outer, "outer");
        let inner_rc = write_rc(&inner, "inner");

        let found = find_rc(&inner).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found.rc_path, inner_rc);
        assert_eq!(found.project_root, inner);

        let found_outer = find_rc(&outer).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found_outer.project_root, outer);
    }

    #[test]
    fn current_dir_fs_or_none_returns_some_normally() {
        assert!(current_dir_fs_or_none().is_some());
    }
}
