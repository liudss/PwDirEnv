//! Discover, authorize, check cache, and export a structured environment patch. Cache hits require host session state; fresh sessions may need force.

use std::collections::BTreeSet;
use std::path::Path;

use jiff::Timestamp;
use pwdir_protocol::{Action, Diagnostic, EnvironmentSnapshot, ExportOutput, PROTOCOL_SCHEMA};

use crate::cache::{CacheEntry, CacheStore, CACHE_SCHEMA};
use crate::diff::diff_env;
use crate::discovery;
use crate::evaluator::{self, EvaluatorConfig};
use crate::fingerprint::{compute_fingerprint, fingerprint_is_current};
use crate::trust::TrustStore;
use crate::PwDirError;

pub fn export(cwd: &Path, cfg: &EvaluatorConfig, force: bool) -> Result<ExportOutput, PwDirError> {
    let store = TrustStore::open()?;
    let cache = CacheStore::open()?;
    export_with_store(cwd, cfg, &store, &cache, force)
}

/// Export using injectable stores. Force bypasses shared cache, never authorization.
pub fn export_with_store(
    cwd: &Path,
    cfg: &EvaluatorConfig,
    store: &TrustStore,
    cache: &CacheStore,
    force: bool,
) -> Result<ExportOutput, PwDirError> {
    if cwd.as_os_str().is_empty() {
        return Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Noop,
            rc: None,
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "warn".to_string(),
                message: "current directory is unavailable; treated as no rc".to_string(),
            }],
        });
    }

    let Some(rc) = discovery::find_rc(cwd)? else {
        return Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Noop,
            rc: None,
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![],
        });
    };

    if !store.is_allowed(&rc.rc_path)? {
        return Ok(blocked_output(&rc.rc_path));
    }

    if !force {
        if let Some(entry) = cache.load(&rc.rc_path)? {
            if fingerprint_is_current(&entry.fingerprint, &rc.rc_path) {
                return Ok(ExportOutput {
                    schema: PROTOCOL_SCHEMA,
                    action: Action::Noop,
                    rc: Some(rc.rc_path),
                    patch: Default::default(),
                    managed_keys: vec![],
                    fingerprint: Some(entry.fingerprint),
                    diagnostics: vec![Diagnostic {
                        level: "info".to_string(),
                        message: "fingerprint unchanged (fast path)".to_string(),
                    }],
                });
            }
        }
    }

    // Cache hits, blocked RCs, and absent RCs do not need an environment snapshot.
    let baseline = EnvironmentSnapshot::from_process();
    let versions = evaluator::embedded_script_versions();
    let report = evaluator::evaluate(cfg, &rc)?;

    if !store.is_allowed(&rc.rc_path)? {
        return Ok(blocked_output(&rc.rc_path));
    }

    match (report.result, report.error) {
        (Some(result), _) => {
            let fingerprint = compute_fingerprint(&rc.rc_path, &result, &versions.0, &versions.1);
            cache.save(&CacheEntry {
                schema: CACHE_SCHEMA,
                rc: rc.rc_path.clone(),
                fingerprint: fingerprint.clone(),
                evaluated_at: Timestamp::now().to_string(),
            })?;

            let desired = result.environment;
            let patch = diff_env(&baseline, &desired);
            let mut managed: BTreeSet<String> = patch.set.keys().cloned().collect();
            managed.extend(patch.unset.iter().cloned());
            let action = if patch.is_empty() {
                Action::Noop
            } else {
                Action::Apply
            };
            Ok(ExportOutput {
                schema: PROTOCOL_SCHEMA,
                action,
                rc: Some(rc.rc_path),
                patch,
                managed_keys: managed.into_iter().collect(),
                fingerprint: Some(fingerprint),
                diagnostics: vec![],
            })
        }
        (None, Some(error)) => Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Error,
            rc: Some(rc.rc_path),
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "error".to_string(),
                message: error.message,
            }],
        }),
        (None, None) => Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Error,
            rc: Some(rc.rc_path),
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "error".to_string(),
                message: "evaluator produced neither a result nor an error".to_string(),
            }],
        }),
    }
}

fn blocked_output(rc_path: &Path) -> ExportOutput {
    ExportOutput {
        schema: PROTOCOL_SCHEMA,
        action: Action::Blocked,
        rc: Some(rc_path.to_path_buf()),
        patch: Default::default(),
        managed_keys: vec![],
        fingerprint: None,
        diagnostics: vec![Diagnostic {
            level: "warn".to_string(),
            message: "rc file is blocked; run `pwdir allow`".to_string(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::OnceLock;

    use pwdir_protocol::RC_FILENAME;

    static PWSH_AVAILABLE: OnceLock<bool> = OnceLock::new();

    fn ensure_pwsh() {
        let ok = *PWSH_AVAILABLE.get_or_init(|| {
            Command::new("pwsh")
                .arg("-NoProfile")
                .arg("-Command")
                .arg("$PSVersionTable.PSVersion")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        });
        assert!(
            ok,
            "pwsh (PowerShell 7+) was not found on PATH; it is required for export tests"
        );
    }

    fn temp_stores() -> (TrustStore, CacheStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::new(tmp.path().join("allow"));
        let cache = CacheStore::new(tmp.path().join("cache"));
        (store, cache, tmp)
    }

    fn write_rc(dir: &Path, body: &str) -> PathBuf {
        let rc = dir.join(RC_FILENAME);
        fs::write(&rc, body).expect("write rc fixture");
        rc
    }

    #[test]
    fn export_applies_allowed_rc_and_caches_fingerprint() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_FOO");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.schema, PROTOCOL_SCHEMA);
        assert_eq!(out.action, Action::Apply);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert_eq!(
            out.patch.set.get("PWDIR_M6_FOO").map(String::as_str),
            Some("bar")
        );
        assert!(out.patch.unset.is_empty());
        assert!(
            out.managed_keys.iter().any(|k| k == "PWDIR_M6_FOO"),
            "managed_keys must contain PWDIR_M6_FOO: {:?}",
            out.managed_keys
        );
        let fingerprint = out
            .fingerprint
            .expect("fingerprint present after evaluation");
        assert_eq!(
            fingerprint.rc_sha256,
            crate::trust::file_sha256(&rc).expect("rc hash")
        );
        let entry = cache.load(&rc).expect("cache load").expect("cache entry");
        assert!(fingerprint_is_current(&entry.fingerprint, &rc));
        std::env::remove_var("PWDIR_M6_FOO");
    }

    #[test]
    fn no_rc_yields_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, cache, _guard) = temp_stores();

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Noop);
        assert!(out.rc.is_none());
        assert!(out.patch.is_empty());
        assert!(out.managed_keys.is_empty());
        assert!(out.fingerprint.is_none());
    }

    #[test]
    fn unavailable_cwd_yields_noop_with_diagnostic() {
        let (store, cache, _guard) = temp_stores();
        let out = export_with_store(
            Path::new(""),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Noop);
        assert!(out.rc.is_none());
        assert!(out.patch.is_empty());
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.message.contains("current directory is unavailable")),
            "expected an unavailable-cwd diagnostic: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn unallowed_rc_is_blocked_with_empty_patch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert!(out.patch.is_empty(), "blocked must never carry a patch");
        assert!(out.managed_keys.is_empty());
        assert!(
            out.fingerprint.is_none(),
            "blocked must not carry a fingerprint"
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.level == "warn" && d.message.contains("pwdir allow")),
            "expected a blocked warning diagnostic: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn rc_byte_change_after_allow_is_blocked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        fs::write(&rc, "$env:PWDIR_M6_FOO = 'baz'").expect("rewrite rc");
        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert!(out.patch.is_empty());
    }

    #[test]
    fn rc_changed_during_evaluation_is_blocked_without_caching_stale_result() {
        ensure_pwsh();
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc_path = tmp.path().join(RC_FILENAME);
        let rc = write_rc(
            tmp.path(),
            &format!(
                "$env:PWDIR_RACE_GUARD = 'old'\nSet-Content -LiteralPath '{}' -Value \"`$env:PWDIR_RACE_GUARD = 'new'\"\n",
                rc_path.display()
            ),
        );
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            true,
        )
        .expect("export returns protocol output");

        assert_eq!(out.action, Action::Blocked);
        assert!(
            out.patch.is_empty(),
            "stale evaluator patch must not escape"
        );
        assert!(out.fingerprint.is_none());
        assert!(cache.load(&rc).expect("cache load").is_none());
        std::env::remove_var("PWDIR_RACE_GUARD");
    }

    #[test]
    fn throwing_rc_yields_error_action_without_cache() {
        ensure_pwsh();
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "throw 'boom-export'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Error);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert!(out.patch.is_empty(), "error must never carry a patch");
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.level == "error" && d.message.contains("boom-export")),
            "expected the evaluation error as diagnostic: {:?}",
            out.diagnostics
        );
        assert!(cache.load(&rc).expect("cache load").is_none());
    }

    #[test]
    fn fast_path_returns_noop_without_reevaluating() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_FAST");
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = tmp.path().join("marker.txt");
        let rc = write_rc(
            tmp.path(),
            &format!(
                "Set-Content -LiteralPath '{}' -Value ([DateTime]::UtcNow.Ticks)\n$env:PWDIR_M6_FAST = 'on'\n",
                marker.display()
            ),
        );
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let first = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert_eq!(first.action, Action::Apply);
        let ticks_after_first = fs::read_to_string(&marker).expect("marker after export 1");

        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(second.action, Action::Noop);
        assert_eq!(second.rc.as_deref(), Some(rc.as_path()));
        assert!(second.patch.is_empty(), "fast path must not carry a patch");
        assert!(second.fingerprint.is_some());
        assert!(
            second
                .diagnostics
                .iter()
                .any(|d| d.level == "info" && d.message == "fingerprint unchanged (fast path)"),
            "expected fast path diagnostic: {:?}",
            second.diagnostics
        );
        assert_eq!(
            fs::read_to_string(&marker).expect("marker after export 2"),
            ticks_after_first,
            "evaluator must not have re-run on the fast path"
        );

        let forced = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            true,
        )
        .expect("export 3 (forced)");
        assert_eq!(forced.action, Action::Apply);
        assert_ne!(
            fs::read_to_string(&marker).expect("marker after forced export"),
            ticks_after_first,
            "force must re-evaluate even when the fingerprint is unchanged"
        );
        std::env::remove_var("PWDIR_M6_FAST");
    }

    #[test]
    fn stale_fingerprint_falls_back_to_evaluation() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_STALE");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_STALE = 'v1'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let first = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert_eq!(first.action, Action::Apply);

        let mut entry = cache.load(&rc).expect("load").expect("entry");
        entry.fingerprint.rc_sha256 = "stale".repeat(8);
        cache.save(&entry).expect("save stale entry");

        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(
            second.action,
            Action::Apply,
            "stale fingerprint must re-evaluate"
        );
        assert!(
            second
                .diagnostics
                .iter()
                .all(|d| d.message != "fingerprint unchanged (fast path)"),
            "stale cache must not report the fast path: {:?}",
            second.diagnostics
        );
        std::env::remove_var("PWDIR_M6_STALE");
    }

    #[test]
    fn invalidated_cache_reevaluates_without_force() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_RELOAD");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_RELOAD = 'x'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert!(cache.load(&rc).expect("load").is_some());

        cache.invalidate(&rc).expect("invalidate");
        assert!(cache.load(&rc).expect("load").is_none());
        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(second.action, Action::Apply);
        assert!(cache.load(&rc).expect("load").is_some(), "cache rebuilt");
        std::env::remove_var("PWDIR_M6_RELOAD");
    }
}
