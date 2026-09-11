//! RC content hashes and dependency size/mtime checks. Missing watches are allowed; unreadable metadata must invalidate the fast path.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use pwdir_protocol::{EvaluationResult, FileFingerprint, Fingerprint, WatchDependency};

use crate::evaluator;
use crate::trust;

/// Read size and modification time; any metadata failure returns None.
pub fn file_state(path: &Path) -> Option<FileFingerprint> {
    read_file_state(path).ok()
}

// Preserve error kinds so unreadable paths cannot match a missing-file sentinel.
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

// Missing watches may stay absent; missing sourced scripts must invalidate.
fn missing_sentinel(path: &Path) -> FileFingerprint {
    FileFingerprint {
        path: path.to_path_buf(),
        size: 0,
        modified: None,
    }
}

/// Capture dependency metadata after evaluation, not a content snapshot.
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

/// Check RC/script hashes and metadata. Callers must verify trust first.
pub fn fingerprint_is_current(cached: &Fingerprint, rc: &Path) -> bool {
    let Ok(current_rc_sha) = trust::file_sha256(rc) else {
        return false;
    };
    if current_rc_sha != cached.rc_sha256 {
        return false;
    }

    let (evaluator_version, stdlib_version) = evaluator::embedded_script_versions();
    if evaluator_version != cached.evaluator_version || stdlib_version != cached.stdlib_version {
        return false;
    }

    for expected in &cached.sourced_files {
        match file_state(&expected.path) {
            Some(current)
                if current.size == expected.size && current.modified == expected.modified => {}
            _ => return false,
        }
    }

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
        let explicit = compute_fingerprint(&rc, &result, "ev-1", "std-1");
        assert_eq!(explicit.evaluator_version, "ev-1");
        assert_eq!(explicit.stdlib_version, "std-1");

        assert_eq!(
            fingerprint.rc_sha256,
            trust::file_sha256(&rc).expect("rc hash")
        );
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let ghost = tmp.path().join("future.json");

        let result = sample_result(vec![WatchDependency::File(ghost.clone())], vec![]);
        let fingerprint = compute(&rc, &result);

        assert_eq!(fingerprint.watch_states.len(), 1);
        assert_eq!(fingerprint.watch_states[0].size, 0);
        assert!(fingerprint.watch_states[0].modified.is_none());
        assert!(fingerprint_is_current(&fingerprint, &rc));
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

        fs::remove_file(&sourced).expect("remove sourced");
        assert!(!fingerprint_is_current(&fingerprint, &rc));
    }

    #[test]
    fn fingerprint_invalidated_by_script_version_change() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_file(tmp.path(), RC_FILENAME, "$env:FOO = 'bar'");
        let fingerprint = compute_fingerprint(&rc, &sample_result(vec![], vec![]), "ev-1", "std-1");

        assert!(!fingerprint_is_current(&fingerprint, &rc));

        let fingerprint_current = compute(&rc, &sample_result(vec![], vec![]));
        assert!(fingerprint_is_current(&fingerprint_current, &rc));
    }

    #[test]
    fn fingerprint_invalidated_when_watch_states_missing() {
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
