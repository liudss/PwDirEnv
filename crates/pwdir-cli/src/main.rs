//! pwdir.exe：CLI 解析与命令路由（PLAN §1.1）。命令实现分布在 pwdir-core，
//! 本 crate 不重复实现 core 逻辑。

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use pwdir_core::cache::CacheStore;
use pwdir_core::dirs;
use pwdir_core::discovery;
use pwdir_core::doctor::{self, CheckStatus};
use pwdir_core::evaluator::EvaluatorConfig;
use pwdir_core::exec;
use pwdir_core::fingerprint::fingerprint_is_current;
use pwdir_core::trust::{self, TrustStore};
use pwdir_core::PwDirError;
use pwdir_protocol::DiscoveredRc;

mod hook;

#[derive(Debug, Parser)]
#[command(
    name = "pwdir",
    version,
    about = "PwDirEnv: directory environment manager for PowerShell (direnv equivalent)"
)]
struct PwDirCli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Authorize the RC file content (by SHA-256)
    Allow {
        /// Path to the RC file (or a directory to discover from); defaults to the RC for the current directory
        path: Option<PathBuf>,
    },
    /// Revoke the authorization of an RC file (idempotent)
    Deny {
        /// Path to the RC file (or a directory to discover from); defaults to the RC for the current directory
        path: Option<PathBuf>,
    },
    /// Show current status
    Status {
        /// Output in stable JSON schema
        #[arg(long)]
        json: bool,
    },
    /// Invalidate the current fingerprint cache
    Reload,
    /// Run a command with the resolved project environment (argv-safe)
    Exec {
        /// Project directory to resolve the RC from
        dir: PathBuf,
        /// Command to execute
        command: String,
        /// Command arguments (everything after COMMAND is passed through verbatim,
        /// so flags like `pwsh -NoProfile` are never parsed as pwdir options)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Fail when no RC is found (default: run with the inherited environment)
        #[arg(long)]
        require_rc: bool,
    },
    /// Export the environment patch (Host module protocol entry, SPEC §14.7)
    Export {
        /// Output format
        #[arg(long, default_value = "json")]
        format: String,
        /// Skip the fingerprint fast path and force a full evaluation (M6)
        #[arg(long)]
        force: bool,
    },
    /// Shell hook operations (print bootstrap snippet / install / uninstall)
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Diagnose the environment (pwsh version, trust/cache dirs, ExecutionPolicy, ...)
    Doctor {
        /// Output checks in a stable JSON schema
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HookAction {
    /// Print the pwsh hook bootstrap snippet (Import-Module + Enable-PwDirEnv)
    Pwsh,
    /// Install the PwDirEnv hook block into the PowerShell profile (idempotent)
    Install {
        /// Skip the interactive confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Target profile file (defaults to $PROFILE.CurrentUserAllHosts)
        #[arg(long, value_name = "FILE")]
        path: Option<PathBuf>,
    },
    /// Remove the PwDirEnv hook block from the PowerShell profile
    Uninstall {
        /// Target profile file (defaults to $PROFILE.CurrentUserAllHosts)
        #[arg(long, value_name = "FILE")]
        path: Option<PathBuf>,
    },
}

/// SPEC §14.1 退出码映射：0 成功 / 2 RcNotFound / 3 IO error / 1 其它。
fn exit_code_for(err: &PwDirError) -> i32 {
    match err {
        PwDirError::RcNotFound(_) => 2,
        PwDirError::Io(_) => 3,
        _ => 1,
    }
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            // PwDirError 的 Display 文本即面向用户的最终消息（RcNotFound 携带完整句子）；
            // 其它 anyhow 错误（参数解析等）走通用前缀
            if let Some(pw_err) = err.downcast_ref::<PwDirError>() {
                eprintln!("pwdir: {pw_err}");
                exit_code_for(pw_err)
            } else {
                eprintln!("pwdir: error: {err:#}");
                1
            }
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let cli = PwDirCli::parse();

    let Some(command) = cli.command else {
        // 顶层没有子命令时打印 --help
        PwDirCli::command().print_help()?;
        return Ok(0);
    };

    match command {
        // M3/M6：export 走 pwdir-core::export 编排（SPEC §14.7 协议入口，含 fingerprint 快速路径）
        Commands::Export { format, force } => {
            if format != "json" {
                anyhow::bail!("unsupported export format: {format} (only 'json' is supported)");
            }
            // cwd 不可用（当前目录被删除）时传空路径：export 返回 Noop + diagnostic，不 panic（SPEC §20.4）
            let cwd = discovery::current_dir_fs_or_none().unwrap_or_default();
            let output = pwdir_core::export::export(&cwd, &EvaluatorConfig::default(), force)?;
            // blocked / error 是协议语义（module 按 JSON 里的 action 分流），仍输出 JSON 并 exit 0；
            // 只有 export 返回 Err（基础设施故障）才走退出码映射
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(0)
        }
        // M1 done: allow / deny / status（discovery + trust）
        Commands::Allow { path } => Ok(cmd_allow(path.as_deref())?),
        Commands::Deny { path } => Ok(cmd_deny(path.as_deref())?),
        Commands::Status { json } => Ok(cmd_status(json)?),
        // M6：使当前 RC 的 fingerprint cache 失效（SPEC §14.4）
        Commands::Reload => Ok(cmd_reload()?),
        // M7：argv-safe exec（SPEC §14.5）
        Commands::Exec {
            dir,
            command,
            args,
            require_rc,
        } => Ok(cmd_exec(&dir, &command, &args, require_rc)?),
        // M7：hook bootstrap / install / uninstall（SPEC §3.1/§14.6）
        Commands::Hook { action } => match action {
            HookAction::Pwsh => Ok(cmd_hook_pwsh()?),
            HookAction::Install { yes, path } => Ok(cmd_hook_install(yes, path.as_deref())?),
            HookAction::Uninstall { path } => Ok(cmd_hook_uninstall(path.as_deref())?),
        },
        // M7：doctor（SPEC §14.8）
        Commands::Doctor { json } => Ok(cmd_doctor(json)?),
    }
}

/// 解析目标 RC（allow / deny 共用）：
/// - 未给 path：从当前目录向上发现；
/// - path 是目录：从该目录向上发现；
/// - path 是其它（显式 RC 文件路径，可能尚不存在）：直接使用，parent 作为 project root；
///   文件读取失败会在 allow 阶段以 IO 错误暴露（exit 3）。
fn resolve_rc(path: Option<&Path>) -> Result<DiscoveredRc, PwDirError> {
    match path {
        None => discover_from(&current_dir_or_rc_error()?),
        Some(p) if p.is_dir() => discover_from(p),
        Some(p) => {
            let project_root = p
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            Ok(DiscoveredRc {
                project_root,
                rc_path: p.to_path_buf(),
            })
        }
    }
}

fn discover_from(dir: &Path) -> Result<DiscoveredRc, PwDirError> {
    discovery::find_rc(dir)?.ok_or_else(|| {
        PwDirError::RcNotFound("no .envrc.ps1 found in this or any parent directory".to_string())
    })
}

/// SPEC §5.4/§20.4：当前目录不可用（被删除）时不 panic，按"找不到 RC"处理。
fn current_dir_or_rc_error() -> Result<PathBuf, PwDirError> {
    discovery::current_dir_fs_or_none().ok_or_else(|| {
        PwDirError::RcNotFound(
            "current directory is unavailable; no .envrc.ps1 found in this or any parent directory"
                .to_string(),
        )
    })
}

fn cmd_allow(path: Option<&Path>) -> Result<i32, PwDirError> {
    let rc = resolve_rc(path)?;
    let store = TrustStore::open()?;
    let record = store.allow(&rc.rc_path)?;
    println!("pwdir: allowed {}", record.path.display());
    Ok(0)
}

fn cmd_deny(path: Option<&Path>) -> Result<i32, PwDirError> {
    // deny 是幂等操作：没有 RC 就没有可撤销的授权（exit 0）
    let rc = match resolve_rc(path) {
        Ok(rc) => rc,
        Err(PwDirError::RcNotFound(_)) => {
            println!("pwdir: nothing to deny");
            return Ok(0);
        }
        Err(err) => return Err(err),
    };
    let store = TrustStore::open()?;
    store.deny(&rc.rc_path)?;
    println!(
        "pwdir: denied {}",
        store.canonical_path(&rc.rc_path).display()
    );
    Ok(0)
}

/// `pwdir reload`（M6，SPEC §14.4）：使当前 RC 的 fingerprint cache 失效，
/// 下一次 hook/export 强制 evaluator。找不到 RC → 提示并成功退出（与 deny 同语义）。
fn cmd_reload() -> Result<i32, PwDirError> {
    let cwd = match discovery::current_dir_fs_or_none() {
        Some(cwd) => cwd,
        None => {
            println!("pwdir: nothing to reload");
            return Ok(0);
        }
    };
    let rc = match discovery::find_rc(&cwd)? {
        Some(rc) => rc,
        None => {
            println!("pwdir: nothing to reload");
            return Ok(0);
        }
    };
    let cache = CacheStore::open()?;
    cache.invalidate(&rc.rc_path)?;
    println!("pwdir: reloaded cache for {}", rc.rc_path.display());
    Ok(0)
}

fn cmd_status(json: bool) -> Result<i32, PwDirError> {
    let cwd = discovery::current_dir_fs_or_none();
    // 发现失败（IO）不视为 status 失败：按"无 RC"呈现（SPEC §14.3，status 不做 evaluation）
    let discovered = cwd
        .as_deref()
        .and_then(|dir| discovery::find_rc(dir).ok().flatten());

    // trust 三态：none = 无 record；allowed = hash 匹配；blocked = 有 record 但内容已变
    let (trust_status, rc_sha256) = match &discovered {
        None => ("none".to_string(), None),
        Some(rc) => {
            let store = TrustStore::open()?;
            match store.record(&rc.rc_path)? {
                None => ("none".to_string(), None),
                Some(record) => {
                    // rc_sha256 显示当前内容的 hash；发现后文件被删（竞态）→ 视为不匹配
                    let current = trust::file_sha256(&rc.rc_path).ok();
                    let matched = current.as_deref() == Some(record.sha256.as_str());
                    let status = if matched { "allowed" } else { "blocked" };
                    (status.to_string(), current)
                }
            }
        }
    };

    // cache 三态（M6，SPEC §14.3）：none = 无条目；fresh = 指纹与当前现实一致；
    // stale = 有条目但指纹已失效（rc/watch/sourced/脚本版本任一变化）。
    // status 不评估、不写 cache；cache 子系统不可用时降级为 none（best-effort 呈现）。
    let cache_status = match &discovered {
        None => "none".to_string(),
        Some(rc) => match dirs::cache_dir() {
            Err(_) => "none".to_string(),
            Ok(dir) => match CacheStore::new(dir).load(&rc.rc_path) {
                Ok(None) => "none".to_string(),
                Ok(Some(entry)) => {
                    if fingerprint_is_current(&entry.fingerprint, &rc.rc_path) {
                        "fresh".to_string()
                    } else {
                        "stale".to_string()
                    }
                }
                Err(_) => "none".to_string(),
            },
        },
    };

    let dirs_root = dirs::pwdir_data_root().ok();

    if json {
        let payload = serde_json::json!({
            "cwd": cwd,
            "discovered_rc": discovered,
            "trust_status": trust_status,
            "rc_sha256": rc_sha256,
            "cache_status": cache_status,
            "dirs": dirs_root.map(|root| serde_json::json!({
                "allow": root.join("allow"),
                "cache": root.join("cache"),
                "state": root.join("state"),
            })),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "pwdir: cwd:             {}",
            cwd.as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(unavailable)".to_string())
        );
        println!(
            "pwdir: discovered rc:   {}",
            discovered
                .as_ref()
                .map(|rc| rc.rc_path.display().to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        println!("pwdir: trust status:    {trust_status}");
        println!(
            "pwdir: rc sha256:       {}",
            rc_sha256.as_deref().unwrap_or("(none)")
        );
        println!("pwdir: cache status:    {cache_status}");
        match &dirs_root {
            Some(root) => {
                println!("pwdir: storage allow:   {}", root.join("allow").display());
                println!("pwdir: storage cache:   {}", root.join("cache").display());
                println!("pwdir: storage state:   {}", root.join("state").display());
            }
            None => println!("pwdir: storage:         (unavailable)"),
        }
    }
    Ok(0)
}

/// `pwdir exec`（M7，SPEC §14.5）：以解析后的项目环境启动 COMMAND，原样转发
/// stdout/stderr 并以子进程退出码退出。错误经 exit_code_for 映射：
/// RcNotFound→2 / Io→3 / NotAllowed→1 / EvaluationFailed→1。
fn cmd_exec(
    dir: &Path,
    command: &str,
    args: &[String],
    require_rc: bool,
) -> Result<i32, PwDirError> {
    let outcome = exec::run_exec(dir, command, args, require_rc, &EvaluatorConfig::default())?;
    // 字节级透传，不追加换行（子进程输出保持原样）
    {
        let mut stdout = io::stdout().lock();
        stdout.write_all(outcome.stdout.as_bytes())?;
        stdout.flush()?;
    }
    {
        let mut stderr = io::stderr().lock();
        stderr.write_all(outcome.stderr.as_bytes())?;
        stderr.flush()?;
    }
    Ok(outcome.exit_code.unwrap_or(1))
}

/// `pwdir hook pwsh`（SPEC §14.6）：输出两行极小 bootstrap。模块路径解析失败时
/// 输出占位符片段并打 Warn —— 用户能立刻看到 snippet 形态，同时知道需要先装模块。
fn cmd_hook_pwsh() -> Result<i32> {
    let (module_display, resolved) = match doctor::module_manifest_path() {
        Some(path) => (path.display().to_string(), true),
        None => (
            "<PwDirEnv.psd1 path not found; run packaging/install.ps1 first>".to_string(),
            false,
        ),
    };
    let block = hook::hook_block_lines(&module_display);
    // hook 块去掉 marker 后正是两行 bootstrap
    for line in &block[1..block.len() - 1] {
        println!("{line}");
    }
    if !resolved {
        eprintln!(
            "pwdir: warning: PwDirEnv.psd1 not found next to pwdir.exe; the snippet above \
             contains a placeholder path"
        );
    }
    Ok(0)
}

/// hook install / uninstall 共用的目标文件解析：`--path` 覆盖默认的
/// `$PROFILE.CurrentUserAllHosts.Path`（doctor 与 hook 共用同一查询）。
fn resolve_hook_target(path: Option<&Path>) -> Result<PathBuf> {
    match path {
        Some(p) => Ok(p.to_path_buf()),
        None => doctor::current_user_all_hosts_profile().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve $PROFILE.CurrentUserAllHosts (pwsh unavailable?); \
                 pass --path <file> explicitly"
            )
        }),
    }
}

/// `pwdir hook install`：往目标 profile 写 marker 块（幂等）。未给 --yes 且 stdin 是
/// tty 时先确认；非交互（管道/CI）直接写，避免脚本化安装被挂起。
/// 文件 IO 失败 → PwDirError::Io → exit 3；目标解析失败 → 通用错误 exit 1。
fn cmd_hook_install(yes: bool, path: Option<&Path>) -> Result<i32> {
    let target = resolve_hook_target(path)?;
    if !yes && io::stdin().is_terminal() {
        print!("pwdir: 将修改 {}，继续? (y/N) ", target.display());
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            println!("pwdir: aborted");
            return Ok(0);
        }
    }
    let module_display = match doctor::module_manifest_path() {
        Some(path) => path.display().to_string(),
        None => "<PwDirEnv.psd1 path not found; run packaging/install.ps1 first>".to_string(),
    };
    let block = hook::hook_block_lines(&module_display);
    match hook::install_to_file(&target, &block) {
        Ok(true) => println!("pwdir: hook installed in {}", target.display()),
        Ok(false) => println!("pwdir: already installed in {}", target.display()),
        Err(e) => return Err(PwDirError::Io(e).into()),
    }
    Ok(0)
}

/// `pwdir hook uninstall`：按 marker 移除块（幂等；无块时 nothing to uninstall）。
/// 文件 IO 失败 → exit 3。
fn cmd_hook_uninstall(path: Option<&Path>) -> Result<i32> {
    let target = resolve_hook_target(path)?;
    match hook::uninstall_from_file(&target) {
        Ok(true) => println!("pwdir: hook removed from {}", target.display()),
        Ok(false) => println!("pwdir: nothing to uninstall"),
        Err(e) => return Err(PwDirError::Io(e).into()),
    }
    Ok(0)
}

/// `pwdir doctor`（M7，SPEC §14.8）：逐项输出 `[OK]/[WARN]/[FAIL]`，
/// `--json` 输出稳定 schema；存在 Fail 项 → exit 1，否则 0。
fn cmd_doctor(json: bool) -> Result<i32> {
    let checks = doctor::diagnose();
    let all_ok = doctor::is_all_ok(&checks);
    if json {
        let payload = serde_json::json!({
            "checks": checks
                .iter()
                .map(|c| serde_json::json!({
                    "name": c.name,
                    "status": c.status.as_str(),
                    "message": c.message,
                }))
                .collect::<Vec<_>>(),
            "all_ok": all_ok,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        for check in &checks {
            let tag = match check.status {
                CheckStatus::Ok => "[OK]",
                CheckStatus::Warn => "[WARN]",
                CheckStatus::Fail => "[FAIL]",
            };
            println!("{tag} {}: {}", check.name, check.message);
        }
    }
    Ok(if all_ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_spec_14_1() {
        assert_eq!(exit_code_for(&PwDirError::RcNotFound("x".into())), 2);
        assert_eq!(
            exit_code_for(&PwDirError::Io(std::io::Error::other("boom"))),
            3
        );
        assert_eq!(exit_code_for(&PwDirError::NotImplemented("x".into())), 1);
    }

    #[test]
    fn resolve_rc_uses_explicit_file_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = tmp.path().join(".envrc.ps1");
        std::fs::write(&rc, b"$env:A = '1'").expect("write rc");

        let found = resolve_rc(Some(&rc)).expect("resolve rc");
        assert_eq!(found.rc_path, rc);
        assert_eq!(found.project_root, tmp.path());
    }

    #[test]
    fn resolve_rc_directory_without_rc_is_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = resolve_rc(Some(tmp.path())).expect_err("must be RcNotFound");
        assert!(matches!(err, PwDirError::RcNotFound(_)));
    }
}
