use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use pwdir_core::evaluator::{evaluate, EvaluatorConfig};
use pwdir_protocol::{DiscoveredRc, EvaluationResult, WatchDependency, RC_FILENAME};

static PWSH_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn temp_project() -> tempfile::TempDir {
    // Hosted Windows runners may expose TEMP through an 8.3 alias. PowerShell
    // expands that alias, so construct fixtures with the long path on both sides.
    // The helper contract tests lexical normalization, not filesystem identity.
    let canonical = fs::canonicalize(std::env::temp_dir()).expect("resolve temp root");
    #[cfg(windows)]
    let root = {
        let text = canonical.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{unc}"))
        } else {
            PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
        }
    };
    #[cfg(not(windows))]
    let root: PathBuf = canonical;
    tempfile::tempdir_in(root).expect("temp project")
}

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
        "pwsh (PowerShell 7+) was not found on PATH; it is required for stdlib integration tests"
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
fn path_add_prepends_project_root_relative_path() {
    ensure_pwsh();
    let tmp = temp_project();
    let rc = write_rc(tmp.path(), "PATH_add '.\\bin'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let path = env_of(&result, "PATH").expect("PATH present");
    let expected_prefix = format!("{};", tmp.path().join("bin").display());
    assert!(
        path.starts_with(&expected_prefix),
        "PATH should start with {expected_prefix} but was: {path}"
    );
}

#[test]
fn path_add_dedupes_equivalent_entry_and_moves_it_to_front() {
    ensure_pwsh();
    let tmp = temp_project();
    let root = tmp.path().to_string_lossy().replace('\\', "/");
    let equivalent = format!("{root}/BIN/");
    // Set PATH inside this evaluator, not in the concurrently shared test process.
    let equivalent = equivalent.replace('\'', "''");
    let rc = write_rc(
        tmp.path(),
        &format!(
            "$env:PATH = 'zzz-pwdirenv-before;{equivalent};zzz-pwdirenv-after;' + $env:PATH\nPATH_add '.\\bin'"
        ),
    );
    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let path = env_of(&result, "PATH").expect("PATH present");

    let items: Vec<&str> = path.split(';').collect();
    let normalize = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let expected = tmp.path().join("bin").to_string_lossy().to_string();
    let expected_norm = normalize(&expected);
    let count = items
        .iter()
        .filter(|item| normalize(item) == expected_norm)
        .count();
    assert_eq!(
        count, 1,
        "equivalent entry must be deduped to exactly one occurrence, PATH was: {path}"
    );
    assert_eq!(
        items.first().copied(),
        Some(expected.as_str()),
        "deduped entry must be moved to the very front"
    );
    let before_pos = items
        .iter()
        .position(|item| *item == "zzz-pwdirenv-before")
        .unwrap_or_else(|| panic!("sentinel entry 'before' must survive, PATH was: {path}"));
    let after_pos = items
        .iter()
        .position(|item| *item == "zzz-pwdirenv-after")
        .unwrap_or_else(|| panic!("sentinel entry 'after' must survive, PATH was: {path}"));
    assert!(
        before_pos < after_pos,
        "other entries must keep their relative order, PATH was: {path}"
    );
}

#[test]
fn path_add_allows_nonexistent_directory() {
    ensure_pwsh();
    let tmp = temp_project();
    let rc = write_rc(tmp.path(), "PATH_add '.\\does-not-exist-at-eval-time'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let path = env_of(&result, "PATH").expect("PATH present");
    let expected_prefix = format!(
        "{};",
        tmp.path().join("does-not-exist-at-eval-time").display()
    );
    assert!(
        path.starts_with(&expected_prefix),
        "nonexistent directory must still be prepended, PATH was: {path}"
    );
}

#[test]
fn dotenv_parses_fixture_and_does_not_override_existing() {
    ensure_pwsh();
    let tmp = temp_project();
    let lines = [
        "\u{FEFF}# PwDirEnv dotenv fixture",
        "",
        "KEY=value",
        r#"export E2=quoted "a b""#,
        r#"E3='single quoted'"#,
        r#"E4="with \"esc\" and \\slash""#,
        r#"E5="line1\nline2""#,
        "E6 = unquoted trimmed   ",
        "E7=",
        "9INVALID=x",
        "BAD-KEY=x",
        "NOEQ",
        "PWDIRENV_DOTENV_EXISTING=changed",
    ];
    fs::write(
        tmp.path().join(".env"),
        format!("{}\r\n", lines.join("\r\n")),
    )
    .expect("write .env fixture");

    let rc = write_rc(tmp.path(), "$env:PWDIRENV_DOTENV_EXISTING = 'keep'\ndotenv");
    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);

    assert_eq!(env_of(&result, "KEY").as_deref(), Some("value"));
    assert_eq!(env_of(&result, "E2").as_deref(), Some(r#"quoted "a b""#));
    assert_eq!(env_of(&result, "E3").as_deref(), Some("single quoted"));
    assert_eq!(
        env_of(&result, "E4").as_deref(),
        Some(r#"with "esc" and \slash"#)
    );
    assert_eq!(env_of(&result, "E5").as_deref(), Some("line1\nline2"));
    assert_eq!(env_of(&result, "E6").as_deref(), Some("unquoted trimmed"));
    assert_eq!(env_of(&result, "E7").as_deref(), Some(""));
    assert!(env_of(&result, "9INVALID").is_none());
    assert!(env_of(&result, "BAD-KEY").is_none());
    assert!(env_of(&result, "NOEQ").is_none());
    assert_eq!(
        env_of(&result, "PWDIRENV_DOTENV_EXISTING").as_deref(),
        Some("keep")
    );
}

#[test]
fn dotenv_overload_switch_forces_override() {
    ensure_pwsh();
    let tmp = temp_project();
    fs::write(
        tmp.path().join(".env"),
        "PWDIRENV_DOTENV_OVERLOAD=after\r\n",
    )
    .expect("write .env fixture");
    let rc = write_rc(
        tmp.path(),
        "$env:PWDIRENV_DOTENV_OVERLOAD = 'before'\ndotenv -Overload",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(
        env_of(&result, "PWDIRENV_DOTENV_OVERLOAD").as_deref(),
        Some("after")
    );
}

#[test]
fn dotenv_missing_file_fails_evaluation() {
    ensure_pwsh();
    let tmp = temp_project();
    let rc = write_rc(tmp.path(), "dotenv '.env.definitely-missing'");

    let report = evaluate(&config(15), &rc).expect("evaluate returns report");
    assert!(report.result.is_none(), "failed evaluation has no result");
    let error = report.error.expect("structured error present");
    assert_eq!(error.kind, "evaluation-failed");
    assert!(
        error.message.contains("dotenv"),
        "error message should mention dotenv: {:?}",
        error.message
    );
    assert_ne!(
        report.exit_code,
        Some(0),
        "failed evaluation must exit non-zero"
    );
}

#[test]
fn dotenv_if_exists_existing_parses_and_missing_is_silent_noop() {
    ensure_pwsh();
    let tmp = temp_project();
    fs::write(tmp.path().join(".env.local"), "FROM_LOCAL=yes\r\n").expect("write .env.local");

    let rc = write_rc(
        tmp.path(),
        "dotenv_if_exists '.env.local'\ndotenv_if_exists '.env.missing'\n$env:PWDIRENV_MARKER = 'ok'",
    );
    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FROM_LOCAL").as_deref(), Some("yes"));
    assert_eq!(env_of(&result, "PWDIRENV_MARKER").as_deref(), Some("ok"));
}

#[test]
fn watch_file_and_watch_dir_resolve_against_project_root() {
    ensure_pwsh();
    let tmp = temp_project();
    let rc = write_rc(
        tmp.path(),
        "watch_file 'package.json'\nwatch_file 'package.json' '..\\shared\\x.ps1'\nwatch_dir '.\\config'\nwatch_dir '.\\config'",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let parent = tmp.path().parent().expect("tempdir always has a parent");
    let expected = vec![
        WatchDependency::File(tmp.path().join("package.json")),
        WatchDependency::File(parent.join("shared").join("x.ps1")),
        WatchDependency::Directory(tmp.path().join("config")),
    ];
    assert_eq!(result.watches, expected);
}

#[test]
fn source_env_runs_in_scope_and_records_metadata() {
    ensure_pwsh();
    let tmp = temp_project();
    fs::write(tmp.path().join("common.ps1"), "$env:FROM_SOURCE = 'yes'")
        .expect("write common.ps1 fixture");
    let rc = write_rc(tmp.path(), "source_env '.\\common.ps1'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FROM_SOURCE").as_deref(), Some("yes"));
    let expected = tmp.path().join("common.ps1");
    assert_eq!(result.sourced_files, vec![expected.clone()]);
    assert_eq!(result.watches, vec![WatchDependency::File(expected)]);
}

#[test]
fn source_up_loads_nearest_parent_rc() {
    ensure_pwsh();
    let tmp = temp_project();
    fs::write(tmp.path().join(RC_FILENAME), "$env:FROM_PARENT = 'up'")
        .expect("write parent rc fixture");
    let project = tmp.path().join("proj");
    fs::create_dir(&project).expect("mkdir proj");
    let rc = write_rc(&project, "source_up");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FROM_PARENT").as_deref(), Some("up"));
    let parent_rc = tmp.path().join(RC_FILENAME);
    assert_eq!(result.sourced_files, vec![parent_rc.clone()]);
    assert_eq!(result.watches, vec![WatchDependency::File(parent_rc)]);
}

#[test]
fn source_up_without_parent_rc_is_noop() {
    ensure_pwsh();
    let tmp = temp_project();
    let project = tmp.path().join("proj");
    fs::create_dir(&project).expect("mkdir proj");
    let rc = write_rc(&project, "source_up\n$env:PWDIRENV_MARKER = 'ok'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "PWDIRENV_MARKER").as_deref(), Some("ok"));
    assert!(env_of(&result, "FROM_PARENT").is_none());
    assert!(result.sourced_files.is_empty());
    assert!(result.watches.is_empty());
}

#[test]
fn source_up_chain_loads_each_ancestor_once_and_restores_context() {
    ensure_pwsh();
    let tmp = temp_project();
    let parent = tmp.path().join("parent");
    let child = parent.join("child");
    fs::create_dir_all(&child).expect("mkdirs");
    let grandparent_rc = write_rc(tmp.path(), "$env:PW_CHAIN += 'G'");
    let parent_rc = write_rc(&parent, "$env:PW_CHAIN += 'P'; if ($env:PW_CHAIN.Length -gt 4) { throw 'recursive source_up' }; source_up");
    fs::write(parent.join("extra.ps1"), "$env:PW_CHAIN += 'E'").expect("extra source");
    let rc = write_rc(
        &child,
        "$env:PW_CHAIN = 'C'; source_up; source_up 'extra.ps1'",
    );
    let report = evaluate(&config(15), &rc).expect("evaluate");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "PW_CHAIN").as_deref(), Some("CPGE"));
    let expected = vec![
        grandparent_rc.rc_path,
        parent_rc.rc_path,
        parent.join("extra.ps1"),
    ];
    assert_eq!(result.sourced_files, expected);
    assert_eq!(
        result.watches,
        expected
            .into_iter()
            .map(WatchDependency::File)
            .collect::<Vec<_>>()
    );
}

#[test]
fn source_up_restores_context_after_caught_source_error() {
    ensure_pwsh();
    let tmp = temp_project();
    let child = tmp.path().join("child");
    fs::create_dir(&child).expect("mkdir child");
    fs::write(tmp.path().join("broken.ps1"), "throw 'expected'").expect("broken source");
    let parent_rc = write_rc(tmp.path(), "$env:PW_RECOVERED = 'yes'");
    let rc = write_rc(&child, "try { source_up 'broken.ps1' } catch {}; source_up");
    let report = evaluate(&config(15), &rc).expect("evaluate");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "PW_RECOVERED").as_deref(), Some("yes"));
    assert_eq!(result.sourced_files, vec![parent_rc.rc_path]);
}
