//! pwdir-protocol：PwDirEnv native core 与 Host Shell 之间的纯数据协议（schema 1）。
//!
//! 本 crate 只定义 serde 数据结构，无 IO、无平台逻辑（见 SPEC §16）。
//! 字段名保持 snake_case，与 `pwdir export --format json` 的 JSON 契约一致。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::ser::{SerializeMap, SerializeStruct};
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

/// 一份完整的 Process 环境快照（SPEC §8.1/§16）。
///
/// JSON 线上格式为 `{ "<name>": "<value>", ... }`（变量名用 display casing，
/// 值为普通 Unicode 字符串）。不直接 derive serde：std `OsString` 在 Windows
/// 上会序列化成 `{"Windows": [bytes]}` 的 WTF-8 enum 形态，且拒绝从普通
/// JSON 字符串反序列化 —— 与 bootstrap 结果文件契约（vars 值是字符串）冲突，
/// 因此手动实现 Serialize/Deserialize（SPEC §16.1：JSON 必须输出 Unicode string）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentSnapshot {
    pub vars: BTreeMap<EnvKey, OsString>,
}

impl Serialize for EnvironmentSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 线上结构 = struct 形态 {"vars": { "<name>": "<value>" }}，与 bootstrap 结果文件契约一致
        let mut state = serializer.serialize_struct("EnvironmentSnapshot", 1)?;
        state.serialize_field("vars", &VarsView(&self.vars))?;
        state.end()
    }
}

/// `vars` map 的视图序列化器：key 用 display casing，value 用普通 Unicode 字符串。
struct VarsView<'a>(&'a BTreeMap<EnvKey, OsString>);

impl Serialize for VarsView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            // JSON 无法表示非法 Unicode；env value 按 lossy 转换（SPEC §16.1 认可的边界）
            map.serialize_entry(&key.display, value.to_string_lossy().as_ref())?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for EnvironmentSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EnvSnapshotVisitor;

        impl<'de> serde::de::Visitor<'de> for EnvSnapshotVisitor {
            type Value = EnvironmentSnapshot;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a struct with a `vars` map of environment variables")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut vars = BTreeMap::new();
                while let Some(field) = access.next_key::<String>()? {
                    if field == "vars" {
                        let raw: BTreeMap<String, String> = access.next_value()?;
                        for (name, value) in raw {
                            vars.insert(EnvKey::from_name(&name), OsString::from(value));
                        }
                    } else {
                        // 未知字段（如未来 schema 扩展）跳过
                        access.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(EnvironmentSnapshot { vars })
            }
        }

        deserializer.deserialize_struct("EnvironmentSnapshot", &["vars"], EnvSnapshotVisitor)
    }
}

impl EnvironmentSnapshot {
    /// 从**当前进程**的 Process environment 构造快照（SPEC §8.1）。
    ///
    /// pwdir.exe 是 Host pwsh 的直接子进程，其继承环境即 Host 当前环境，
    /// 因此 M3 的 baseline diff 直接以本方法的结果为起点（SPEC §7.3）。
    ///
    /// 变量名经 `EnvKey::from_name` 归一 canonical 大小写；重复 key
    /// （仅大小写不同）按 `vars_os` 枚举顺序**后者胜出**，与 Windows
    /// 环境块"同名覆盖"语义一致。
    pub fn from_process() -> Self {
        let mut vars: BTreeMap<EnvKey, OsString> = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            vars.insert(EnvKey::from_name(&name.to_string_lossy()), value);
        }
        Self { vars }
    }

    /// 按 canonical 名称（大小写不敏感）查询变量值。
    pub fn get_canonical(&self, name: &str) -> Option<&OsString> {
        let canonical = EnvKey::from_name(name).canonical;
        self.vars
            .keys()
            .find(|k| k.canonical == canonical)
            .and_then(|k| self.vars.get(k))
    }
}

/// 一次成功 evaluation 的完整结果（SPEC §16）：resulting environment +
/// watch/source 元数据 + 结构化 diagnostics。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub environment: EnvironmentSnapshot,
    pub watches: Vec<WatchDependency>,
    pub sourced_files: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

/// evaluator 失败时的结构化错误（bootstrap catch 分支产物，SPEC §17）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvaluationError {
    /// 错误类别，如 `evaluation-failed` / `timeout` / `no-result` / `protocol-invalid`。
    pub kind: String,
    pub message: String,
    /// 异常类型全名（如 `System.Management.Automation.RuntimeException`）。
    pub exception: Option<String>,
    /// PowerShell script stack trace.
    pub stack: Option<String>,
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
    /// watched 文件/目录在 evaluate 时点的 size+mtime 快照（M6，与 `watches` 按下标对齐）。
    /// 快速路径据此做廉价判变，不必重新 evaluate；`#[serde(default)]` 保证旧 JSON 兼容。
    /// watch 目标允许尚不存在（stdlib 语义），不存在时以 size=0 + modified=None 的
    /// 「缺失哨兵」占位，使「评估时不存在、检查时仍不存在」稳定判为未变。
    #[serde(default)]
    pub watch_states: Vec<FileFingerprint>,
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

    #[test]
    fn environment_snapshot_from_process_contains_path() {
        let snapshot = EnvironmentSnapshot::from_process();
        let path = snapshot
            .get_canonical("PATH")
            .expect("process env always has PATH");
        assert!(!path.is_empty(), "PATH value should not be empty");
        // display casing 保留原始形态（Windows 上通常是 `Path`）
        let key = snapshot
            .vars
            .keys()
            .find(|k| k.canonical == "PATH")
            .expect("PATH key exists");
        assert!(!key.display.is_empty());
    }

    #[test]
    fn environment_snapshot_from_process_round_trips() {
        let snapshot = EnvironmentSnapshot::from_process();
        let raw = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let back: EnvironmentSnapshot = serde_json::from_str(&raw).expect("deserialize snapshot");
        assert_eq!(back, snapshot);
        // canonical 归并：反序列化后 PATH 仍可按 canonical 查询
        assert!(back.get_canonical("pAtH").is_some());
    }

    #[test]
    fn evaluation_result_round_trips_with_watches_and_diagnostics() {
        let result = EvaluationResult {
            environment: EnvironmentSnapshot {
                vars: BTreeMap::from([
                    (EnvKey::from_name("FOO"), OsString::from("bar")),
                    (
                        EnvKey::from_name("Path"),
                        OsString::from(r"C:\first;C:\Windows"),
                    ),
                ]),
            },
            watches: vec![
                WatchDependency::File(PathBuf::from(r"C:\src\app\package.json")),
                WatchDependency::Directory(PathBuf::from(r"C:\src\app\config")),
            ],
            sourced_files: vec![PathBuf::from(r"C:\src\common\.envrc.ps1")],
            diagnostics: vec![Diagnostic {
                level: "info".to_string(),
                message: "loaded".to_string(),
            }],
        };

        let value = serde_json::to_value(&result).unwrap();
        // watches 的 kind 标签必须出现在 JSON 中（file / dir）
        assert_eq!(
            value["watches"][0],
            json!({"kind": "file", "path": r"C:\src\app\package.json"})
        );
        assert_eq!(
            value["watches"][1],
            json!({"kind": "dir", "path": r"C:\src\app\config"})
        );
        assert_eq!(
            value["sourced_files"][0],
            json!(r"C:\src\common\.envrc.ps1")
        );
        assert_eq!(
            value["diagnostics"][0],
            json!({"level": "info", "message": "loaded"})
        );
        // environment.vars 的 value 是字符串形态
        assert_eq!(value["environment"]["vars"]["FOO"], json!("bar"));

        let back: EvaluationResult = serde_json::from_value(value).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn evaluation_error_round_trips() {
        let error = EvaluationError {
            kind: "evaluation-failed".to_string(),
            message: "boom".to_string(),
            exception: Some("System.Management.Automation.RuntimeException".to_string()),
            stack: Some("at <ScriptBlock>, :line 1".to_string()),
        };
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(
            value,
            json!({
                "kind": "evaluation-failed",
                "message": "boom",
                "exception": "System.Management.Automation.RuntimeException",
                "stack": "at <ScriptBlock>, :line 1"
            })
        );
        let back: EvaluationError = serde_json::from_value(value).unwrap();
        assert_eq!(back, error);
    }

    #[test]
    fn fingerprint_round_trips_with_watch_states() {
        let fingerprint = Fingerprint {
            rc_sha256: "a".repeat(64),
            sourced_files: vec![FileFingerprint {
                path: PathBuf::from(r"C:\src\common\.envrc.ps1"),
                size: 42,
                modified: Some(std::time::UNIX_EPOCH),
            }],
            watches: vec![WatchDependency::File(PathBuf::from(
                r"C:\src\app\package.json",
            ))],
            watch_states: vec![FileFingerprint {
                path: PathBuf::from(r"C:\src\app\package.json"),
                size: 7,
                modified: None,
            }],
            evaluator_version: "bootstrap-sha".to_string(),
            stdlib_version: "stdlib-sha".to_string(),
        };

        let value = serde_json::to_value(&fingerprint).unwrap();
        assert_eq!(value["watch_states"][0]["size"], json!(7));
        assert_eq!(
            value["watch_states"][0]["path"],
            json!(r"C:\src\app\package.json")
        );

        let back: Fingerprint = serde_json::from_value(value).unwrap();
        assert_eq!(back, fingerprint);
    }

    #[test]
    fn fingerprint_deserializes_legacy_json_without_watch_states() {
        // M6 前的 JSON 没有 watch_states：#[serde(default)] 兜底为空 Vec，
        // 旧 cache/协议载荷反序列化不报错（由 core 侧把空 watch_states 视为「已变化」）
        let legacy = json!({
            "rc_sha256": "abc",
            "sourced_files": [],
            "watches": [{"kind": "file", "path": r"C:\x\p.json"}],
            "evaluator_version": "e",
            "stdlib_version": "s"
        });
        let back: Fingerprint = serde_json::from_value(legacy).unwrap();
        assert_eq!(back.rc_sha256, "abc");
        assert!(back.watch_states.is_empty());
        assert_eq!(back.watches.len(), 1);
    }
}
