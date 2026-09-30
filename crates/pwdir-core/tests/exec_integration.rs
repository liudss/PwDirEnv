use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use pwdir_core::evaluator::EvaluatorConfig;
use pwdir_core::exec::{run_exec_with_store, ExecOutcome};
use pwdir_core::trust::TrustStore;
use pwdir_core::PwDirError;
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
        "pwsh (PowerShell 7+) was not found on PATH; it is required for exec tests"
    );
}

fn temp_store() -> (TrustStore, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::new(tmp.path().join("allow"));
    (store, tmp)
}

fn write_rc(dir: &Path, body: &str) -> PathBuf {
    let rc = dir.join(RC_FILENAME);
    fs::write(&rc, body).expect("write rc fixture");
    rc
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

#[test]
fn exec_injects_rc_environment_and_runs_command() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "$env:PW_EXEC = '42'");
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");

    let outcome = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&[
            "-NoProfile",
            "-Command",
            "if ($env:PW_EXEC -ne '42') { exit 5 }; Write-Output 'ok'",
        ]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect("exec ok");

    assert_eq!(outcome.exit_code, Some(0), "rc environment must be applied");
    assert!(
        outcome.stdout.contains("ok"),
        "stdout must carry the child output: {:?}",
        outcome.stdout
    );
}

#[test]
fn exec_propagates_child_exit_code() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "$env:PW_EXEC_EXIT = 'set'");
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");

    let outcome = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&["-NoProfile", "-Command", "exit 7"]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect("exec ok");

    assert_eq!(outcome.exit_code, Some(7), "child exit code must propagate");
}

#[test]
fn exec_works_in_dir_with_spaces() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let spaced = tmp.path().join("dir with spaces");
    fs::create_dir(&spaced).expect("mkdir spaced");
    let rc = write_rc(&spaced, "$env:PW_EXEC_SPACE = 'spa ced'");
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");

    let outcome = run_exec_with_store(
        &spaced,
        "pwsh",
        &strings(&[
            "-NoProfile",
            "-Command",
            "Write-Output \"$env:PW_EXEC_SPACE|$PWD\"",
        ]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect("exec ok");

    assert_eq!(outcome.exit_code, Some(0));
    assert!(
        outcome.stdout.contains("spa ced"),
        "rc env must be applied in spaced dir: {:?}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("dir with spaces"),
        "child must run inside the spaced dir: {:?}",
        outcome.stdout
    );
}

#[test]
fn exec_without_rc_runs_with_inherited_environment() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, _guard) = temp_store();

    let outcome = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&["-NoProfile", "-Command", "Write-Output 'plain'"]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect("exec ok");

    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.stdout.contains("plain"));
}

#[test]
fn exec_unsets_variables_removed_by_rc() {
    ensure_pwsh();
    std::env::set_var("PWDIR_EXEC_REMOVE_ME", "gone-soon");
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "Remove-Item Env:PWDIR_EXEC_REMOVE_ME -ErrorAction Ignore",
    );
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");

    let outcome = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&[
            "-NoProfile",
            "-Command",
            "if ($env:PWDIR_EXEC_REMOVE_ME) { exit 9 } else { Write-Output 'unset-ok' }",
        ]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect("exec ok");

    assert_eq!(
        outcome.exit_code,
        Some(0),
        "variable must be unset: {:?}",
        outcome.stderr
    );
    assert!(outcome.stdout.contains("unset-ok"));
    std::env::remove_var("PWDIR_EXEC_REMOVE_ME");
}

#[test]
fn exec_require_rc_without_rc_is_rc_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, _guard) = temp_store();

    let err = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&["-NoProfile", "-Command", "Write-Output 'never'"]),
        true,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect_err("require_rc must fail without rc");

    assert!(matches!(err, PwDirError::RcNotFound(_)));
}

#[test]
fn exec_blocked_rc_is_not_allowed_and_command_never_runs() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let _rc = write_rc(tmp.path(), "$env:PW_EXEC_BLOCKED = '1'");
    let (store, _guard) = temp_store();

    let marker = tmp.path().join("child-ran.marker");
    let err = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&[
            "-NoProfile",
            "-Command",
            &format!(
                "Set-Content -LiteralPath '{}' -Value 'ran'",
                marker.display()
            ),
        ]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect_err("blocked rc must fail");

    assert!(matches!(err, PwDirError::NotAllowed(_)));
    assert!(
        !marker.exists(),
        "child command must never run while the rc is not allowed"
    );
}

#[test]
fn exec_throwing_rc_is_evaluation_failed() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "throw 'boom-exec'");
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");

    let err = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&["-NoProfile", "-Command", "Write-Output 'never'"]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect_err("throwing rc must fail");

    assert!(matches!(err, PwDirError::EvaluationFailed(_)));
}

#[test]
fn exec_does_not_spawn_command_when_rc_changes_during_evaluation() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc_path = tmp.path().join(RC_FILENAME);
    let rc = write_rc(
        tmp.path(),
        &format!(
            "$env:PW_EXEC_RACE = 'old'\nSet-Content -LiteralPath '{}' -Value \"`$env:PW_EXEC_RACE = 'new'\"\n",
            rc_path.display()
        ),
    );
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow rc");
    let marker = tmp.path().join("command-ran.marker");

    let err = run_exec_with_store(
        tmp.path(),
        "pwsh",
        &strings(&[
            "-NoProfile",
            "-Command",
            &format!("Set-Content -LiteralPath '{}' -Value ran", marker.display()),
        ]),
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect_err("changed RC must block exec");

    assert!(matches!(err, PwDirError::NotAllowed(_)));
    assert!(
        !marker.exists(),
        "command must not spawn with a stale RC patch"
    );
}

#[test]
fn exec_missing_command_is_friendly_io_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (store, _guard) = temp_store();

    let err = run_exec_with_store(
        tmp.path(),
        "definitely-not-a-real-command-xyz",
        &[],
        false,
        &EvaluatorConfig::default(),
        &store,
    )
    .expect_err("missing command must fail");

    match err {
        PwDirError::Io(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("not found") && msg.contains("absolute path"),
                "friendly hint expected: {msg}"
            );
        }
        other => panic!("expected PwDirError::Io, got: {other:?}"),
    }
}

#[test]
fn exec_outcome_default_is_empty() {
    let outcome = ExecOutcome::default();
    assert_eq!(outcome.exit_code, None);
    assert!(outcome.stdout.is_empty());
    assert!(outcome.stderr.is_empty());
}

#[test]
fn exec_resolves_relative_project_and_parent_rc() {
    ensure_pwsh();
    let cwd = std::env::current_dir().expect("cwd");
    let tmp = tempfile::tempdir_in(&cwd).expect("local fixture");
    let project = tmp.path().join("project");
    let child = project.join("child");
    fs::create_dir_all(&child).expect("mkdir child");
    let rc = write_rc(&project, "$env:PW_EXEC_RELATIVE = 'resolved'");
    let (store, _guard) = temp_store();
    store.allow(&rc).expect("allow fixture");
    for dir in [&project, &child] {
        let relative = Path::new(".").join(dir.strip_prefix(&cwd).expect("relative path"));
        let outcome = run_exec_with_store(
            &relative,
            "pwsh",
            &strings(&[
                "-NoProfile",
                "-Command",
                "Write-Output $env:PW_EXEC_RELATIVE; (Get-Location).ProviderPath; exit 42",
            ]),
            true,
            &EvaluatorConfig::default(),
            &store,
        )
        .expect("relative exec succeeds");
        assert_eq!(outcome.exit_code, Some(42));
        let lines: Vec<_> = outcome.stdout.lines().collect();
        assert_eq!(lines[0], "resolved");
        assert_eq!(Path::new(lines[1]), dir);
    }
}
