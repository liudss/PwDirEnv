//! Launch an isolated pwsh with embedded scripts and a separate JSON result file. Drain both output pipes concurrently and attempt process-tree cleanup on timeout.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use wait_timeout::ChildExt;

use pwdir_protocol::{DiscoveredRc, EvaluationError, EvaluationResult};

use crate::PwDirError;

const BOOTSTRAP_PS1: &str = include_str!("../../../evaluator/bootstrap.ps1");
const STDLIB_PS1: &str = include_str!("../../../evaluator/stdlib.ps1");

// Hash scripts once per native process; content changes invalidate cache.
static SCRIPT_VERSIONS: LazyLock<(String, String)> = LazyLock::new(|| {
    (
        crate::trust::sha256_hex(BOOTSTRAP_PS1.as_bytes()),
        crate::trust::sha256_hex(STDLIB_PS1.as_bytes()),
    )
});

pub fn embedded_script_versions() -> (String, String) {
    SCRIPT_VERSIONS.clone()
}

#[derive(Debug, Clone)]
pub struct EvaluatorConfig {
    pub pwsh: String,
    pub timeout: Duration,
    pub execution_policy: String,
}

impl Default for EvaluatorConfig {
    fn default() -> Self {
        Self {
            pwsh: "pwsh".to_string(),
            timeout: Duration::from_secs(10),
            execution_policy: "Bypass".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EvaluationReport {
    pub result: Option<EvaluationResult>,
    pub error: Option<EvaluationError>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub timed_out: bool,
}

const TIMEOUT_CLEANUP_LIMIT: Duration = Duration::from_secs(5);

static WORK_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_tag() -> String {
    let seq = WORK_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

fn create_work_dir() -> Result<PathBuf, PwDirError> {
    let dir = std::env::temp_dir().join(format!("PwDirEnv-{}", unique_tag()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Evaluate with best-effort temporary-file cleanup. Err denotes infrastructure
/// failure; RC failures are represented in EvaluationReport.error.
pub fn evaluate(cfg: &EvaluatorConfig, rc: &DiscoveredRc) -> Result<EvaluationReport, PwDirError> {
    let started = Instant::now();
    let work_dir = create_work_dir()?;
    let report = run(cfg, rc, &work_dir, started);
    let _ = fs::remove_dir_all(&work_dir);
    report
}

fn run(
    cfg: &EvaluatorConfig,
    rc: &DiscoveredRc,
    work_dir: &Path,
    started: Instant,
) -> Result<EvaluationReport, PwDirError> {
    let bootstrap_path = work_dir.join("bootstrap.ps1");
    let stdlib_path = work_dir.join("stdlib.ps1");
    let result_path = work_dir.join(format!("result-{}.json", unique_tag()));
    debug_assert!(!result_path.exists());
    fs::write(&bootstrap_path, BOOTSTRAP_PS1)?;
    fs::write(&stdlib_path, STDLIB_PS1)?;

    let mut command = Command::new(&cfg.pwsh);
    command
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg(&cfg.execution_policy)
        .arg("-File")
        .arg(&bootstrap_path)
        .arg("-RcPath")
        .arg(&rc.rc_path)
        .arg("-ProjectRoot")
        .arg(&rc.project_root)
        .arg("-ResultPath")
        .arg(&result_path)
        .arg("-StdlibPath")
        .arg(&stdlib_path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PwDirError::Io(std::io::Error::other(format!(
                "pwsh executable {:?} not found on PATH; install PowerShell 7+ or point evaluator config at a valid pwsh path",
                cfg.pwsh
            ))));
        }
        Err(e) => return Err(e.into()),
    };

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| missing_pipe_error("stdout"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| missing_pipe_error("stderr"))?;
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = stdout_tx.send(drain_to_string(stdout_pipe));
    });
    thread::spawn(move || {
        let _ = stderr_tx.send(drain_to_string(stderr_pipe));
    });

    let mut timed_out = false;
    let status = match child.wait_timeout(cfg.timeout)? {
        Some(status) => status,
        None => {
            timed_out = true;
            kill_process_tree(&mut child);
            match child.wait_timeout(TIMEOUT_CLEANUP_LIMIT)? {
                Some(status) => status,
                None => {
                    let _ = child.kill();
                    child.wait()?
                }
            }
        }
    };

    let stdout = receive_drain(stdout_rx, timed_out, "stdout");
    let stderr = receive_drain(stderr_rx, timed_out, "stderr");

    let exit_code = status.code();
    let (result, error) = match fs::read_to_string(&result_path) {
        Ok(raw) => parse_result_file(&raw),
        Err(_) => (None, Some(fallback_error(timed_out, exit_code, &stderr))),
    };

    Ok(EvaluationReport {
        result,
        error,
        exit_code,
        stdout,
        stderr,
        duration: started.elapsed(),
        timed_out,
    })
}

fn receive_drain(rx: mpsc::Receiver<String>, timed_out: bool, stream: &str) -> String {
    let result = if timed_out {
        rx.recv_timeout(TIMEOUT_CLEANUP_LIMIT).ok()
    } else {
        rx.recv().ok()
    };
    result.unwrap_or_else(|| format!("<{stream} drain unavailable after evaluator timeout>"))
}

pub(crate) fn missing_pipe_error(which: &str) -> PwDirError {
    PwDirError::Io(std::io::Error::other(format!(
        "child process did not provide a {which} pipe"
    )))
}

pub(crate) fn drain_to_string(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(windows)]
fn kill_process_tree(child: &mut Child) {
    let pid = child.id().to_string();
    let killer = Command::new("taskkill.exe")
        .arg("/PID")
        .arg(pid)
        .arg("/T")
        .arg("/F")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut killer) = killer {
        match killer.wait_timeout(TIMEOUT_CLEANUP_LIMIT) {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = killer.kill();
                let _ = killer.wait();
            }
            Err(_) => {
                let _ = killer.kill();
            }
        }
    }
    let _ = child.kill();
}

#[cfg(not(windows))]
fn kill_process_tree(child: &mut Child) {
    let _ = child.kill();
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Option<EvaluationError>,
}

fn parse_result_file(raw: &str) -> (Option<EvaluationResult>, Option<EvaluationError>) {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return (
                None,
                Some(EvaluationError {
                    kind: "protocol-invalid".to_string(),
                    message: format!("evaluator result file is not valid JSON: {e}"),
                    exception: None,
                    stack: None,
                }),
            );
        }
    };

    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if ok {
        match serde_json::from_value::<EvaluationResult>(value) {
            Ok(result) => (Some(result), None),
            Err(e) => (
                None,
                Some(EvaluationError {
                    kind: "protocol-invalid".to_string(),
                    message: format!("evaluator result file does not match protocol schema: {e}"),
                    exception: None,
                    stack: None,
                }),
            ),
        }
    } else {
        let error = match serde_json::from_value::<ErrorEnvelope>(value) {
            Ok(ErrorEnvelope { error: Some(error) }) => error,
            _ => EvaluationError {
                kind: "evaluation-failed".to_string(),
                message: "evaluator reported failure without structured error details".to_string(),
                exception: None,
                stack: None,
            },
        };
        (None, Some(error))
    }
}

fn fallback_error(timed_out: bool, exit_code: Option<i32>, stderr: &str) -> EvaluationError {
    if timed_out {
        return EvaluationError {
            kind: "timeout".to_string(),
            message:
                "evaluator timed out; the process tree was killed before a result file was written"
                    .to_string(),
            exception: None,
            stack: None,
        };
    }
    let hint = stderr.lines().rev().find(|l| !l.trim().is_empty());
    let hint = hint.map(|l| l.trim()).unwrap_or_default();
    let mut end = hint.len().min(200);
    while !hint.is_char_boundary(end) {
        end -= 1;
    }
    let hint = &hint[..end];
    EvaluationError {
        kind: "no-result".to_string(),
        message: format!(
            "evaluator exited with code {exit_code:?} without writing a result file; last stderr line: {hint}"
        ),
        exception: None,
        stack: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    use pwdir_protocol::WatchDependency;

    #[test]
    fn embedded_script_versions_are_stable_sha256_hex() {
        let (bootstrap, stdlib) = embedded_script_versions();
        assert_eq!(bootstrap.len(), 64);
        assert_eq!(stdlib.len(), 64);
        assert!(bootstrap.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(bootstrap, stdlib);
        assert_eq!(embedded_script_versions(), (bootstrap, stdlib));
    }

    #[test]
    fn default_config_matches_spec() {
        let cfg = EvaluatorConfig::default();
        assert_eq!(cfg.pwsh, "pwsh");
        assert_eq!(cfg.timeout, Duration::from_secs(10));
        assert_eq!(cfg.execution_policy, "Bypass");
    }

    #[test]
    fn parse_result_file_routes_by_ok_field() {
        let ok_payload = r#"{
            "schema": 1, "ok": true,
            "environment": {"vars": {"FOO": "bar"}},
            "watches": [{"kind": "file", "path": "C:\\x\\package.json"}],
            "sourced_files": [], "diagnostics": []
        }"#;
        let (result, error) = parse_result_file(ok_payload);
        assert!(error.is_none());
        let result = result.expect("ok payload yields result");
        assert_eq!(
            result.environment.get_canonical("FOO").map(OsString::from),
            Some(OsString::from("bar"))
        );
        assert_eq!(
            result.watches,
            vec![WatchDependency::File(PathBuf::from("C:\\x\\package.json"))]
        );

        let err_payload = r#"{
            "schema": 1, "ok": false,
            "error": {"kind": "evaluation-failed", "message": "boom",
                      "exception": "System.Management.Automation.RuntimeException", "stack": "at <ScriptBlock>"}
        }"#;
        let (result, error) = parse_result_file(err_payload);
        assert!(result.is_none());
        let error = error.expect("error payload yields error");
        assert_eq!(error.kind, "evaluation-failed");
        assert_eq!(error.message, "boom");
    }

    #[test]
    fn parse_result_file_reports_invalid_json() {
        let (result, error) = parse_result_file("not json at all");
        assert!(result.is_none());
        let error = error.expect("invalid json yields error");
        assert_eq!(error.kind, "protocol-invalid");
    }

    #[test]
    fn fallback_error_for_timeout_and_missing_result() {
        let e1 = fallback_error(true, None, "");
        assert_eq!(e1.kind, "timeout");

        let e2 = fallback_error(false, Some(1), "boom\r\nat line 1");
        assert_eq!(e2.kind, "no-result");
        assert!(e2.message.contains("Some(1)"));
        assert!(e2.message.contains("at line 1"));
    }
}
