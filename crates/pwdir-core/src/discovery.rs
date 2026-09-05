//! `.envrc.ps1` 向上发现（M1，SPEC §5.2）。
//!
//! 搜索语义：从 `start` 开始逐级向上，只返回**最近**的一份 RC（ADR-004，
//! 不隐式叠加父级配置）。终止条件：`Path::parent()` 返回 `None` 或与自身
//! 相等（Windows 盘符根 / UNC 根），避免在根上死循环。全程纯路径操作 +
//! `is_file()` 探测：当前目录被删除、`start` 不存在时都不 panic，
//! `is_file` 返回 false 后继续向上（SPEC §5.4/§20.4）。

use std::path::{Path, PathBuf};

use pwdir_protocol::{DiscoveredRc, RC_FILENAME};

use crate::PwDirError;

/// 从 `start` 开始向上查找最近的 `.envrc.ps1`。
///
/// `Err` 分支为未来扩展预留（当前实现是纯路径探测，不会产生 IO 失败）；
/// 无 RC 时返回 `Ok(None)`。
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
            // `parent != dir` 防御：若某平台/路径形式下 parent() 返回自身则直接终止
            Some(parent) if parent != dir => dir = parent,
            _ => return Ok(None),
        }
    }
}

/// 当前进程工作目录；获取失败（如目录已被删除）返回 `None` 而非 panic
/// （SPEC §5.4/§20.4）。CLI 据此走"找不到 RC"路径而不是崩溃。
pub fn current_dir_fs_or_none() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 在指定目录写入一份 RC fixture，返回 rc 路径。
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
        // 模拟盘符根边界：取 tempdir 所在盘的根（如 C:\）。
        // 正常环境该处没有 .envrc.ps1 → None；关键是终止且不 panic、不死循环。
        let tmp = std::env::temp_dir();
        let root = tmp
            .ancestors()
            .last()
            .expect("absolute path always has a root")
            .to_path_buf();
        let found = find_rc(&root).expect("find_rc ok");
        if let Some(rc) = found {
            // 极小概率环境下盘根真有 RC；此时只需保证结果自洽
            assert_eq!(rc.rc_path, root.join(RC_FILENAME));
        }
    }

    #[test]
    fn nonexistent_start_does_not_panic() {
        // 不存在的 start：is_file 全为 false，逐级向上至根返回 None
        let tmp = tempfile::tempdir().expect("tempdir");
        let ghost = tmp.path().join("does").join("not").join("exist");
        assert!(find_rc(&ghost).expect("find_rc ok").is_none());
    }

    #[test]
    fn nearest_rc_wins_when_nested() {
        // 有 rc 但目录更深一层：选择更近（更深）的那份
        let tmp = tempfile::tempdir().expect("tempdir");
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        fs::create_dir_all(&inner).expect("mkdirs");
        write_rc(&outer, "outer");
        let inner_rc = write_rc(&inner, "inner");

        let found = find_rc(&inner).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found.rc_path, inner_rc);
        assert_eq!(found.project_root, inner);

        // 从 outer 自身出发则命中 outer 的 rc
        let found_outer = find_rc(&outer).expect("find_rc ok").expect("rc discovered");
        assert_eq!(found_outer.project_root, outer);
    }

    #[test]
    fn current_dir_fs_or_none_returns_some_normally() {
        assert!(current_dir_fs_or_none().is_some());
    }
}
