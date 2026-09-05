//! pwdir.exe：CLI 解析与命令路由（PLAN §1.1）。命令实现分布在 pwdir-core，
//! 本 crate 不重复实现 core 逻辑。

use std::path::PathBuf;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use pwdir_protocol::{Action, EnvPatch, ExportOutput, PROTOCOL_SCHEMA};

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
        /// Path to the RC file; defaults to the RC for the current directory
        path: Option<PathBuf>,
    },
    /// Revoke the authorization of an RC file
    Deny {
        /// Path to the RC file; defaults to the RC for the current directory
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
        /// Command arguments
        args: Vec<String>,
    },
    /// Export the environment patch (Host module protocol entry, SPEC §14.7)
    Export {
        /// Output format
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Print the shell hook bootstrap snippet
    Hook {
        /// Target shell (currently: pwsh)
        shell: String,
    },
    /// Diagnose the environment (pwsh version, trust/cache dirs, ExecutionPolicy, ...)
    Doctor,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            // 退出码约定（SPEC §14.1）：0 成功、2 RC 找不到、3 IO error；未实现先用 1
            eprintln!("pwdir: error: {err:#}");
            1
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
        // M3 对接前的占位输出：始终是一个合法的 schema-1 JSON（action=noop，空 patch），
        // 保证 PowerShell module 可以立即按协议解析
        Commands::Export { format } => {
            if format != "json" {
                anyhow::bail!("unsupported export format: {format} (only 'json' is supported)");
            }
            let output = ExportOutput {
                schema: PROTOCOL_SCHEMA,
                action: Action::Noop,
                rc: None,
                patch: EnvPatch::default(),
                managed_keys: vec![],
                fingerprint: None,
                diagnostics: vec![],
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(0)
        }
        Commands::Allow { .. } => Err(anyhow::anyhow!("not implemented yet (milestone 1)")),
        Commands::Deny { .. } => Err(anyhow::anyhow!("not implemented yet (milestone 1)")),
        Commands::Status { .. } => Err(anyhow::anyhow!("not implemented yet (milestone 1)")),
        Commands::Reload => Err(anyhow::anyhow!("not implemented yet (milestone 6)")),
        Commands::Exec { .. } => Err(anyhow::anyhow!("not implemented yet (milestone 7)")),
        Commands::Hook { .. } => Err(anyhow::anyhow!("not implemented yet (milestone 4)")),
        Commands::Doctor => Err(anyhow::anyhow!("not implemented yet (milestone 7)")),
    }
}
