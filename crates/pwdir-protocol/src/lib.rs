//! pwdir-protocol：PwDirEnv native core 与 Host Shell 之间的纯数据协议（schema 1）。
//!
//! 本 crate 只定义 serde 数据结构，无 IO、无平台逻辑（见 SPEC §16）。
//! 字段名保持 snake_case，与 `pwdir export --format json` 的 JSON 契约一致。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 协议 schema 版本。结构变更必须提升此值。
pub const PROTOCOL_SCHEMA: u32 = 1;

/// 唯一默认 RC 文件名（SPEC §5.1）。
///
/// 首版不识别 Bash 语义的 `.envrc`，避免把 Bash 脚本交给 PowerShell evaluator。
pub const RC_FILENAME: &str = ".envrc.ps1";

/// 向上发现的结果：RC 文件与其所属 project root（即 RC 所在目录，SPEC §5.2/§16）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredRc {
    pub project_root: PathBuf,
    pub rc_path: PathBuf,
}

/// Windows 环境变量名（大小写不敏感语义，SPEC §8.2）。
///
/// `canonical` 为 ASCII invariant uppercase，用于归并与比较；
/// `display` 保留原始大小写，序列化时作为 JSON map key 输出。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub struct EnvKey {
    pub canonical: String,
    pub display: String,
}

impl EnvKey {
    pub fn from_name(name: &str) -> Self {
        // 只对 ASCII 做 invariant uppercase；非 ASCII 字符保持原样，
        // 避免 Unicode 大小写折叠引入跨平台的歧义映射。
        let canonical: String = name.chars().map(|c| c.to_ascii_uppercase()).collect();
        Self {
            canonical,
            display: name.to_string(),
        }
    }
}

impl From<&str> for EnvKey {
    fn from(name: &str) -> Self {
        Self::from_name(name)
    }
}

impl From<String> for EnvKey {
    fn from(name: String) -> Self {
        Self::from_name(&name)
    }
}

impl From<EnvKey> for String {
    fn from(key: EnvKey) -> Self {
        key.display
    }
}

/// 环境变量 diff：`set` 同时覆盖新增和修改，`unset` 为待删除的变量名（SPEC §8.3）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvPatch {
    pub set: BTreeMap<String, String>,
    pub unset: BTreeSet<String>,
}

impl EnvPatch {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.unset.is_empty()
    }
}

/// Trust record：基于 RC 内容 SHA-256 的授权记录（SPEC §6.2，ADR-005）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub schema: u32,
    pub path: PathBuf,
    pub file_id: Option<u64>,
    pub sha256: String,
    pub allowed_at: DateTime<Utc>,
}

/// 一份完整的 Process 环境快照。M2/M3 在此结构上补齐 capture/diff 方法。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    pub vars: BTreeMap<EnvKey, OsString>,
}

/// Watch 依赖（SPEC §12.2）。JSON 形如 `{"kind": "file", "path": "..."}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path")]
pub enum WatchDependency {
    #[serde(rename = "file")]
    File(PathBuf),
    #[serde(rename = "dir")]
    Directory(PathBuf),
}

/// 单个文件的快速 fingerprint（size + mtime；SHA-256 仅在快速路径判变后计算）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// 一次 evaluation 的指纹，用于 hook 快速路径判变（SPEC §12.2）。
/// `rc_sha256` 用 hex 字符串表示，便于 JSON 与 trust record 互操作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub rc_sha256: String,
    pub sourced_files: Vec<FileFingerprint>,
    pub watches: Vec<WatchDependency>,
    pub evaluator_version: String,
    pub stdlib_version: String,
}

/// export 动作语义（SPEC §14.7）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Noop,
    Apply,
    Blocked,
    Unload,
    Error,
}

/// 结构化诊断信息。level 取 error/warn/info/debug。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: String,
    pub message: String,
}

/// `pwdir export --format json` 的顶层输出（SPEC §14.7）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOutput {
    pub schema: u32,
    pub action: Action,
    pub rc: Option<PathBuf>,
    pub patch: EnvPatch,
    pub managed_keys: Vec<String>,
    pub fingerprint: Option<Fingerprint>,
    pub diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_key_is_case_insensitive() {
        let upper = EnvKey::from_name("PATH");
        let mixed = EnvKey::from_name("Path");
        let lower = EnvKey::from_name("path");

        assert_eq!(upper.canonical, mixed.canonical);
        assert_eq!(mixed.canonical, lower.canonical);
        assert_eq!(upper.canonical, "PATH");

        // display 保留原始大小写
        assert_eq!(upper.display, "PATH");
        assert_eq!(mixed.display, "Path");
        assert_eq!(lower.display, "path");
    }

    #[test]
    fn env_key_ord_groups_by_canonical() {
        // BTreeMap<EnvKey, _> 按 canonical 优先排序，同类 key 聚在一起
        let mut map = BTreeMap::new();
        map.insert(EnvKey::from_name("Path"), OsString::from("a"));
        map.insert(EnvKey::from_name("APPDATA"), OsString::from("b"));
        let keys: Vec<String> = map.keys().map(|k| k.canonical.clone()).collect();
        assert_eq!(keys, vec!["APPDATA", "PATH"]);
    }

    #[test]
    fn env_patch_is_empty() {
        let mut patch = EnvPatch::default();
        assert!(patch.is_empty());
        patch.set.insert("FOO".to_string(), "bar".to_string());
        assert!(!patch.is_empty());
        patch.set.clear();
        patch.unset.insert("OLD".to_string());
        assert!(!patch.is_empty());
    }

    #[test]
    fn watch_dependency_round_trips() {
        let dep = WatchDependency::File(PathBuf::from(r"C:\src\app\package.json"));
        let value = serde_json::to_value(&dep).unwrap();
        assert_eq!(
            value,
            json!({"kind": "file", "path": r"C:\src\app\package.json"})
        );
        let back: WatchDependency = serde_json::from_value(value).unwrap();
        assert_eq!(back, dep);
    }

    #[test]
    fn discovered_rc_serde_round_trips() {
        let rc = DiscoveredRc {
            project_root: PathBuf::from(r"C:\src\app"),
            rc_path: PathBuf::from(r"C:\src\app\.envrc.ps1"),
        };
        let value = serde_json::to_value(&rc).unwrap();
        assert_eq!(
            value,
            json!({
                "project_root": r"C:\src\app",
                "rc_path": r"C:\src\app\.envrc.ps1",
            })
        );
        let back: DiscoveredRc = serde_json::from_value(value).unwrap();
        assert_eq!(back, rc);
    }

    #[test]
    fn export_output_round_trips_spec_json() {
        // 对齐 SPEC §14.7 的示例 JSON 结构
        let output = ExportOutput {
            schema: PROTOCOL_SCHEMA,
            action: Action::Apply,
            rc: Some(PathBuf::from(r"C:\src\app\.envrc.ps1")),
            patch: EnvPatch {
                set: BTreeMap::from([("FOO".to_string(), "bar".to_string())]),
                unset: BTreeSet::from(["OLD".to_string()]),
            },
            managed_keys: vec!["FOO".to_string(), "OLD".to_string()],
            fingerprint: None,
            diagnostics: vec![Diagnostic {
                level: "info".to_string(),
                message: "loaded".to_string(),
            }],
        };

        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(
            value,
            json!({
                "schema": 1,
                "action": "apply",
                "rc": r"C:\src\app\.envrc.ps1",
                "patch": {"set": {"FOO": "bar"}, "unset": ["OLD"]},
                "managed_keys": ["FOO", "OLD"],
                "fingerprint": null,
                "diagnostics": [{"level": "info", "message": "loaded"}]
            })
        );

        let back: ExportOutput = serde_json::from_value(value).unwrap();
        assert_eq!(back, output);
    }
}
