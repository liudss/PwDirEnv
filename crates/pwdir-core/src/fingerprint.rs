//! Fingerprint 计算与快速路径判变（M6，SPEC §12.2/§12.3）。
//!
//! 两级策略：快速路径只比对 **size + mtime**（`file_state`，廉价），内容级 SHA-256
//! 只用于 RC 本体（trust 语义要求，不能依赖 mtime，ADR-005）。任何状态获取失败
//! （文件消失、权限问题等）一律按「已变化」处理 —— 安全降级为重新评估，
//! 宁可多评估一次，绝不漏掉一次变化（恢复正确性 > 性能）。

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use pwdir_protocol::{EvaluationResult, FileFingerprint, Fingerprint, WatchDependency};

use crate::evaluator;
use crate::trust;

/// 单个文件/目录的快速状态（跟随 symlink/junction 的 `fs::metadata` 语义）。
///
/// metadata 或 mtime 获取失败返回 `None`；调用方把 `None` 视为「变了」，
/// 快速路径据此整体失效、退回完整 evaluation。
pub fn file_state(path: &Path) -> Option<FileFingerprint> {
    read_file_state(path).ok()
}

// 保留错误类型：只有 NotFound 能与缺失哨兵匹配，权限或非法路径错误必须失效。
fn read_file_state(path: &Path) -> io::Result<FileFingerprint> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    let modified: Option<SystemTime> = Some(meta.modified()?);
    Ok(FileFingerprint {
        path: path.to_path_buf(),
        size,
        modified,
    })
}

/// 「watch 目标评估时不存在」的占位状态（stdlib 允许 watch 尚不存在的路径）。
/// 文件真实存在时 mtime 几乎总有值，(size=0, modified=None) 不会与真实状态混淆。
fn missing_sentinel(path: &Path) -> FileFingerprint {
    FileFingerprint {
        path: path.to_path_buf(),
        size: 0,
        modified: None,
    }
}

/// 根据一次成功的 evaluation 构造指纹。
///
/// - `rc_sha256`：RC 当前内容的 SHA-256；RC 在评估后、构造指纹前被删除的竞态
///   下取空串 —— 该指纹在快速路径必然失配（当前 hash 不可能为空串），安全降级。
/// - `watch_states`：与 `result.watches` 按下标对齐；目标不存在时存缺失哨兵。
pub fn compute_fingerprint(
    rc: &Path,
    result: &EvaluationResult,
    evaluator_version: &str,
    stdlib_version: &str,
) -> Fingerprint {
    let rc_sha256 = trust::file_sha256(rc).unwrap_or_default();
    let sourced_files = result
        .sourced_files
        .iter()
        .map(|p| file_state(p).unwrap_or_else(|| missing_sentinel(p)))
        .collect();
    let watch_states = result
        .watches
        .iter()
        .map(|dep| {
            let path = watch_path(dep);
            file_state(path).unwrap_or_else(|| missing_sentinel(path))
        })
        .collect();
    Fingerprint {
        rc_sha256,
        sourced_files,
        watches: result.watches.clone(),
        watch_states,
        evaluator_version: evaluator_version.to_string(),
        stdlib_version: stdlib_version.to_string(),
    }
}

/// 快速路径判变：磁盘 cache 中的指纹是否仍与当前现实一致（SPEC §12.5）。
///
/// 判定顺序：
/// 1. RC 内容 hash 与缓存一致（内容级，trust 同源）；
/// 2. evaluator / stdlib 版本（嵌入脚本内容 hash）一致；
/// 3. sourced 文件与 watch 目标逐项 size+mtime 一致。
///
/// 任一条件不满足或状态无法获取 → false（重新评估）。本函数不做 trust 校验，
/// 调用方必须先完成 allow 检查（安全边界 > 性能）。
pub fn fingerprint_is_current(cached: &Fingerprint, rc: &Path) -> bool {
    // 1. RC 内容（重算 hash 比对，而非 mtime —— 内容变化必须立即失效）
    let Ok(current_rc_sha) = trust::file_sha256(rc) else {
        return false;
    };
    if current_rc_sha != cached.rc_sha256 {
        return false;
    }

    // 2. 嵌入脚本版本：evaluator 行为变化（如 stdlib 修复）后旧 cache 不可信
    let (evaluator_version, stdlib_version) = evaluator::embedded_script_versions();
    if evaluator_version != cached.evaluator_version || stdlib_version != cached.stdlib_version {
        return false;
    }

    // 3a. sourced 文件：评估时点它们存在且被 dot-source；现在缺失或状态变化 → 变了
    //    （严格 None = 变化：sourced 文件缺失会导致重新评估直接失败，不能假装未变）
    for expected in &cached.sourced_files {
        match file_state(&expected.path) {
            Some(current)
                if current.size == expected.size && current.modified == expected.modified => {}
            _ => return false,
        }
    }

    // 3b. watch 目标：与缓存按下标对齐比较。「评估时不存在、检查时仍不存在」
    //     （哨兵对哨兵）视为未变，支持 watch 预创建场景；「存在 → 消失」为变化。
    if cached.watch_states.len() != cached.watches.len() {
        return false;
    }
    for (dep, expected) in cached.watches.iter().zip(&cached.watch_states) {
        let path = watch_path(dep);
        if path != expected.path || !state_matches(expected, path) {
            return false;
        }
    }
    true
}

/// 当前状态是否与缓存记录一致；缺失仅与缺失哨兵相容。
fn state_matches(expected: &FileFingerprint, path: &Path) -> bool {
    match read_file_state(path) {
        Ok(current) => current.size == expected.size && current.modified == expected.modified,
        Err(error) => {
            error.kind() == io::ErrorKind::NotFound
                && expected.size == 0
                && expected.modified.is_none()
        }
    }
}

fn watch_path(dep: &WatchDependency) -> &Path {
    match dep {
        WatchDependency::File(p) | WatchDependency::Directory(p) => p.as_path(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use pwdir_protocol::{EnvKey, EnvironmentSnapshot, RC_FILENAME};

    /// pwsh 免参与的纯结构 EvaluationResult（fingerprint 只依赖元数据）。
    fn sample_result(watches: Vec<WatchDependency>, sourced: Vec<PathBuf>) -> EvaluationResult {
        EvaluationResult {
            environment: EnvironmentSnapshot {
                vars: BTreeMap::from([(EnvKey::from_name("FOO"), OsString::from("bar"))]),
            },
            watches,
            sourced_files: sourced,
            diagnostics: vec![],
        }
    }

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).expect("write fixture");
        path
    }

    /// 用真实嵌入脚本版本计算指纹（fingerprint_is_current 会与当前版本比对）。
    fn compute(rc: &Path, result: &EvaluationResult) -> Fingerprint {
        let (ev, std) = evaluator::embedded_script_versions();
        compute_fingerprint(rc, result, &ev, &std)
    }

    #[test]
    fn file_state_reports_size_and_mtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = write_file(tmp.path(), "a.txt", "hello");

        let state = file_state(&file).expect("state for existing file");
        assert_eq!(state.path, file);
        assert_eq!(state.size, 5);
        assert!(state.modified.is_some(), "existing file has an mtime");
    }

    #[test]
    fn file_state_is_none_for_missing_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ghost = tmp.path().join("does-not-exist.txt");
        assert!(file_state(&ghost).is_none());
    }

    #[test]
    fn invalid_watch_path_never_matches_missing_sentinel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "content");
        let invalid = tmp.path().join("invalid\0path");
        let fingerprint = compute(
            &rc,
            &sample_result(vec![WatchDependency::File(invalid)], vec![]),
        );
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn missing_sourced_file_never_matches_missing_sentinel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "content");
        let fingerprint = compute(
            &rc,
            &sample_result(vec![], vec![tmp.path().join("missing.ps1")]),
        );
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn compute_fingerprint_matches_reality() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let watch = write_file(tmp.path(), "package.json", "{}");
        let sourced = write_file(tmp.path(), "common.ps1", "$env:B = 'c'");

        let result = sample_result(
            vec![WatchDependency::File(watch.clone())],
            vec![sourced.clone()],
        );
        let fingerprint = compute(&rc, &result);
        // 显式传入的版本号被原样记录（结构正确性）
        let explicit = compute_fingerprint(&rc, &result, "ev-1", "std-1");
        assert_eq!(explicit.evaluator_version, "ev-1");
        assert_eq!(explicit.stdlib_version, "std-1");

        assert_eq!(
            fingerprint.rc_sha256,
            trust::file_sha256(&rc).expect("rc hash")
        );
        // compute 走真实嵌入脚本版本；显式传参版本被原样记录（结构正确性）
        let (ev, std) = evaluator::embedded_script_versions();
        assert_eq!(fingerprint.evaluator_version, ev);
        assert_eq!(fingerprint.stdlib_version, std);
        assert_eq!(fingerprint.sourced_files.len(), 1);
        assert_eq!(fingerprint.sourced_files[0].path, sourced);
        assert_eq!(fingerprint.watches.len(), 1);
        assert_eq!(fingerprint.watch_states.len(), 1);
        assert_eq!(fingerprint.watch_states[0].path, watch);
        assert_eq!(
            fingerprint.watch_states[0].size,
            fs::metadata(&watch).expect("meta").len()
        );
    }

    #[test]
    fn compute_fingerprint_sentinel_for_missing_watch_target() {
        // watch 允许指向尚不存在的路径：评估时以缺失哨兵占位
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let ghost = tmp.path().join("future.json");

        let result = sample_result(vec![WatchDependency::File(ghost.clone())], vec![]);
        let fingerprint = compute(&rc, &result);

        assert_eq!(fingerprint.watch_states.len(), 1);
        assert_eq!(fingerprint.watch_states[0].size, 0);
        assert!(fingerprint.watch_states[0].modified.is_none());
        // 仍不存在 → 未变（预创建场景的稳态）
        assert!(fingerprint_is_current(&fingerprint, &rc));
        // 预创建后内容出现 → 变了
        fs::write(&ghost, "created").expect("create watch target");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_is_current_true_when_nothing_changed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let watch = write_file(tmp.path(), "package.json", "{}");

        let result = sample_result(vec![WatchDependency::File(watch)], vec![]);
        let fingerprint = compute(&rc, &result);
        assert!(fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_single_rc_byte_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");

        let fingerprint = compute(&rc, &sample_result(vec![], vec![]));
        assert!(fingerprint_is_current(&fingerprint, &rc));

        fs::write(&rc, "$env:FOO = 'baz'").expect("rewrite rc");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_rc_deletion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "content");
        let fingerprint = compute(&rc, &sample_result(vec![], vec![]));

        fs::remove_file(&rc).expect("remove rc");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_watch_mtime_touch() {
        // Windows mtime 有文件系统粒度：sleep 后原样重写（size 不变），
        // 确保只有 mtime 变化也能被判出
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let watch = write_file(tmp.path(), "package.json", "{}");

        let result = sample_result(vec![WatchDependency::File(watch.clone())], vec![]);
        let fingerprint = compute(&rc, &result);
        assert!(fingerprint_is_current(&fingerprint, &rc));

        thread::sleep(Duration::from_millis(60));
        let contents = fs::read_to_string(&watch).expect("read watch");
        fs::write(&watch, contents).expect("touch watch (same bytes, new mtime)");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_sourced_file_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "source_env 'common.ps1'");
        let sourced = write_file(tmp.path(), "common.ps1", "$env:B = 'c'");

        let result = sample_result(vec![], vec![sourced.clone()]);
        let fingerprint = compute(&rc, &result);
        assert!(fingerprint_is_current(&fingerprint, &rc));

        fs::write(&sourced, "$env:B = 'changed'").expect("change sourced");
        assert!(!fingerprint_is_current(&fingerprint, &rc));

        // sourced 文件被删除：严格判为变化（重新评估会因 source 失败而报错）
        fs::remove_file(&sourced).expect("remove sourced");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_script_version_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let fingerprint = compute_fingerprint(&rc, &sample_result(vec![], vec![]), "ev-1", "std-1");

        // 模拟嵌入脚本内容变化后的版本号：旧 cache 必须失效
        assert!(!fingerprint_is_current(&fingerprint, &rc));

        let fingerprint_current = compute(&rc, &sample_result(vec![], vec![]));
        assert!(fingerprint_is_current(&fingerprint_current, &rc));
    }

    #[test]
    fn fingerprint_invalidated_when_watch_states_missing() {
        // 防御：watch_states 与 watches 数量不一致（旧载荷/损坏）→ 视为已变化
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "content");
        let mut fingerprint = compute(
            &rc,
            &sample_result(vec![WatchDependency::File(rc.clone())], vec![]),
        );
        fingerprint.watch_states.clear();
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }
}
