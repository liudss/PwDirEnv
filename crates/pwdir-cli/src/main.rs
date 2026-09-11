//! CLI argument parsing and routing. Protocol actions are separate from process exit codes.

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
    Export {
        /// Output format
        #[arg(long, default_value = "json")]
        format: String,
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
        PwDirCli::command().print_help()?;
        return Ok(0);
    };

    match command {
        Commands::Export { format, force } => {
            if format != "json" {
                anyhow::bail!("unsupported export format: {format} (only 'json' is supported)");
            }
            let cwd = discovery::current_dir_fs_or_none().unwrap_or_default();
            let output = pwdir_core::export::export(&cwd, &EvaluatorConfig::default(), force)?;
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(0)
        }
        Commands::Allow { path } => Ok(cmd_allow(path.as_deref())?),
        Commands::Deny { path } => Ok(cmd_deny(path.as_deref())?),
        Commands::Status { json } => Ok(cmd_status(json)?),
        Commands::Reload => Ok(cmd_reload()?),
        Commands::Exec {
            dir,
            command,
            args,
            require_rc,
        } => Ok(cmd_exec(&dir, &command, &args, require_rc)?),
        Commands::Hook { action } => match action {
            HookAction::Pwsh => Ok(cmd_hook_pwsh()?),
            HookAction::Install { yes, path } => Ok(cmd_hook_install(yes, path.as_deref())?),
            HookAction::Uninstall { path } => Ok(cmd_hook_uninstall(path.as_deref())?),
        },
        Commands::Doctor { json } => Ok(cmd_doctor(json)?),
    }
}

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
    let discovered = cwd
        .as_deref()
        .and_then(|dir| discovery::find_rc(dir).ok().flatten());

    let (trust_status, rc_sha256) = match &discovered {
        None => ("none".to_string(), None),
        Some(rc) => {
            let store = TrustStore::open()?;
            match store.record(&rc.rc_path)? {
                None => ("none".to_string(), None),
                Some(record) => {
                    let current = trust::file_sha256(&rc.rc_path).ok();
                    let matched = current.as_deref() == Some(record.sha256.as_str());
                    let status = if matched { "allowed" } else { "blocked" };
                    (status.to_string(), current)
                }
            }
        }
    };

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

fn cmd_exec(
    dir: &Path,
    command: &str,
    args: &[String],
    require_rc: bool,
) -> Result<i32, PwDirError> {
    let outcome = exec::run_exec(dir, command, args, require_rc, &EvaluatorConfig::default())?;
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

fn cmd_hook_pwsh() -> Result<i32> {
    let (module_display, resolved) = match doctor::module_manifest_path() {
        Some(path) => (path.display().to_string(), true),
        None => (
            "<PwDirEnv.psd1 path not found; run packaging/install.ps1 first>".to_string(),
            false,
        ),
    };
    let block = hook::hook_block_lines(&module_display);
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

fn cmd_hook_install(yes: bool, path: Option<&Path>) -> Result<i32> {
    let target = resolve_hook_target(path)?;
    if !yes && io::stdin().is_terminal() {
        print!("pwdir: modify {}? (y/N) ", target.display());
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

fn cmd_hook_uninstall(path: Option<&Path>) -> Result<i32> {
    let target = resolve_hook_target(path)?;
    match hook::uninstall_from_file(&target) {
        Ok(true) => println!("pwdir: hook removed from {}", target.display()),
        Ok(false) => println!("pwdir: nothing to uninstall"),
        Err(e) => return Err(PwDirError::Io(e).into()),
    }
    Ok(0)
}

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
