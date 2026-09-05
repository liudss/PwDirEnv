//! export 编排（M3，SPEC §14.7）：`pwdir export --format json` 的核心协议入口。
//!
//! 流程（严格顺序）：
//! 1. 捕获 baseline = `EnvironmentSnapshot::from_process()` —— pwdir.exe 是 Host pwsh 的
//!    直接子进程，其继承环境即 Host 当前环境，与 evaluator 继承的一致（SPEC §8.1/§7.3）；
//! 2. 向上发现 RC：无 RC → `Noop`（module 自己判断要不要 unload 旧环境）；
//! 3. 有 RC → trust 校验：未授权 → `Blocked`（patch 恒为空，RC 内容绝不执行）；
//! 4. 已授权 → evaluator 隔离评估：
//!    - 成功 → `patch = diff_env(baseline, desired)`，非空则 `Apply` 否则 `Noop`；
//!    - RC 失败/超时 → `Error`（协议语义，不是 CLI 失败）；
//!    - 基础设施故障（pwsh 缺失等）→ `Err` 向上传播，由 CLI 按退出码映射。

use std::collections::BTreeSet;
use std::path::Path;

use pwdir_protocol::{Action, Diagnostic, EnvironmentSnapshot, ExportOutput, PROTOCOL_SCHEMA};

use crate::diff::diff_env;
use crate::discovery;
use crate::evaluator::{self, EvaluatorConfig};
use crate::trust::TrustStore;
use crate::PwDirError;

/// 用默认 trust store（`%LOCALAPPDATA%\PwDirEnv\allow`）执行一次 export。
pub fn export(cwd: &Path, cfg: &EvaluatorConfig) -> Result<ExportOutput, PwDirError> {
    let store = TrustStore::open()?;
    export_with_store(cwd, cfg, &store)
}

/// 用注入的 trust store 执行一次 export（测试可注入临时 allow 目录）。
pub fn export_with_store(
    cwd: &Path,
    cfg: &EvaluatorConfig,
    store: &TrustStore,
) -> Result<ExportOutput, PwDirError> {
    // 1. baseline：export 时点进程 env（与 evaluator 继承的一致）
    let baseline = EnvironmentSnapshot::from_process();

    // cwd 不可用（目录被删除，CLI 传空路径）时不 panic：按无 RC 处理并给 diagnostic（SPEC §20.4）
    if cwd.as_os_str().is_empty() {
        return Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Noop,
            rc: None,
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "warn".to_string(),
                message: "current directory is unavailable; treated as no rc".to_string(),
            }],
        });
    }

    // 2. 发现 RC：None → Noop（module 侧决定是否 unload 旧环境）
    let Some(rc) = discovery::find_rc(cwd)? else {
        return Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Noop,
            rc: None,
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![],
        });
    };

    // 3. trust 校验：blocked 时 patch 恒为空 —— RC 内容一行都不执行（安全边界 > 一切）
    if !store.is_allowed(&rc.rc_path)? {
        return Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Blocked,
            rc: Some(rc.rc_path),
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "warn".to_string(),
                message: "rc file is blocked; run `pwdir allow`".to_string(),
            }],
        });
    }

    // 4. 隔离评估。Err（基础设施故障）直接向上传播，CLI 按退出码映射处理
    let report = evaluator::evaluate(cfg, &rc)?;

    match (report.result, report.error) {
        // 成功 → diff 出 patch；fingerprint 留待 M6 填充
        (Some(result), _) => {
            let desired = result.environment;
            let patch = diff_env(&baseline, &desired);
            // managed_keys = set.keys ∪ unset，排序去重 —— PwDirEnv "owns" 的 key 集合（SPEC §8.5）
            let mut managed: BTreeSet<String> = patch.set.keys().cloned().collect();
            managed.extend(patch.unset.iter().cloned());
            let action = if patch.is_empty() {
                Action::Noop
            } else {
                Action::Apply
            };
            Ok(ExportOutput {
                schema: PROTOCOL_SCHEMA,
                action,
                rc: Some(rc.rc_path),
                patch,
                managed_keys: managed.into_iter().collect(),
                fingerprint: None,
                diagnostics: vec![],
            })
        }
        // RC 失败/超时：结构化协议语义（action=error），patch 恒为空
        (None, Some(error)) => Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Error,
            rc: Some(rc.rc_path),
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "error".to_string(),
                message: error.message,
            }],
        }),
        // 防御分支：evaluator 契约保证 result/error 至少有一个；真出现时按 error 呈现而非 panic
        (None, None) => Ok(ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Error,
            rc: Some(rc.rc_path),
            patch: Default::default(),
            managed_keys: vec![],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "error".to_string(),
                message: "evaluator produced neither a result nor an error".to_string(),
            }],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::OnceLock;

    use pwdir_protocol::RC_FILENAME;

    /// pwsh 可用性只探测一次（spawn pwsh 成本高）。
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
            "pwsh (PowerShell 7+) was not found on PATH; it is required for export tests"
        );
    }

    /// 注入临时 allow 目录的 store，不碰真实 LOCALAPPDATA。
    fn temp_store() -> (TrustStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::new(tmp.path().join("allow"));
        (store, tmp)
    }

    /// 写入 `.envrc.ps1` 并返回 rc 路径。
    fn write_rc(dir: &Path, body: &str) -> PathBuf {
        let rc = dir.join(RC_FILENAME);
        fs::write(&rc, body).expect("write rc fixture");
        rc
    }

    #[test]
    fn export_applies_allowed_rc() {
        ensure_pwsh();
        // 保证 baseline 里没有 FOO，set 语义确定
        std::env::remove_var("FOO");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:FOO = 'bar'");
        let (store, _allow) = temp_store();
        store.allow(&rc).expect("allow");

        let out =
            export_with_store(tmp.path(), &EvaluatorConfig::default(), &store).expect("export ok");

        assert_eq!(out.schema, PROTOCOL_SCHEMA);
        assert_eq!(out.action, Action::Apply);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert_eq!(out.patch.set.get("FOO").map(String::as_str), Some("bar"));
        assert!(out.patch.unset.is_empty());
        assert!(
            out.managed_keys.iter().any(|k| k == "FOO"),
            "managed_keys must contain FOO: {:?}",
            out.managed_keys
        );
        assert!(out.fingerprint.is_none(), "fingerprint is filled in M6");
        std::env::remove_var("FOO");
    }

    #[test]
    fn no_rc_yields_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, _allow) = temp_store();

        let out =
            export_with_store(tmp.path(), &EvaluatorConfig::default(), &store).expect("export ok");

        assert_eq!(out.action, Action::Noop);
        assert!(out.rc.is_none());
        assert!(out.patch.is_empty());
        assert!(out.managed_keys.is_empty());
    }

    #[test]
    fn unavailable_cwd_yields_noop_with_diagnostic() {
        // CLI 在 current_dir 失败时传空路径：不 panic，返回 Noop + diagnostic
        let (store, _allow) = temp_store();
        let out = export_with_store(Path::new(""), &EvaluatorConfig::default(), &store)
            .expect("export ok");

        assert_eq!(out.action, Action::Noop);
        assert!(out.rc.is_none());
        assert!(out.patch.is_empty());
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.message.contains("current directory is unavailable")),
            "expected an unavailable-cwd diagnostic: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn unallowed_rc_is_blocked_with_empty_patch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:FOO = 'bar'");
        let (store, _allow) = temp_store();
        // 没有 allow → blocked

        let out =
            export_with_store(tmp.path(), &EvaluatorConfig::default(), &store).expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert!(out.patch.is_empty(), "blocked must never carry a patch");
        assert!(out.managed_keys.is_empty());
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.level == "warn" && d.message.contains("pwdir allow")),
            "expected a blocked warning diagnostic: {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn rc_byte_change_after_allow_is_blocked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:FOO = 'bar'");
        let (store, _allow) = temp_store();
        store.allow(&rc).expect("allow");

        // 改一个 byte → trust 失效 → blocked（内容 hash，非 mtime）
        fs::write(&rc, "$env:FOO = 'baz'").expect("rewrite rc");
        let out =
            export_with_store(tmp.path(), &EvaluatorConfig::default(), &store).expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert!(out.patch.is_empty());
    }

    #[test]
    fn throwing_rc_yields_error_action() {
        ensure_pwsh();
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "throw 'boom-export'");
        let (store, _allow) = temp_store();
        store.allow(&rc).expect("allow");

        let out =
            export_with_store(tmp.path(), &EvaluatorConfig::default(), &store).expect("export ok");

        assert_eq!(out.action, Action::Error);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert!(out.patch.is_empty(), "error must never carry a patch");
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.level == "error" && d.message.contains("boom-export")),
            "expected the evaluation error as diagnostic: {:?}",
            out.diagnostics
        );
    }
}
