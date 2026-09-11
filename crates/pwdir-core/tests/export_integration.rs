use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use pwdir_core::cache::CacheStore;
use pwdir_core::evaluator::EvaluatorConfig;
use pwdir_core::export::export_with_store;
use pwdir_core::trust::TrustStore;
use pwdir_protocol::{Action, RC_FILENAME};

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
        "pwsh (PowerShell 7+) was not found on PATH; it is required for export integration tests"
    );
}

fn config() -> EvaluatorConfig {
    EvaluatorConfig {
        timeout: Duration::from_secs(15),
        ..EvaluatorConfig::default()
    }
}

fn temp_stores() -> (TrustStore, CacheStore, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::new(tmp.path().join("allow"));
    let cache = CacheStore::new(tmp.path().join("cache"));
    (store, cache, tmp)
}

fn write_and_allow(store: &TrustStore, dir: &Path, body: &str) -> std::path::PathBuf {
    let rc = dir.join(RC_FILENAME);
    fs::write(&rc, body).expect("write rc fixture");
    store.allow(&rc).expect("allow rc");
    rc
}

#[test]
fn set_and_unset_in_one_rc_produce_respective_patch_sides() {
    ensure_pwsh();
    const TO_REMOVE: &str = "PWDIR_TEST_UNSET_UNIQUE_XYZ";
    std::env::set_var(TO_REMOVE, "1");

    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, cache, _guard) = temp_stores();
    let _rc = write_and_allow(
        &store,
        tmp.path(),
        &format!(
            "$env:PWDIR_TEST_SET_FOO = 'bar'\nRemove-Item Env:{TO_REMOVE} -ErrorAction Ignore"
        ),
    );

    let out = export_with_store(tmp.path(), &config(), &store, &cache, false)
        .expect("export must not return Err");

    std::env::remove_var(TO_REMOVE);

    assert_eq!(out.action, Action::Apply);
    assert_eq!(
        out.patch.set.get("PWDIR_TEST_SET_FOO").map(String::as_str),
        Some("bar"),
        "set side must contain the new variable"
    );
    assert!(
        !out.patch.set.contains_key(TO_REMOVE),
        "removed variable must not appear on the set side"
    );
    assert!(
        out.patch.unset.contains(TO_REMOVE),
        "unset side must contain the removed variable: {:?}",
        out.patch.unset
    );
    assert!(out.managed_keys.iter().any(|k| k == "PWDIR_TEST_SET_FOO"));
    assert!(out.managed_keys.iter().any(|k| k == TO_REMOVE));
    assert_eq!(
        out.managed_keys.len(),
        out.patch.set.len() + out.patch.unset.len()
    );
}

#[test]
fn execution_policy_artifact_is_not_reported_as_new_var() {
    ensure_pwsh();
    std::env::remove_var("FOO");
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, cache, _guard) = temp_stores();
    let _rc = write_and_allow(&store, tmp.path(), "$env:FOO = 'bar'");

    let out = export_with_store(tmp.path(), &config(), &store, &cache, false)
        .expect("export must not return Err");
    std::env::remove_var("FOO");

    assert_eq!(out.action, Action::Apply);
    assert!(
        !out.patch
            .set
            .keys()
            .any(|k| k.eq_ignore_ascii_case("PSEXECUTIONPOLICYPREFERENCE")),
        "PSExecutionPolicyPreference artifact must not appear on the set side: {:?}",
        out.patch.set
    );
    assert!(
        !out.managed_keys
            .iter()
            .any(|k| k.eq_ignore_ascii_case("PSEXECUTIONPOLICYPREFERENCE")),
        "managed_keys must not contain the policy artifact: {:?}",
        out.managed_keys
    );
    assert_eq!(
        out.patch.set.get("FOO").map(String::as_str),
        Some("bar"),
        "FOO must still be reported normally"
    );
}

#[test]
fn unicode_value_survives_export_round_trip() {
    ensure_pwsh();
    std::env::remove_var("PWDIR_TEST_UNICODE_VAR");
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, cache, _guard) = temp_stores();
    let _rc = write_and_allow(
        &store,
        tmp.path(),
        "$env:PWDIR_TEST_UNICODE_VAR = '你好 🎉'",
    );

    let out = export_with_store(tmp.path(), &config(), &store, &cache, false)
        .expect("export must not return Err");
    std::env::remove_var("PWDIR_TEST_UNICODE_VAR");

    assert_eq!(out.action, Action::Apply);
    assert_eq!(
        out.patch
            .set
            .get("PWDIR_TEST_UNICODE_VAR")
            .map(String::as_str),
        Some("你好 🎉"),
        "unicode value must round-trip through the evaluator and diff intact"
    );
}
