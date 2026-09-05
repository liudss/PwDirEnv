//! doctor 环境自检（M7，SPEC §14.8）。
//!
//! 原则：**单项失败不中断**——每项检查内部 catch 自己的错误并降级为 Warn/Fail，
//! `diagnose()` 永不 panic、永不向上传播错误。检查覆盖：
//! OS/pwsh 存在性、PowerShell 版本（目标 ≥ 7.2）、module 安装、profile hook、
//! 数据目录可写性、temp 目录、ExecutionPolicy、长路径注册表开关。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::dirs;

/// 单项诊断结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

/// 检查结论。`Warn` 不影响 `all_ok`，只有 `Fail` 会让 doctor 以非 0 退出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    /// 人类可读 / CLI 标签形态（`[OK]` / `[WARN]` / `[FAIL]` 用大写另行格式化）。
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

/// 全部没有 Fail 项即视为环境可用（Warn 只是提示，不阻断）。
pub fn is_all_ok(checks: &[Check]) -> bool {
    !checks.iter().any(|c| matches!(c.status, CheckStatus::Fail))
}

/// 运行全部诊断项。任何单项的内部失败都被该项自身吸收为 Warn/Fail。
pub fn diagnose() -> Vec<Check> {
    let mut checks = Vec::with_capacity(9);
    checks.push(check_os());
    // pwsh 版本只探测一次，供 pwsh 存在性与版本两项检查共用（每次 spawn pwsh ≈ 0.5s）
    let version = pwsh_version();
    checks.push(check_pwsh(&version));
    checks.push(check_powershell_version(&version));
    checks.push(check_module());
    checks.push(check_profile_hook());
    checks.push(check_localappdata());
    checks.push(check_temp_dir());
    checks.push(check_execution_policy());
    checks.push(check_long_path());
    checks
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 运行外部命令并返回成功时的 stdout（trim 后）。失败 / 非 0 退出 → None。
/// stdin 显式置 null：防止诊断命令意外等待输入挂起。
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// 探测目录可写性：写入 + 删除探针文件。探针名带 pid+纳秒避免并发 doctor 互相覆盖。
fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(
        ".pwdir-doctor-probe-{}",
        crate::trust::random_suffix()
    ));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 查询 pwsh 版本字符串；Err 携带面向用户的诊断句。
fn pwsh_version() -> Result<String, String> {
    match command_output(
        "pwsh",
        &[
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ],
    ) {
        Some(v) if !v.is_empty() => Ok(v),
        Some(_) => Err("pwsh produced no version output".to_string()),
        None => Err(
            "pwsh was not found on PATH; install PowerShell 7+ (winget install Microsoft.PowerShell)"
                .to_string(),
        ),
    }
}

/// 定位 PwDirEnv.psd1（doctor module 检查与 `pwdir hook` 共用），按顺序探测：
/// 1. `<exe_dir>\modules\PwDirEnv\` —— 平铺布局（pwdir.exe 与 modules\ 同目录）；
/// 2. `<exe_dir>\..\modules\PwDirEnv\` —— **packaging/install.ps1 的标准布局**
///    （`<InstallRoot>\bin\pwdir.exe` 与 `<InstallRoot>\modules\` 是兄弟目录）；
/// 3. `<exe_dir>\..\..\powershell\PwDirEnv\` —— 开发布局（pwdir.exe 在 target\debug 下）。
///
/// 返回的路径已做词法清理（无 `..` 残留），可直接用于展示与生成 hook 片段。
pub fn module_manifest_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let candidates = [
        exe_dir.join(r"modules\PwDirEnv\PwDirEnv.psd1"),
        exe_dir.join(r"..\modules\PwDirEnv\PwDirEnv.psd1"),
        exe_dir.join(r"..\..\powershell\PwDirEnv\PwDirEnv.psd1"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| lexical_clean(&p))
}

/// 词法清理路径中的 `.` / `..` 组件。不用 `fs::canonicalize`：它会解析 symlink
/// 并追加 `\\?\` verbatim 前缀，展示与互操作都不友好；这里只是纯字符串层净化。
/// `..` 只在栈顶为普通组件时回退（前缀/根目录不回退，防御畸形输入）。
fn lexical_clean(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let top_is_normal =
                    matches!(out.components().next_back(), Some(Component::Normal(_)));
                if top_is_normal {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 通过 pwsh 查询 `$PROFILE.CurrentUserAllHosts`（`pwdir hook install` 的默认
/// 目标，doctor 的 profile-hook 检查共用）。查询失败 / 输出为空 → None。
///
/// 注意查询的是属性本身：`$PROFILE.CurrentUserAllHosts` 已经是路径字符串
/// （实测 pwsh 7.6），再取 `.Path` 会得到 null（空输出）——不能按直觉写
/// `$PROFILE.CurrentUserAllHosts.Path`。
pub fn current_user_all_hosts_profile() -> Option<PathBuf> {
    let raw = command_output(
        "pwsh",
        &[
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PROFILE.CurrentUserAllHosts",
        ],
    )?;
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

// ---------------------------------------------------------------------------
// 各检查项
// ---------------------------------------------------------------------------

fn check_os() -> Check {
    let base = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
    // Windows 上 best-effort 追加 registry 版本号（Win10 20H2+ / Win11 的 DisplayVersion）；
    // 读不到（旧系统 / 非管理员受限）不构成任何失败
    #[cfg(windows)]
    if let Some(out) = command_output(
        "reg.exe",
        &[
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "/v",
            "DisplayVersion",
        ],
    ) {
        if let Some(value) = out
            .lines()
            .find(|l| l.contains("DisplayVersion"))
            .and_then(|l| l.split_whitespace().last())
        {
            return Check {
                name: "os".to_string(),
                status: CheckStatus::Ok,
                message: format!("{base}, Windows version {value}"),
            };
        }
    }
    Check {
        name: "os".to_string(),
        status: CheckStatus::Ok,
        message: base,
    }
}

fn check_pwsh(version: &Result<String, String>) -> Check {
    match version {
        Ok(v) => Check {
            name: "pwsh".to_string(),
            status: CheckStatus::Ok,
            message: format!("PowerShell {v} found on PATH"),
        },
        Err(e) => Check {
            name: "pwsh".to_string(),
            status: CheckStatus::Fail,
            message: e.clone(),
        },
    }
}

fn check_powershell_version(version: &Result<String, String>) -> Check {
    let name = "powershell-version";
    let Ok(v) = version else {
        return Check {
            name: name.to_string(),
            status: CheckStatus::Fail,
            message: "PowerShell version unknown (pwsh check failed)".to_string(),
        };
    };
    let major = v.split('.').next().and_then(|p| p.parse::<u32>().ok());
    let minor = v.split('.').nth(1).and_then(|p| p.parse::<u32>().ok());
    match (major, minor) {
        // Windows PowerShell 5.1 明确不支持（SPEC §24.5）
        (Some(m), _) if m < 7 => Check {
            name: name.to_string(),
            status: CheckStatus::Fail,
            message: format!("PowerShell {v} is not supported; PwDirEnv requires PowerShell 7.2+"),
        },
        // 7.0/7.1 可运行但低于目标版本（SPEC §14.8：目标 >= 7.2）
        (Some(7), Some(mi)) if mi < 2 => Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: format!(
                "PowerShell {v} works but 7.2+ is recommended (target >= 7.2, recommended >= 7.4)"
            ),
        },
        _ => Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!("PowerShell {v}"),
        },
    }
}

fn check_module() -> Check {
    let name = "module";
    match module_manifest_path() {
        Some(p) => Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!("module manifest found at {}", p.display()),
        },
        None => Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: "PwDirEnv.psd1 not found next to pwdir.exe (expected \
                      modules\\PwDirEnv\\, ..\\modules\\PwDirEnv\\ or \
                      ..\\..\\powershell\\PwDirEnv\\); run packaging/install.ps1 or \
                      Import-Module with an explicit path"
                .to_string(),
        },
    }
}

fn check_profile_hook() -> Check {
    let name = "profile-hook";
    // CurrentUserAllHosts = `profile.ps1`：对 pwsh 全部 host 生效的持久化 hook 位置
    let Some(path) = current_user_all_hosts_profile() else {
        return Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: "cannot resolve $PROFILE.CurrentUserAllHosts (pwsh unavailable?)".to_string(),
        };
    };
    if !path.is_file() {
        return Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: format!(
                "profile {} does not exist; run `pwdir hook install` to create it",
                path.display()
            ),
        };
    }
    match fs::read_to_string(&path) {
        Ok(content) if content.contains("PwDirEnv") => Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!("PwDirEnv hook present in {}", path.display()),
        },
        Ok(_) => Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: format!(
                "{} exists but has no PwDirEnv hook block; run `pwdir hook install`",
                path.display()
            ),
        },
        Err(e) => Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: format!("cannot read {}: {e}", path.display()),
        },
    }
}

fn check_localappdata() -> Check {
    let name = "localappdata";
    match dirs::pwdir_data_root() {
        Err(e) => Check {
            name: name.to_string(),
            status: CheckStatus::Fail,
            message: format!("cannot resolve PwDirEnv data root: {e}"),
        },
        Ok(root) => {
            // 逐个子目录写删探针：trust/cache/state 任一不可写都会破坏核心功能
            for sub in ["allow", "cache", "state"] {
                let dir = root.join(sub);
                if !dir_is_writable(&dir) {
                    return Check {
                        name: name.to_string(),
                        status: CheckStatus::Fail,
                        message: format!("{} is not writable", dir.display()),
                    };
                }
            }
            Check {
                name: name.to_string(),
                status: CheckStatus::Ok,
                message: format!(
                    "data root {} is writable (allow/cache/state)",
                    root.display()
                ),
            }
        }
    }
}

fn check_temp_dir() -> Check {
    let name = "temp-dir";
    let dir = std::env::temp_dir();
    if dir_is_writable(&dir) {
        Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!("{} is writable", dir.display()),
        }
    } else {
        Check {
            name: name.to_string(),
            status: CheckStatus::Fail,
            message: format!(
                "{} is not writable; evaluator result channel needs it",
                dir.display()
            ),
        }
    }
}

fn check_execution_policy() -> Check {
    let name = "execution-policy";
    let Some(policy) = command_output(
        "pwsh",
        &[
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "Get-ExecutionPolicy -Scope Process",
        ],
    ) else {
        return Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: "cannot query execution policy (pwsh unavailable?)".to_string(),
        };
    };
    match policy.as_str() {
        "Bypass" | "Unrestricted" | "RemoteSigned" => Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!("execution policy (Process scope): {policy}"),
        },
        "Restricted" | "AllSigned" => Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            // evaluator 用 `-ExecutionPolicy Bypass` 启动，仅作用于自身进程，
            // 不修改 Machine/User policy（SPEC §7.2）；但更严格的 Machine/User
            // 级策略可能覆盖进程级 Bypass —— 此时需管理员处理
            message: format!(
                "execution policy (Process scope) is {policy}; the evaluator still runs with \
                 -ExecutionPolicy Bypass in its own process, but stricter Machine/User policy \
                 may override it"
            ),
        },
        // Undefined = Process scope 未显式设置，生效策略由其它 scope 决定；
        // evaluator 自带 -ExecutionPolicy Bypass 参数，此状态不构成阻断
        other => Check {
            name: name.to_string(),
            status: CheckStatus::Ok,
            message: format!(
                "execution policy (Process scope): {other} (unset here; evaluator runs with \
                 -ExecutionPolicy Bypass)"
            ),
        },
    }
}

fn check_long_path() -> Check {
    let name = "long-path";
    #[cfg(windows)]
    {
        let Some(out) = command_output(
            "reg.exe",
            &[
                "query",
                r"HKLM\SYSTEM\CurrentControlSet\Control\FileSystem",
                "/v",
                "LongPathsEnabled",
            ],
        ) else {
            return Check {
                name: name.to_string(),
                status: CheckStatus::Warn,
                message: "cannot read LongPathsEnabled from the registry".to_string(),
            };
        };
        // reg 输出形如 "    LongPathsEnabled    REG_DWORD    0x1"：取行内最后 token 按 hex 解析
        let value = out
            .lines()
            .find(|l| l.contains("LongPathsEnabled"))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|tok| u32::from_str_radix(tok.trim_start_matches("0x"), 16).ok());
        match value {
            Some(1) => Check {
                name: name.to_string(),
                status: CheckStatus::Ok,
                message: "LongPathsEnabled=1 (long paths enabled)".to_string(),
            },
            Some(_) => Check {
                name: name.to_string(),
                status: CheckStatus::Warn,
                message: "LongPathsEnabled=0; paths longer than 260 chars may fail \
                          (enable via registry or group policy)"
                    .to_string(),
            },
            None => Check {
                name: name.to_string(),
                status: CheckStatus::Warn,
                message: "cannot parse LongPathsEnabled registry value".to_string(),
            },
        }
    }
    #[cfg(not(windows))]
    {
        Check {
            name: name.to_string(),
            status: CheckStatus::Warn,
            message: "not applicable on non-Windows platforms".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnose_returns_core_checks_without_panicking() {
        let checks = diagnose();
        // 至少包含 SPEC §14.8 的核心三项；全部检查都有非空 name/message
        for required in ["os", "pwsh", "temp-dir"] {
            assert!(
                checks.iter().any(|c| c.name == required),
                "diagnose() must include the '{required}' check, got: {:?}",
                checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
        }
        assert!(checks.iter().all(|c| !c.name.is_empty()));
        assert!(checks.iter().all(|c| !c.message.is_empty()));
        // 名字不重复
        let mut names: Vec<_> = checks.iter().map(|c| c.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), checks.len(), "check names must be unique");
    }

    #[test]
    fn is_all_ok_ignores_warns_but_not_fails() {
        assert!(is_all_ok(&[]), "empty checks are vacuously ok");

        let warns = vec![
            Check {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "m".into(),
            },
            Check {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "m".into(),
            },
        ];
        assert!(is_all_ok(&warns), "Warn must not fail all_ok");

        let with_fail = vec![Check {
            name: "c".into(),
            status: CheckStatus::Fail,
            message: "m".into(),
        }];
        assert!(!is_all_ok(&with_fail), "Fail must fail all_ok");
    }

    #[cfg(windows)]
    #[test]
    fn os_check_reports_windows_with_arch() {
        let check = check_os();
        assert_eq!(check.name, "os");
        assert_eq!(check.status, CheckStatus::Ok);
        assert!(check.message.contains(std::env::consts::OS));
        assert!(check.message.contains(std::env::consts::ARCH));
    }
}
