//! M5 stdlib 集成测试（SPEC §24.3 的 M5 子集 + §9/§10/§11 语义）。
//!
//! 每个用例构造真实 temp 项目 + `.envrc.ps1`，spawn 真实 pwsh evaluator，
//! 断言结构化结果（environment / watches / sourced_files）。
//! helper 与 evaluator_integration.rs 保持同一模式（本地复制，避免为测试小工具引入公共模块重构）。

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use pwdir_core::evaluator::{evaluate, EvaluatorConfig};
use pwdir_protocol::{DiscoveredRc, EvaluationResult, WatchDependency, RC_FILENAME};

/// pwsh 可用性只探测一次（探测本身要 spawn pwsh）。
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
        "pwsh (PowerShell 7+) was not found on PATH; it is required for stdlib integration tests"
    );
}

fn config(timeout_secs: u64) -> EvaluatorConfig {
    EvaluatorConfig {
        timeout: Duration::from_secs(timeout_secs),
        ..EvaluatorConfig::default()
    }
}

/// 已存在 RC 文件的项目描述（RC 内容由调用方另行写入）。
fn proj(dir: &Path) -> DiscoveredRc {
    DiscoveredRc {
        project_root: dir.to_path_buf(),
        rc_path: dir.join(RC_FILENAME),
    }
}

/// 写入 `.envrc.ps1` 并返回项目描述。
fn write_rc(dir: &Path, body: &str) -> DiscoveredRc {
    let rc = proj(dir);
    fs::write(&rc.rc_path, body).expect("write .envrc.ps1 fixture");
    rc
}

/// 按 canonical（大小写不敏感）名称查询 resulting environment 中的变量。
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

/// 测试进程 PATH 的 RAII 守卫：构造时保存原值，drop（含 panic unwind）时恢复，
/// 保证「测试改进程 PATH」不泄漏到其它并行测试。
struct PathGuard {
    original: Option<std::ffi::OsString>,
}

impl PathGuard {
    fn set(value: String) -> Self {
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", value);
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }
}

#[test]
fn path_add_prepends_project_root_relative_path() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    // 等价形态：正斜杠 + 大小写不同（BIN）+ 尾分隔符 —— Windows 语义下与 <root>\bin 等价
    let root = tmp.path().to_string_lossy().replace('\\', "/");
    let equivalent = format!("{root}/BIN/");
    // 末尾追加原 PATH：本测试改的是测试进程环境（evaluator 继承），
    // pwsh 可执行文件的解析也依赖它，必须保持可解析
    let original = std::env::var_os("PATH").unwrap_or_default();
    let _guard = PathGuard::set(format!(
        "zzz-pwdirenv-before;{equivalent};zzz-pwdirenv-after;{}",
        original.to_string_lossy()
    ));

    let rc = write_rc(tmp.path(), "PATH_add '.\\bin'");
    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    let path = env_of(&result, "PATH").expect("PATH present");

    let items: Vec<&str> = path.split(';').collect();
    let normalize = |s: &str| s.replace('\\', "/").to_lowercase();
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
    // 其它项保持相对顺序。注意不硬编码 items[1]：pwsh 启动时会把自身安装目录
    // prepend 进自己进程的 PATH（在继承环境之后），因此只断言哨兵项存活且相对顺序不变
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
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    // UTF-8 + BOM、CRLF、注释、空行、export 前缀、单/双引号、最小 escape、非法 key 行
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
    // key 不合规 / 无 `=` 的行被静默跳过
    assert!(env_of(&result, "9INVALID").is_none());
    assert!(env_of(&result, "BAD-KEY").is_none());
    assert!(env_of(&result, "NOEQ").is_none());
    // 默认不覆盖已存在的环境变量（direnv 兼容语义）
    assert_eq!(
        env_of(&result, "PWDIRENV_DOTENV_EXISTING").as_deref(),
        Some("keep")
    );
}

#[test]
fn dotenv_overload_switch_forces_override() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join(".env.local"), "FROM_LOCAL=yes\r\n").expect("write .env.local");

    let rc = write_rc(
        tmp.path(),
        "dotenv_if_exists '.env.local'\ndotenv_if_exists '.env.missing'\n$env:PWDIRENV_MARKER = 'ok'",
    );
    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    // 存在的文件正常解析
    assert_eq!(env_of(&result, "FROM_LOCAL").as_deref(), Some("yes"));
    // 缺失的文件静默跳过，不阻断后续语句
    assert_eq!(env_of(&result, "PWDIRENV_MARKER").as_deref(), Some("ok"));
}

#[test]
fn watch_file_and_watch_dir_resolve_against_project_root() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let rc = write_rc(
        tmp.path(),
        "watch_file 'package.json'\nwatch_file 'package.json' '..\\shared\\x.ps1'\nwatch_dir '.\\config'\nwatch_dir '.\\config'",
    );

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    // 相对路径按 project root 绝对化（`..` 归约到 tempdir 的父目录）；
    // 重复注册（相同 kind + 等价路径）去重
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
    let tmp = tempfile::tempdir().expect("tempdir");
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
    let tmp = tempfile::tempdir().expect("tempdir");
    // 父目录放 .envrc.ps1，项目在其子目录：source_up 从 ProjectRoot 的父目录开始向上
    fs::write(tmp.path().join(RC_FILENAME), "$env:FROM_PARENT = 'up'")
        .expect("write parent rc fixture");
    let project = tmp.path().join("proj");
    fs::create_dir(&project).expect("mkdir proj");
    let rc = write_rc(&project, "source_up");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    assert_eq!(env_of(&result, "FROM_PARENT").as_deref(), Some("up"));
    // source_up 经 source_env 生效：sourced_files 与 watches 同步登记
    let parent_rc = tmp.path().join(RC_FILENAME);
    assert_eq!(result.sourced_files, vec![parent_rc.clone()]);
    assert_eq!(result.watches, vec![WatchDependency::File(parent_rc)]);
}

#[test]
fn source_up_without_parent_rc_is_noop() {
    ensure_pwsh();
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("proj");
    fs::create_dir(&project).expect("mkdir proj");
    let rc = write_rc(&project, "source_up\n$env:PWDIRENV_MARKER = 'ok'");

    let report = evaluate(&config(15), &rc).expect("evaluate ok");
    let result = expect_ok(&report);
    // 未找到父级 RC：静默 no-op，不报错也不阻断后续语句
    assert_eq!(env_of(&result, "PWDIRENV_MARKER").as_deref(), Some("ok"));
    assert!(env_of(&result, "FROM_PARENT").is_none());
    assert!(result.sourced_files.is_empty());
    assert!(result.watches.is_empty());
}
