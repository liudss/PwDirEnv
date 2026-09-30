use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use pwdir_core::evaluator::{evaluate, EvaluatorConfig};
use pwdir_protocol::{DiscoveredRc, EvaluationResult, RC_FILENAME};

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
        "pwsh (PowerShell 7+) was not found on PATH; it is required for evaluator integration tests"
    );
}

fn config(timeout_secs: u64) -> EvaluatorConfig {
    EvaluatorConfig {
        timeout: Duration::from_secs(timeout_secs),
        ..EvaluatorConfig::default()
    }
}

fn proj(dir: &Path) -> DiscoveredRc {
    DiscoveredRc {
        project_root: dir.to_path_buf(),
        rc_path: dir.join(RC_FILENAME),
    }
}

fn write_rc(dir: &Path, body: &str) -> DiscoveredRc {
    let rc = proj(dir);
    fs::write(&rc.rc_path, body).expect("write .envrc.ps1 fixture");
    rc
}

fn env_of<'a>(result: &'a EvaluationResult, name: &str) -> Option<std::borrow::Cow<'a, str>> {
    result
        .environment
        .get_canonical(name)
        .map(|v| v.to_string_lossy())
}

fn expect_ok(report: &pwdir_core::evaluator::EvaluationReport) -> EvaluationResult {
    assert!(
        report.error.is_none(),
        "unexpected evaluation error: {:?}",
        report.error
    );
    assert_eq!(report.exit_code, Some(0), "exit code should be 0");
    assert!(!report.timed_out, "must not time out");
    report
        .result
        .clone()
        .expect("ok evaluation must carry a result")
}

#[test]
fn sets_env_var() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "$env:FOO = 'bar'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FOO").as_deref(), Some("bar"));
    assert!(
        report.stderr.is_empty(),
        "no stderr expected: {:?}",
        report.stderr
    );
}

#[test]
fn removes_env_var() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "Remove-Item Env:PWDIRENV_TEST_LEGACY -ErrorAction Ignore",
    );

    std::env::set_var("PWDIRENV_TEST_LEGACY", "doomed");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert!(
        env_of(&result, "PWDIRENV_TEST_LEGACY").is_none(),
        "removed variable must be absent from resulting environment"
    );

    std::env::remove_var("PWDIRENV_TEST_LEGACY");
}

#[test]
fn prepends_path() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "$env:PATH = 'C:\\first;' + $env:PATH");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let path = env_of(&result, "PATH").expect("PATH present");
    assert!(
        path.starts_with("C:\\first;"),
        "PATH should start with C:\\first; but was: {path}"
    );
}

#[test]
fn pipeline_json_parsing() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("package.json"), r#"{"version": "1.2.3"}"#).expect("write fixture");
    let rc = write_rc(
        tmp.path(),
        "$v = (Get-Content package.json | ConvertFrom-Json).version\n$env:APP_VERSION = [string]$v",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "APP_VERSION").as_deref(), Some("1.2.3"));
}

#[test]
fn invoke_expression() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "'$env:FROM_IEX = \"via-iex\"' | Invoke-Expression",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FROM_IEX").as_deref(), Some("via-iex"));
}

#[test]
fn write_output_noise_does_not_break_protocol() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "Write-Output 'noise1'\n$env:FOO = 'bar'\nWrite-Output 'noise2'\nWrite-Output 'noise3'",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FOO").as_deref(), Some("bar"));
    assert!(report.stdout.contains("noise1"));
    assert!(report.stdout.contains("noise3"));
}

#[test]
fn write_host_noise_does_not_break_protocol() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "Write-Host 'hello host'\n$env:FOO = 'bar'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FOO").as_deref(), Some("bar"));
    assert!(
        report.stdout.contains("hello host") || report.stderr.contains("hello host"),
        "host noise must land on a diagnostic channel"
    );
}

#[test]
fn throwing_rc_produces_structured_error() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(tmp.path(), "throw 'boom'");

    let report = evaluate(&config(15), &rc).expect("evaluate returns report");
    assert!(report.result.is_none(), "failed evaluation has no result");
    let error = report.error.expect("structured error present");
    assert_eq!(error.kind, "evaluation-failed");
    assert!(
        error.message.contains("boom"),
        "error message should contain the throw payload: {:?}",
        error.message
    );
    assert!(
        error
            .exception
            .as_deref()
            .is_some_and(|e| e.contains("RuntimeException")),
        "exception type should be captured: {:?}",
        error.exception
    );
    assert_ne!(
        report.exit_code,
        Some(0),
        "failed evaluation must exit non-zero"
    );
}

#[test]
fn infinite_loop_times_out_and_kills_process_tree() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "while ($true) { Start-Sleep -Milliseconds 100 }",
    );

    let report = evaluate(&config(2), &rc).expect("evaluate returns report");
    assert!(report.timed_out, "must be flagged as timed out");
    assert!(
        report.result.is_none(),
        "timed-out evaluation has no result"
    );
    let error = report.error.expect("timeout error present");
    assert_eq!(error.kind, "timeout");
    assert!(
        report.duration >= Duration::from_secs(2),
        "duration must cover the full timeout window: {:?}",
        report.duration
    );
}

#[test]
fn project_root_with_spaces() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("proj with spaces");
    fs::create_dir(&project).expect("mkdir project");
    let rc = write_rc(&project, "$env:FOO = 'bar'");

    assert!(
        rc.rc_path.as_os_str().to_string_lossy().contains(' '),
        "fixture sanity: rc path contains a space"
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FOO").as_deref(), Some("bar"));
}

#[test]
fn missing_result_with_long_unicode_stderr_is_reported_without_panic() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "[Console]::Error.WriteLine(('错误信息' * 100)); [Environment]::Exit(7)",
    );
    let report = evaluate(&config(15), &rc).expect("evaluate");
    assert!(report.result.is_none());
    assert_eq!(report.exit_code, Some(7));
    let error = report.error.expect("structured fallback error");
    assert_eq!(error.kind, "no-result");
    assert!(error
        .message
        .ends_with(&"错误信息".repeat(100).chars().take(66).collect::<String>()));
}
