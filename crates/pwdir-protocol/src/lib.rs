//! Schema-1 data structures for native/PowerShell communication. JSON uses Unicode strings; native OS values may require lossy conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_SCHEMA: u32 = 1;

pub const RC_FILENAME: &str = ".envrc.ps1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredRc {
    pub project_root: PathBuf,
    pub rc_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
/// Retain display casing; diff code indexes by canonical ASCII-uppercase names.
pub struct EnvKey {
    pub canonical: String,
    pub display: String,
}

impl EnvKey {
    pub fn from_name(name: &str) -> Self {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub schema: u32,
    pub path: PathBuf,
    pub file_id: Option<u64>,
    pub sha256: String,
    pub allowed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Custom JSON representation avoids platform-specific OsString serialization.
pub struct EnvironmentSnapshot {
    pub vars: BTreeMap<EnvKey, OsString>,
}

impl Serialize for EnvironmentSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("EnvironmentSnapshot", 1)?;
        state.serialize_field("vars", &VarsView(&self.vars))?;
        state.end()
    }
}

struct VarsView<'a>(&'a BTreeMap<EnvKey, OsString>);

impl Serialize for VarsView<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
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
    pub fn from_process() -> Self {
        let mut vars: BTreeMap<EnvKey, OsString> = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            vars.insert(EnvKey::from_name(&name.to_string_lossy()), value);
        }
        Self { vars }
    }

    pub fn get_canonical(&self, name: &str) -> Option<&OsString> {
        let canonical = EnvKey::from_name(name).canonical;
        self.vars
            .keys()
            .find(|k| k.canonical == canonical)
            .and_then(|k| self.vars.get(k))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub environment: EnvironmentSnapshot,
    pub watches: Vec<WatchDependency>,
    pub sourced_files: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EvaluationError {
    pub kind: String,
    pub message: String,
    pub exception: Option<String>,
    /// PowerShell script stack trace.
    pub stack: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path")]
pub enum WatchDependency {
    #[serde(rename = "file")]
    File(PathBuf),
    #[serde(rename = "dir")]
    Directory(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub rc_sha256: String,
    pub sourced_files: Vec<FileFingerprint>,
    pub watches: Vec<WatchDependency>,
    #[serde(default)]
    pub watch_states: Vec<FileFingerprint>,
    pub evaluator_version: String,
    pub stdlib_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Noop,
    Apply,
    Blocked,
    Unload,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: String,
    pub message: String,
}

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

        assert_eq!(upper.display, "PATH");
        assert_eq!(mixed.display, "Path");
        assert_eq!(lower.display, "path");
    }

    #[test]
    fn env_key_ord_groups_by_canonical() {
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
