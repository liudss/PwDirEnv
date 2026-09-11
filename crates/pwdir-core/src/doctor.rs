//! Installation diagnostics. Each check reports its own failure; warnings do not make the overall result fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::dirs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

pub fn is_all_ok(checks: &[Check]) -> bool {
    !checks.iter().any(|c| matches!(c.status, CheckStatus::Fail))
}

pub fn diagnose() -> Vec<Check> {
    let mut checks = Vec::with_capacity(9);
    checks.push(check_os());
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

fn check_os() -> Check {
    let base = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
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
        (Some(m), _) if m < 7 => Check {
            name: name.to_string(),
            status: CheckStatus::Fail,
            message: format!("PowerShell {v} is not supported; PwDirEnv requires PowerShell 7.2+"),
        },
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
            message: format!(
                "execution policy (Process scope) is {policy}; the evaluator still runs with \
                 -ExecutionPolicy Bypass in its own process, but stricter Machine/User policy \
                 may override it"
            ),
        },
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
        for required in ["os", "pwsh", "temp-dir"] {
            assert!(
                checks.iter().any(|c| c.name == required),
                "diagnose() must include the '{required}' check, got: {:?}",
                checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
        }
        assert!(checks.iter().all(|c| !c.name.is_empty()));
        assert!(checks.iter().all(|c| !c.message.is_empty()));
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
