//! exec 编排（M7，SPEC §14.5）：以解析后的项目环境启动任意子进程。
//!
//! 安全边界（SPEC §14.5 / §27）：
//! - 未 allow 的 RC **一行都不执行**（包括 evaluator 的部分执行）；
//! - COMMAND 以 argv 形式直接 spawn —— 绝不经 shell 字符串拼接（§27 明令禁止），
//!   参数中的空格 / 引号 / `$` 等由 OS 进程模型天然隔离；
//! - 评估结果只通过 [`Command::env`] / [`Command::env_remove`] 作用于**本次子进程**，
//!   绝不触碰 User/Machine scope（ADR-006）。
//!
//! diff 基线：evaluator 从 pwdir.exe 的进程环境开始（继承自 Host，SPEC §7.3），
//! 因此 `diff(from_process(), result.environment)` 应用在子进程继承环境之上，
//! 恰好还原出 evaluator 的 resulting environment。

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use pwdir_protocol::EnvironmentSnapshot;

use crate::diff::diff_env;
use crate::discovery;
use crate::evaluator::{self, EvaluatorConfig};
use crate::trust::TrustStore;
use crate::PwDirError;

/// exec 场景的 evaluator 超时（SPEC §7.6："pwdir exec 可允许更长 evaluator timeout"）。
///
/// exec 的 RC 往往做重量级初始化（fnm env、拉取 secrets、构建缓存等），hook 的 10s
/// 默认值太紧；固定放宽为 60s（SPEC §7.6 hook 模式上限同值），不改
/// [`EvaluatorConfig`] 的默认值，避免影响 export/hook 快速路径的语义。
pub const EXEC_EVALUATOR_TIMEOUT: Duration = Duration::from_secs(60);

/// 一次 exec 的结果：子进程退出码 + 捕获的 stdout/stderr。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// 用默认 trust store 执行一次 exec（生产入口）。
pub fn run_exec(
    dir: &Path,
    command: &str,
    args: &[String],
    require_rc: bool,
    cfg: &EvaluatorConfig,
) -> Result<ExecOutcome, PwDirError> {
    let store = TrustStore::open()?;
    run_exec_with_store(dir, command, args, require_rc, cfg, &store)
}

/// 用注入的 trust store 执行一次 exec（测试可注入临时目录，不碰真实 LOCALAPPDATA）。
pub fn run_exec_with_store(
    dir: &Path,
    command: &str,
    args: &[String],
    require_rc: bool,
    cfg: &EvaluatorConfig,
    store: &TrustStore,
) -> Result<ExecOutcome, PwDirError> {
    // 1. 发现 RC：无 RC 且 require_rc → RcNotFound；无 RC 且不 require → 直接继承环境执行
    let rc = match discovery::find_rc(dir)? {
        Some(rc) => Some(rc),
        None if require_rc => {
            return Err(PwDirError::RcNotFound(
                "no .envrc.ps1 found in this or any parent directory".to_string(),
            ));
        }
        None => None,
    };

    // 2. trust 校验：未 allow → NotAllowed，RC 内容绝不执行（安全边界 > 一切）
    if let Some(rc) = &rc {
        if !store.is_allowed(&rc.rc_path)? {
            return Err(PwDirError::NotAllowed("run `pwdir allow`".to_string()));
        }
    }

    // 3. 隔离评估（仅 allowed RC 才走到这里），exec 专用更长超时
    let patch = if let Some(rc) = &rc {
        let exec_cfg = EvaluatorConfig {
            timeout: EXEC_EVALUATOR_TIMEOUT,
            ..cfg.clone()
        };
        let report = evaluator::evaluate(&exec_cfg, rc)?;
        match (report.result, report.error) {
            (Some(result), _) => {
                // RC 若在 evaluator 运行期间被修改，result 与当前受信内容不再对应；
                // 此时禁止启动目标命令，避免把过期 patch 当成已授权配置使用。
                if !store.is_allowed(&rc.rc_path)? {
                    return Err(PwDirError::NotAllowed(
                        "rc changed during evaluation; run `pwdir allow`".to_string(),
                    ));
                }
                let baseline = EnvironmentSnapshot::from_process();
                diff_env(&baseline, &result.environment)
            }
            // RC 抛错/超时 → exec 失败，不 spawn 任何子进程
            (None, Some(error)) => return Err(PwDirError::EvaluationFailed(error.message)),
            // 防御分支：evaluator 契约保证 result/error 至少有一个
            (None, None) => {
                return Err(PwDirError::EvaluationFailed(
                    "evaluator produced neither a result nor an error".to_string(),
                ));
            }
        }
    } else {
        Default::default()
    };

    // 4. argv-safe spawn：直接 Command::new(command).args(args)，不经 shell 拼接。
    //    子进程默认继承 pwdir 进程环境，再叠加 diff patch 即得 evaluator 的 resulting env。
    let mut cmd = Command::new(command);
    cmd.args(args).current_dir(dir);
    for (key, value) in &patch.set {
        cmd.env(key, value);
    }
    for key in &patch.unset {
        cmd.env_remove(key);
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(PwDirError::Io(std::io::Error::other(format!(
                "command {command:?} not found; use an absolute path or a command already on PATH"
            ))));
        }
        Err(e) => return Err(e.into()),
    };

    // spawn 后立即用两个线程并发排空 stdout/stderr：不并发排空的话，子进程写满
    // 管道缓冲区会阻塞在 Write 上直到我们这边等待结束（假死/死锁形态）
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| evaluator::missing_pipe_error("stdout"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| evaluator::missing_pipe_error("stderr"))?;
    let stdout_handle = thread::spawn(move || evaluator::drain_to_string(stdout_pipe));
    let stderr_handle = thread::spawn(move || evaluator::drain_to_string(stderr_pipe));

    // exec 不设超时（SPEC §14.5：返回 COMMAND exit code；长任务属预期行为）
    let status = child.wait()?;

    // 子进程已退出，管道写端关闭，drain 线程必然到达 EOF
    let stdout = stdout_handle
        .join()
        .unwrap_or_else(|_| String::from("<stdout drain thread panicked>"));
    let stderr = stderr_handle
        .join()
        .unwrap_or_else(|_| String::from("<stderr drain thread panicked>"));

    Ok(ExecOutcome {
        exit_code: status.code(),
        stdout,
        stderr,
    })
}
