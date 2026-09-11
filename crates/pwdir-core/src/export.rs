//! export 编排（M3 + M6，SPEC §14.7/§12.5）：`pwdir export --format json` 的核心协议入口。
//!
//! 流程（严格顺序）：
//! 1. 延迟到完整评估前捕获 baseline；无 RC、blocked 和缓存命中无需复制环境。
//!    pwdir.exe 继承的环境与 evaluator 一致（SPEC §8.1/§7.3）；
//! 2. 向上发现 RC：无 RC → `Noop`（module 自己判断要不要 unload 旧环境）；
//! 3. 有 RC → trust 校验：未授权 → `Blocked`（patch 恒为空，RC 内容绝不执行）；
//! 4. 已授权 → **fingerprint 快速路径（M6）**：未传 force 且磁盘 cache 的指纹仍与当前
//!    现实一致 → 直接返回 `Noop`（不 spawn evaluator，patch 恒为空）—— 这是每 prompt
//!    的稳态，P95 目标 < 20ms（SPEC §22）；
//! 5. 快速路径未命中（首次评估 / RC 或 watch/sourced 变化 / 版本变化 / force）→
//!    evaluator 隔离评估：
//!    - 成功 → 计算 fingerprint、原子写入 cache、`patch = diff_env(baseline, desired)`，
//!      非空则 `Apply` 否则 `Noop`；
//!    - RC 失败/超时 → `Error`（协议语义，不写 cache —— last-known-good 不被坏结果污染）；
//!    - 基础设施故障（pwsh 缺失等）→ `Err` 向上传播，由 CLI 按退出码映射。
//!
//! `force` 语义：跳过快速路径强制评估。Host module 在「rc 与 ActiveRc 不一致」
//! （新 shell 首次进入 / 切换目录）时必须传 force —— cache 是跨会话共享的，
//! 上一个会话留下的有效 cache 会令快速路径返回空 patch，而新 shell 的进程环境
//! 尚未加载该项目环境，不 force 会漏掉加载。

use std::collections::BTreeSet;
use std::path::Path;

use chrono::Utc;
use pwdir_protocol::{Action, Diagnostic, EnvironmentSnapshot, ExportOutput, PROTOCOL_SCHEMA};

use crate::cache::{CacheEntry, CacheStore, CACHE_SCHEMA};
use crate::diff::diff_env;
use crate::discovery;
use crate::evaluator::{self, EvaluatorConfig};
use crate::fingerprint::{compute_fingerprint, fingerprint_is_current};
use crate::trust::TrustStore;
use crate::PwDirError;

/// 用默认 trust store 与 cache store 执行一次 export。
pub fn export(cwd: &Path, cfg: &EvaluatorConfig, force: bool) -> Result<ExportOutput, PwDirError> {
    let store = TrustStore::open()?;
    let cache = CacheStore::open()?;
    export_with_store(cwd, cfg, &store, &cache, force)
}

/// 用注入的 trust / cache store 执行一次 export（测试可注入临时目录）。
pub fn export_with_store(
    cwd: &Path,
    cfg: &EvaluatorConfig,
    store: &TrustStore,
    cache: &CacheStore,
    force: bool,
) -> Result<ExportOutput, PwDirError> {
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
        return Ok(blocked_output(&rc.rc_path));
    }

    // 4. fingerprint 快速路径（M6）：trust 已通过，cache 指纹仍与现实一致 → noop 不 spawn。
    //    cache load / 判变的任何「未命中」都自然落入下方完整评估，安全降级。
    if !force {
        if let Some(entry) = cache.load(&rc.rc_path)? {
            if fingerprint_is_current(&entry.fingerprint, &rc.rc_path) {
                return Ok(ExportOutput {
                    schema: PROTOCOL_SCHEMA,
                    action: Action::Noop,
                    rc: Some(rc.rc_path),
                    patch: Default::default(),
                    managed_keys: vec![],
                    fingerprint: Some(entry.fingerprint),
                    diagnostics: vec![Diagnostic {
                        level: "info".to_string(),
                        message: "fingerprint unchanged (fast path)".to_string(),
                    }],
                });
            }
        }
    }

    // 5. 隔离评估。Err（基础设施故障）直接向上传播，CLI 按退出码映射处理
    // 只有完整评估需要 baseline，避免每次 prompt 的快速路径枚举、分配全部环境变量。
    let baseline = EnvironmentSnapshot::from_process();
    let versions = evaluator::embedded_script_versions();
    let report = evaluator::evaluate(cfg, &rc)?;

    // evaluator 运行期间 RC 可能被编辑或替换。此时 evaluation result 对应的是
    // 旧内容，绝不能以新内容的 fingerprint 写入 cache，更不能把旧 patch 应用到 Host。
    // 重新验证授权会同时校验当前 bytes 是否仍是 allow 时的内容。
    if !store.is_allowed(&rc.rc_path)? {
        return Ok(blocked_output(&rc.rc_path));
    }

    match (report.result, report.error) {
        // 成功 → 计算 fingerprint、写 cache（原子替换）、diff 出 patch
        (Some(result), _) => {
            let fingerprint = compute_fingerprint(&rc.rc_path, &result, &versions.0, &versions.1);
            cache.save(&CacheEntry {
                schema: CACHE_SCHEMA,
                rc: rc.rc_path.clone(),
                fingerprint: fingerprint.clone(),
                evaluated_at: Utc::now().to_rfc3339(),
            })?;

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
                fingerprint: Some(fingerprint),
                diagnostics: vec![],
            })
        }
        // RC 失败/超时：结构化协议语义（action=error），patch 恒为空；
        // 不写 cache —— 失败结果不能成为后续快速路径的依据（last-known-good，SPEC §20.2）
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

/// 未授权（或在 evaluation 期间发生内容变化）的 RC 统一返回安全的空 patch。
fn blocked_output(rc_path: &Path) -> ExportOutput {
    ExportOutput {
        schema: PROTOCOL_SCHEMA,
        action: Action::Blocked,
        rc: Some(rc_path.to_path_buf()),
        patch: Default::default(),
        managed_keys: vec![],
        fingerprint: None,
        diagnostics: vec![Diagnostic {
            level: "warn".to_string(),
            message: "rc file is blocked; run `pwdir allow`".to_string(),
        }],
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

    /// 注入临时 allow/cache 目录的 store 对，不碰真实 LOCALAPPDATA。
    fn temp_stores() -> (TrustStore, CacheStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::new(tmp.path().join("allow"));
        let cache = CacheStore::new(tmp.path().join("cache"));
        (store, cache, tmp)
    }

    /// 写入 `.envrc.ps1` 并返回 rc 路径。
    fn write_rc(dir: &Path, body: &str) -> PathBuf {
        let rc = dir.join(RC_FILENAME);
        fs::write(&rc, body).expect("write rc fixture");
        rc
    }

    #[test]
    fn export_applies_allowed_rc_and_caches_fingerprint() {
        ensure_pwsh();
        // 保证 baseline 里没有 PWDIR_M6_FOO，set 语义确定
        std::env::remove_var("PWDIR_M6_FOO");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.schema, PROTOCOL_SCHEMA);
        assert_eq!(out.action, Action::Apply);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert_eq!(
            out.patch.set.get("PWDIR_M6_FOO").map(String::as_str),
            Some("bar")
        );
        assert!(out.patch.unset.is_empty());
        assert!(
            out.managed_keys.iter().any(|k| k == "PWDIR_M6_FOO"),
            "managed_keys must contain PWDIR_M6_FOO: {:?}",
            out.managed_keys
        );
        // M6：fingerprint 已填充且 rc_sha256 与 trust 同源
        let fingerprint = out.fingerprint.expect("fingerprint present after M6");
        assert_eq!(
            fingerprint.rc_sha256,
            crate::trust::file_sha256(&rc).expect("rc hash")
        );
        // cache 已落盘，且能被快速路径判为「仍然新鲜」
        let entry = cache.load(&rc).expect("cache load").expect("cache entry");
        assert!(fingerprint_is_current(&entry.fingerprint, &rc));
        std::env::remove_var("PWDIR_M6_FOO");
    }

    #[test]
    fn no_rc_yields_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (store, cache, _guard) = temp_stores();

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Noop);
        assert!(out.rc.is_none());
        assert!(out.patch.is_empty());
        assert!(out.managed_keys.is_empty());
        assert!(out.fingerprint.is_none());
    }

    #[test]
    fn unavailable_cwd_yields_noop_with_diagnostic() {
        // CLI 在 current_dir 失败时传空路径：不 panic，返回 Noop + diagnostic
        let (store, cache, _guard) = temp_stores();
        let out = export_with_store(
            Path::new(""),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
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
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();
        // 没有 allow → blocked

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert_eq!(out.rc.as_deref(), Some(rc.as_path()));
        assert!(out.patch.is_empty(), "blocked must never carry a patch");
        assert!(out.managed_keys.is_empty());
        assert!(
            out.fingerprint.is_none(),
            "blocked must not carry a fingerprint"
        );
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
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_FOO = 'bar'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        // 改一个 byte → trust 失效 → blocked（内容 hash，非 mtime）
        fs::write(&rc, "$env:PWDIR_M6_FOO = 'baz'").expect("rewrite rc");
        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

        assert_eq!(out.action, Action::Blocked);
        assert!(out.patch.is_empty());
    }

    #[test]
    fn rc_changed_during_evaluation_is_blocked_without_caching_stale_result() {
        ensure_pwsh();
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc_path = tmp.path().join(RC_FILENAME);
        let rc = write_rc(
            tmp.path(),
            &format!(
                "$env:PWDIR_RACE_GUARD = 'old'\nSet-Content -LiteralPath '{}' -Value \"`$env:PWDIR_RACE_GUARD = 'new'\"\n",
                rc_path.display()
            ),
        );
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            true,
        )
        .expect("export returns protocol output");

        assert_eq!(out.action, Action::Blocked);
        assert!(
            out.patch.is_empty(),
            "stale evaluator patch must not escape"
        );
        assert!(out.fingerprint.is_none());
        assert!(cache.load(&rc).expect("cache load").is_none());
        std::env::remove_var("PWDIR_RACE_GUARD");
    }

    #[test]
    fn throwing_rc_yields_error_action_without_cache() {
        ensure_pwsh();
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "throw 'boom-export'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let out = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export ok");

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
        // 失败不写 cache：坏结果不能成为快速路径依据（last-known-good）
        assert!(cache.load(&rc).expect("cache load").is_none());
    }

    #[test]
    fn fast_path_returns_noop_without_reevaluating() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_FAST");
        let tmp = tempfile::tempdir().expect("tempdir");
        let marker = tmp.path().join("marker.txt");
        // rc 每次被执行都会以当前 UTC ticks 重写 marker —— ticks 变化即证明 evaluator 运行过
        let rc = write_rc(
            tmp.path(),
            &format!(
                "Set-Content -LiteralPath '{}' -Value ([DateTime]::UtcNow.Ticks)\n$env:PWDIR_M6_FAST = 'on'\n",
                marker.display()
            ),
        );
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let first = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert_eq!(first.action, Action::Apply);
        let ticks_after_first = fs::read_to_string(&marker).expect("marker after export 1");

        // 第二次（不 force）：快速路径 noop —— marker 未被重写 = evaluator 未运行
        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(second.action, Action::Noop);
        assert_eq!(second.rc.as_deref(), Some(rc.as_path()));
        assert!(second.patch.is_empty(), "fast path must not carry a patch");
        assert!(second.fingerprint.is_some());
        assert!(
            second
                .diagnostics
                .iter()
                .any(|d| d.level == "info" && d.message == "fingerprint unchanged (fast path)"),
            "expected fast path diagnostic: {:?}",
            second.diagnostics
        );
        assert_eq!(
            fs::read_to_string(&marker).expect("marker after export 2"),
            ticks_after_first,
            "evaluator must not have re-run on the fast path"
        );

        // force=true 跳过快速路径：重评估，marker 被重写
        let forced = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            true,
        )
        .expect("export 3 (forced)");
        assert_eq!(forced.action, Action::Apply);
        assert_ne!(
            fs::read_to_string(&marker).expect("marker after forced export"),
            ticks_after_first,
            "force must re-evaluate even when the fingerprint is unchanged"
        );
        std::env::remove_var("PWDIR_M6_FAST");
    }

    #[test]
    fn stale_fingerprint_falls_back_to_evaluation() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_STALE");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_STALE = 'v1'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        let first = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert_eq!(first.action, Action::Apply);

        // 预写一个「指纹与当前 rc 内容不符」的 cache 条目：快速路径必须失配 → 重新评估
        let mut entry = cache.load(&rc).expect("load").expect("entry");
        entry.fingerprint.rc_sha256 = "stale".repeat(8);
        cache.save(&entry).expect("save stale entry");

        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(
            second.action,
            Action::Apply,
            "stale fingerprint must re-evaluate"
        );
        assert!(
            second
                .diagnostics
                .iter()
                .all(|d| d.message != "fingerprint unchanged (fast path)"),
            "stale cache must not report the fast path: {:?}",
            second.diagnostics
        );
        std::env::remove_var("PWDIR_M6_STALE");
    }

    #[test]
    fn invalidated_cache_reevaluates_without_force() {
        ensure_pwsh();
        std::env::remove_var("PWDIR_M6_RELOAD");
        let tmp = tempfile::tempdir().expect("tempdir");
        let rc = write_rc(tmp.path(), "$env:PWDIR_M6_RELOAD = 'x'");
        let (store, cache, _guard) = temp_stores();
        store.allow(&rc).expect("allow");

        export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 1");
        assert!(cache.load(&rc).expect("load").is_some());

        // `pwdir reload` 语义：cache 失效后，下一次 export（不 force）强制重评估
        cache.invalidate(&rc).expect("invalidate");
        assert!(cache.load(&rc).expect("load").is_none());
        let second = export_with_store(
            tmp.path(),
            &EvaluatorConfig::default(),
            &store,
            &cache,
            false,
        )
        .expect("export 2");
        assert_eq!(second.action, Action::Apply);
        assert!(cache.load(&rc).expect("load").is_some(), "cache rebuilt");
        std::env::remove_var("PWDIR_M6_RELOAD");
    }
}
