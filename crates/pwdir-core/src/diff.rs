//! 大小写不敏感的环境 diff（M3，SPEC §8.3 / §8.2 / §9.4）。
//!
//! 语义：
//! - `set` 同时覆盖新增与修改（desired 有、baseline 无 → 新增；双方都有但值不同 → 修改）；
//! - `unset` 仅 baseline 有而 desired 没有的 key；
//! - key 比较按 canonical（ASCII invariant uppercase，SPEC §8.2），`PATH`/`Path`/`path` 同一 key；
//! - 输出 key 用 display casing：set 用 desired 的 display，unset 用 baseline 的 display；
//! - PATH 空值三态（不存在 / `''` / 非空）天然区分：变量不存在 → 不进索引，
//!   空串是"存在但空"，与不存在不同（SPEC §9.4）。

use std::collections::BTreeMap;
use std::ffi::OsString;

use pwdir_protocol::{EnvPatch, EnvironmentSnapshot};

/// 把快照索引成 `canonical → (display, value)`，便于按 Windows 语义归并比较。
///
/// 同一 canonical 出现多次（仅大小写不同的重复 key）时**后写胜出**：
/// BTreeMap 按 (canonical, display) 有序迭代，与 `EnvironmentSnapshot::from_process`
/// 的"同名覆盖、后者胜出"语义保持一致。
fn index_by_canonical(snapshot: &EnvironmentSnapshot) -> BTreeMap<String, (String, OsString)> {
    let mut index = BTreeMap::new();
    for (key, value) in &snapshot.vars {
        // Windows 环境块里隐藏的「盘符当前目录」伪变量（`=C:=...` / `=D:=...`，Zed / VS Code
        // 等按目录启动终端的启动器会注入）名称含 `=`，不是合法环境变量名。两侧 API 处理不对称：
        // 基线用 Rust `std::env::vars_os()` 会捕获它们，desired 用 .NET
        // `GetEnvironmentVariables('Process')` 却会过滤 —— 于是 diff 产出 `=D:` 这类伪 key，
        // Host 应用时 `[Environment]::SetEnvironmentVariable` 抛
        // "Environment variable name cannot contain equal character"。这里统一剔除，
        // 保证 patch 的 key 永远是合法 Windows 变量名（SPEC §8.2 语义）。
        if key.display.contains('=') {
            continue;
        }
        index.insert(key.canonical.clone(), (key.display.clone(), value.clone()));
    }
    index
}

/// baseline → desired 的环境 diff（SPEC §8.3）。
///
/// set: 仅 desired 有 或 双方都有但值不同（覆盖新增+修改）；
/// unset: 仅 baseline 有。
/// key 比较用 canonical；输出到 set 的 key 用 desired 的 display，unset 用 baseline 的 display。
pub fn diff_env(baseline: &EnvironmentSnapshot, desired: &EnvironmentSnapshot) -> EnvPatch {
    let base = index_by_canonical(baseline);
    let want = index_by_canonical(desired);

    let mut patch = EnvPatch::default();

    for (canonical, (display, value)) in &want {
        match base.get(canonical) {
            // baseline 没有该 key → 新增（包括 baseline 无 PATH、desired 有 `PATH=''` 的三态场景）
            None => {
                // JSON 边界要求 Unicode 字符串（SPEC §16.1）；env value 通常可按 Unicode 处理，
                // 非 UTF-8 字节做有损替换是该边界的既定策略
                patch
                    .set
                    .insert(display.clone(), value.to_string_lossy().into_owned());
            }
            // 双方都有但值不同 → 修改；OsString 精确比较（含空串 vs 非空）
            Some((_, old)) if old != value => {
                patch
                    .set
                    .insert(display.clone(), value.to_string_lossy().into_owned());
            }
            // 值完全相同 → 无变化
            Some(_) => {}
        }
    }

    for (canonical, (display, _)) in &base {
        if !want.contains_key(canonical) {
            patch.unset.insert(display.clone());
        }
    }

    patch
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwdir_protocol::EnvKey;

    /// 手工构造快照：`[("PATH", "a;b"), ...]` → `BTreeMap<EnvKey, OsString>`。
    fn snap(pairs: &[(&str, &str)]) -> EnvironmentSnapshot {
        EnvironmentSnapshot {
            vars: pairs
                .iter()
                .map(|(k, v)| (EnvKey::from_name(k), OsString::from(*v)))
                .collect(),
        }
    }

    #[test]
    fn new_var_in_desired_becomes_set() {
        let baseline = snap(&[("A", "1")]);
        let desired = snap(&[("A", "1"), ("NEW_VAR", "fresh")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("NEW_VAR").map(String::as_str), Some("fresh"));
        assert!(patch.unset.is_empty());
    }

    #[test]
    fn changed_value_becomes_set_with_new_value() {
        let baseline = snap(&[("MODE", "debug")]);
        let desired = snap(&[("MODE", "release")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("MODE").map(String::as_str), Some("release"));
        assert!(patch.unset.is_empty());
    }

    #[test]
    fn removed_var_becomes_unset() {
        let baseline = snap(&[("OLD", "1"), ("KEEP", "x")]);
        let desired = snap(&[("KEEP", "x")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["OLD"]);
    }

    #[test]
    fn no_changes_yield_empty_patch() {
        let baseline = snap(&[("A", "1"), ("B", ""), ("PATH", "C:\\bin")]);
        let desired = snap(&[("A", "1"), ("B", ""), ("PATH", "C:\\bin")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.is_empty(), "identical snapshots must diff to empty");
    }

    #[test]
    fn same_value_different_casing_is_same_key() {
        // baseline "PATH" vs desired "Path"：canonical 相同且值相同 → 空 patch
        let baseline = snap(&[("PATH", "C:\\bin")]);
        let desired = snap(&[("Path", "C:\\bin")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.is_empty());
    }

    #[test]
    fn value_change_across_casing_uses_desired_display() {
        // 值不同 → set，且 key 用 desired 的 display（"Path"），不是 baseline 的 "PATH"
        let baseline = snap(&[("PATH", "C:\\old")]);
        let desired = snap(&[("Path", "C:\\new")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.unset.is_empty(), "same canonical must not be unset");
        assert_eq!(
            patch.set.keys().collect::<Vec<_>>(),
            ["Path"],
            "set key must use desired display casing"
        );
        assert_eq!(patch.set.get("Path").map(String::as_str), Some("C:\\new"));
    }

    #[test]
    fn path_three_states() {
        // SPEC §9.4：不存在 / `''` / 非空 必须区分

        // 不存在 → 存在但空：set PATH=''（"存在但空"与"不存在"不是同一状态）
        let patch = diff_env(&snap(&[]), &snap(&[("PATH", "")]));
        assert_eq!(patch.set.get("PATH").map(String::as_str), Some(""));

        // 存在但空 → 非空：set PATH='abc'
        let patch = diff_env(&snap(&[("PATH", "")]), &snap(&[("PATH", "abc")]));
        assert_eq!(patch.set.get("PATH").map(String::as_str), Some("abc"));

        // 非空 → 不存在：unset PATH
        let patch = diff_env(&snap(&[("PATH", "abc")]), &snap(&[]));
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["PATH"]);

        // 存在但空 → 不存在：同样是 unset（baseline "有"含空串形态）
        let patch = diff_env(&snap(&[("PATH", "")]), &snap(&[]));
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["PATH"]);
    }

    #[test]
    fn unicode_values_round_trip() {
        let baseline = snap(&[("GREETING", "你好")]);
        let desired = snap(&[("GREETING", "こんにちは 🎉")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(
            patch.set.get("GREETING").map(String::as_str),
            Some("こんにちは 🎉")
        );

        // 反向：中文 → emoji 修改，值原样保留
        let baseline = snap(&[("EMOJI", "🦀")]);
        let desired = snap(&[("EMOJI", "🚀")]);
        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("EMOJI").map(String::as_str), Some("🚀"));
    }

    #[test]
    fn empty_value_vs_missing_are_different_states() {
        // 非环境名变量的同款三态验证：baseline 无 X、desired X='' → set（而非无变化）
        let patch = diff_env(&snap(&[]), &snap(&[("X", "")]));
        assert_eq!(patch.set.get("X").map(String::as_str), Some(""));

        // baseline X=''、desired X='' → 无变化
        let patch = diff_env(&snap(&[("X", "")]), &snap(&[("X", "")]));
        assert!(patch.is_empty());
    }

    #[test]
    fn hidden_drive_vars_with_equal_sign_are_ignored() {
        // Windows 隐藏「盘符当前目录」伪变量（=C: / =D:）名称含 `=`：基线（Rust vars_os）
        // 会捕获、desired（.NET GetEnvironmentVariables）会过滤。若不剔除，diff 会产出
        // `=D:` 伪 key，Host 应用时 SetEnvironmentVariable 抛 "name cannot contain equal character"。
        // 基线有 =D: + OLD，desired 两者皆无 → 只有 OLD 进 unset，=D: 被忽略
        let baseline = snap(&[("=D:", "D:\\project\\hip_bench"), ("OLD", "1")]);
        let desired = snap(&[]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty());
        assert_eq!(
            patch.unset.iter().collect::<Vec<_>>(),
            vec!["OLD"],
            "hidden =X: pseudo-vars must never surface in the patch"
        );
        assert!(
            !patch.unset.iter().any(|k| k.contains('=')),
            "patch unset must only contain valid env var names: {:?}",
            patch.unset
        );

        // 反向：desired 有 =D:、baseline 无 → 也不得进入 set
        let baseline = snap(&[("OLD", "1")]);
        let desired = snap(&[("=D:", "x"), ("OLD", "1")]);
        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty(), "set must not carry =X: keys");
    }
}
