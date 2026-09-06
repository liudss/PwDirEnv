//! Evaluator launcher：spawn 隔离 pwsh 子进程执行 RC（M2，ADR-002 / SPEC §7）。
//!
//! 执行模型（SPEC §7）：
//! - `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy <policy> -File bootstrap.ps1`
//!   子进程内按 §17 十步序列：init state → stdlib → Set-Location → dot-source RC → 枚举 env。
//! - 结果通道：bootstrap 把结构化 JSON 写入 native core 分配的结果文件
//!   （同目录 `.tmp` + atomic rename），协议结果绝不与 stdout/stderr 混用（§7.5）。
//! - 超时（§7.6）：`wait_timeout` 到点后 `taskkill /T /F` 杀整个进程树。
//!
//! bootstrap.ps1 / stdlib.ps1 在编译期 `include_str!` 嵌入二进制，运行时解包到
//! 一次性 temp 目录 —— 部署后仓库目录可能不存在，`-File` 不能依赖源码相对路径。

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

// include_str! 相对路径基于本 .rs 文件位置：src/ → pwdir-core → crates → 仓库根/evaluator
/// bootstrap.ps1（SPEC §17 十步序列）。UTF-8 无 BOM 落盘后由 `pwsh -File` 执行。
const BOOTSTRAP_PS1: &str = include_str!("../../../evaluator/bootstrap.ps1");
/// stdlib.ps1（M5 前为骨架，但必须可被 bootstrap dot-source）。
const STDLIB_PS1: &str = include_str!("../../../evaluator/stdlib.ps1");

/// 嵌入脚本版本指纹（M6）：bootstrap / stdlib 内容的 SHA-256，进程内只算一次。
///
/// 脚本内容的任何变化（= evaluator 行为可能变化）都必须使全部 cache 失效，
/// 因此版本号直接取内容 hash 而非手工维护的版本字符串。
static SCRIPT_VERSIONS: LazyLock<(String, String)> = LazyLock::new(|| {
    (
        crate::trust::sha256_hex(BOOTSTRAP_PS1.as_bytes()),
        crate::trust::sha256_hex(STDLIB_PS1.as_bytes()),
    )
});

/// 返回 `(bootstrap_sha256_hex, stdlib_sha256_hex)`（M6 fingerprint 的版本分量）。
pub fn embedded_script_versions() -> (String, String) {
    SCRIPT_VERSIONS.clone()
}

/// evaluator 启动配置。默认值对齐 SPEC §7.2/§7.6。
#[derive(Debug, Clone)]
pub struct EvaluatorConfig {
    /// pwsh 可执行文件名或路径（默认 `pwsh`，要求 PowerShell 7+）。
    pub pwsh: String,
    /// 超时上限，默认 10s（SPEC §7.6）。
    pub timeout: Duration,
    /// `-ExecutionPolicy` 参数，仅作用于本次 evaluator 进程。
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

/// 一次 evaluation 的完整报告：成功时 `result` 为 Some；失败/超时时 `error` 为 Some。
/// stdout/stderr 原样保留（diagnostics 素材），与结构化结果互不干扰。
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

/// 超时清理（taskkill、父进程回收和诊断管道）都必须有自己的上限。RC 不可信，
/// 不能让“超时处理”本身重新变成无界等待。
const TIMEOUT_CLEANUP_LIMIT: Duration = Duration::from_secs(5);

/// 一次性工作目录命名用的进程内序列号（配合 pid + 纳秒时间戳防碰撞）。
static WORK_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_tag() -> String {
    let seq = WORK_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

/// 创建一次性工作目录：`%TEMP%/PwDirEnv-<pid>-<nanos>-<seq>/`（SPEC §18.4：当前用户专用目录）。
fn create_work_dir() -> Result<PathBuf, PwDirError> {
    let dir = std::env::temp_dir().join(format!("PwDirEnv-{}", unique_tag()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 执行一次 RC evaluation（SPEC §7.1：Host 永不执行 RC，只在隔离子进程中执行）。
///
/// `Err` 仅表示基础设施失败（pwsh 不存在、temp 目录不可写等，`PwDirError::Io`）；
/// RC 本身的 throw / 超时属于"评估结果"，通过 [`EvaluationReport::error`] 返回。
pub fn evaluate(cfg: &EvaluatorConfig, rc: &DiscoveredRc) -> Result<EvaluationReport, PwDirError> {
    let started = Instant::now();
    let work_dir = create_work_dir()?;
    let report = run(cfg, rc, &work_dir, started);
    // best-effort 清理一次性目录：结果已读入内存，残留不影响正确性（consume 后删除，§18.4）
    let _ = fs::remove_dir_all(&work_dir);
    report
}

fn run(
    cfg: &EvaluatorConfig,
    rc: &DiscoveredRc,
    work_dir: &Path,
    started: Instant,
) -> Result<EvaluationReport, PwDirError> {
    // 1. 解包嵌入脚本 + 分配结果文件路径（工作目录本身全新且不可预测，天然无同名冲突）
    let bootstrap_path = work_dir.join("bootstrap.ps1");
    let stdlib_path = work_dir.join("stdlib.ps1");
    let result_path = work_dir.join(format!("result-{}.json", unique_tag()));
    debug_assert!(!result_path.exists());
    // fs::write 写原始字节（&str 即 UTF-8、无 BOM），供 pwsh 按 UTF-8 读取
    fs::write(&bootstrap_path, BOOTSTRAP_PS1)?;
    fs::write(&stdlib_path, STDLIB_PS1)?;

    // 2. 构造命令。路径经 argv 传递，空格/Unicode 天然安全；
    //    current_dir 不设置 —— 继承 pwdir.exe 的（即 Host 当前，SPEC §7.3）。
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

    // 3. spawn 后立即用两个线程分别 drain stdout/stderr：
    //    若不并发排空，子进程写满管道缓冲区后会阻塞在 Write 上直到超时（假超时/死锁形态）。
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

    // 4. 带超时等待；到点杀整棵进程树
    let mut timed_out = false;
    let status = match child.wait_timeout(cfg.timeout)? {
        Some(status) => status,
        None => {
            timed_out = true;
            kill_process_tree(&mut child);
            // `taskkill` 可能因为系统服务或权限问题卡住/失败；即使如此也要直接
            // 终止 evaluator 本身，并把等待限制在清理窗口内。
            match child.wait_timeout(TIMEOUT_CLEANUP_LIMIT)? {
                Some(status) => status,
                None => {
                    let _ = child.kill();
                    child.wait()?
                }
            }
        }
    };

    // 正常退出时等待完整诊断输出。超时路径中，RC 的孤儿进程可能仍持有继承的
    // stdout/stderr 写端；因此只能在有限窗口内收集，绝不能让 drain 永久阻塞返回。
    let stdout = receive_drain(stdout_rx, timed_out, "stdout");
    let stderr = receive_drain(stderr_rx, timed_out, "stderr");

    // 5. 读结果文件并按 ok 字段分流
    let exit_code = status.code();
    let (result, error) = match fs::read_to_string(&result_path) {
        Ok(raw) => parse_result_file(&raw),
        Err(_) => (None, Some(fallback_error(timed_out, exit_code, &stderr))),
    };

    // 6. 汇总报告；temp 目录由调用方清理
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

/// 管道缺失的统一错误（evaluator 与 exec 共用）。
pub(crate) fn missing_pipe_error(which: &str) -> PwDirError {
    PwDirError::Io(std::io::Error::other(format!(
        "child process did not provide a {which} pipe"
    )))
}

/// 排空管道为 String。pwsh 输出按控制台编码（Windows 上通常 UTF-8）；
/// 无法解码的字节做有损替换 —— 诊断通道允许 lossy，协议通道在结果文件里。
/// evaluator 与 exec 的子进程输出捕获共用。
pub(crate) fn drain_to_string(mut pipe: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// 超时后杀整棵进程树（SPEC §7.6 "kill process tree"）。
///
/// Windows 上必须 `taskkill /T /F`：RC 可能启动了自己的子进程，仅 kill pwsh
/// 会留下孤儿且不关闭继承的管道写端，drain 线程将永远等不到 EOF。
#[cfg(windows)]
fn kill_process_tree(child: &mut Child) {
    let pid = child.id().to_string();
    // 不用 `.status()`：taskkill 自己也可能在系统调用中卡住，导致原本有上限的
    // evaluator timeout 变成无限等待。清理命令在自己的有限窗口内运行。
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
    // taskkill 是“整树”清理；直接 kill 是它失败时保证 evaluator 父进程能退出的兜底。
    let _ = child.kill();
}

#[cfg(not(windows))]
fn kill_process_tree(child: &mut Child) {
    let _ = child.kill();
}

/// bootstrap 错误信封（`{ ok: false, error: {...} }`）。
#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Option<EvaluationError>,
}

/// 解析 bootstrap 产物：按顶层 `ok` 字段分流成 EvaluationResult / EvaluationError。
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
        // 信封里的 schema/ok 为额外字段，serde 默认忽略；只取协议字段
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

/// 结果文件缺失（崩溃 / 被 RC exit / 超时杀树）时的兜底错误。
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
    // stderr 末尾往往有 pwsh 的崩溃线索；截尾避免错误消息爆炸
    let hint = stderr.lines().rev().find(|l| !l.trim().is_empty());
    let hint = hint.map(|l| l.trim()).unwrap_or_default();
    let hint = if hint.len() > 200 { &hint[..200] } else { hint };
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
        // bootstrap 与 stdlib 是不同文件，hash 不应相同
        assert_ne!(bootstrap, stdlib);
        // 嵌入内容不变则版本稳定；两次调用取同一缓存值
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
