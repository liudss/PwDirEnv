//! Fingerprint cache（M6，SPEC §12.4）：每个 RC 一份 JSON 条目，供 hook 快速路径判变。
//!
//! 关键取舍：**cache 不持久化 desired environment 值**（SPEC §12.4 的保守选择）——
//! 环境变量值可能含 secrets，持久化到磁盘会扩大泄漏面；新 shell 首次进入由 module
//! 传 `--force` 强制重评估，天然拿到完整环境，无需从 cache 恢复。cache 只存
//! 指纹 + watch/source 路径 + rc 路径 + 评估时间戳。
//!
//! 写入与 trust store 相同的「同目录临时文件 + rename」原子替换（Windows 上
//! `fs::rename` 对已存在目标是原子覆盖），并发 shell 同 RC 评估不会读到半写 JSON。
//! 条目文件名 = canonical RC 路径的 SHA-256（与 trust record 同一命名策略）。

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use pwdir_protocol::Fingerprint;

use crate::trust::{self, random_suffix};
use crate::{dirs, PwDirError};

/// cache 条目 schema 版本。结构变更必须提升此值；load 对不识别的 schema 按未命中处理。
pub const CACHE_SCHEMA: u32 = 1;

/// 一次成功 evaluation 的缓存条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub schema: u32,
    pub rc: PathBuf,
    pub fingerprint: Fingerprint,
    /// ISO8601（RFC3339）时间戳，仅诊断用，不参与判变。
    pub evaluated_at: String,
}

/// Cache 存储。`cache_dir` 可注入以便测试；生产路径由 [`crate::dirs`] 解析。
pub struct CacheStore {
    cache_dir: PathBuf,
}

impl CacheStore {
    /// 用显式 cache 目录构造（测试注入 / 高级用法）。
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// 用默认存储目录构造（`%LOCALAPPDATA%\PwDirEnv\cache`，SPEC §6.3）。
    pub fn open() -> Result<Self, PwDirError> {
        Ok(Self::new(dirs::cache_dir()?))
    }

    /// 读取 RC 的 cache 条目；无条目 / 损坏 / schema 不识别 → `Ok(None)`。
    ///
    /// cache 是纯加速结构：任何「读不出可用条目」的情形都必须安全降级为
    /// 重新评估（宁可慢，不可错），因此反序列化失败不作为错误上抛。
    pub fn load(&self, rc: &Path) -> Result<Option<CacheEntry>, PwDirError> {
        let bytes = match fs::read(self.cache_path(rc)) {
            Ok(bytes) => bytes,
            // 无条目 = 从未评估过，是正常状态而非错误
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let entry: CacheEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            // 损坏 / 半写（理论上原子替换可避免）/ 未来 schema → 未命中
            Err(_) => return Ok(None),
        };
        if entry.schema != CACHE_SCHEMA {
            return Ok(None);
        }
        Ok(Some(entry))
    }

    /// 原子写入条目（同目录 `.tmp` + rename 覆盖，SPEC §21 并发语义）。
    pub fn save(&self, entry: &CacheEntry) -> Result<(), PwDirError> {
        fs::create_dir_all(&self.cache_dir)?;
        let payload = serde_json::to_vec_pretty(entry)?;
        let name = self.entry_name(&entry.rc);
        let final_path = self.cache_dir.join(&name);
        // 原子替换：直接原地写会让并发读读到半写 JSON（SPEC §6.5/§12.4 同源要求）
        let tmp_path = self
            .cache_dir
            .join(format!(".{name}.tmp-{}", random_suffix()));
        fs::write(&tmp_path, &payload)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    /// 使 RC 的 cache 失效（`pwdir reload` 语义）。条目不存在视为成功（幂等）。
    pub fn invalidate(&self, rc: &Path) -> Result<(), PwDirError> {
        match fs::remove_file(self.cache_path(rc)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// 条目文件路径：`<cache_dir>/<sha256(canonical rc path)>.json`。
    fn cache_path(&self, rc: &Path) -> PathBuf {
        self.cache_dir.join(self.entry_name(rc))
    }

    fn entry_name(&self, rc: &Path) -> String {
        let canonical = trust::canonical_path(rc);
        format!(
            "{}.json",
            trust::sha256_hex(canonical.to_string_lossy().as_bytes())
        )
    }
}

/// 当前时刻的 ISO8601 时间戳（cache 条目的 `evaluated_at` 字段）。
pub fn now_iso8601() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    use pwdir_protocol::{FileFingerprint, WatchDependency};

    fn sample_fingerprint(rc_hint: &str) -> Fingerprint {
        Fingerprint {
            rc_sha256: trust::sha256_hex(rc_hint.as_bytes()),
            sourced_files: vec![],
            watches: vec![WatchDependency::File(PathBuf::from("C:\\x\\p.json"))],
            watch_states: vec![],
            evaluator_version: "ev".to_string(),
            stdlib_version: "std".to_string(),
        }
    }

    fn sample_entry(rc: &Path) -> CacheEntry {
        CacheEntry {
            schema: CACHE_SCHEMA,
            rc: rc.to_path_buf(),
            fingerprint: sample_fingerprint(&rc.to_string_lossy()),
            evaluated_at: now_iso8601(),
        }
    }

    fn temp_store() -> (CacheStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = CacheStore::new(tmp.path().join("cache"));
        (store, tmp)
    }

    #[test]
    fn save_then_load_round_trips() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");
        let entry = sample_entry(&rc);

        store.save(&entry).expect("save");
        let loaded = store.load(&rc).expect("load").expect("entry exists");
        assert_eq!(loaded, entry);
        assert_eq!(loaded.schema, CACHE_SCHEMA);
        assert_eq!(
            loaded.fingerprint,
            sample_fingerprint(&rc.to_string_lossy())
        );
        assert!(!loaded.evaluated_at.is_empty());
    }

    #[test]
    fn load_without_entry_is_none() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("never-evaluated").join(".envrc.ps1");
        assert!(store.load(&rc).expect("load").is_none());
    }

    #[test]
    fn invalidate_removes_entry_and_is_idempotent() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");
        store.save(&sample_entry(&rc)).expect("save");

        store.invalidate(&rc).expect("invalidate");
        assert!(store.load(&rc).expect("load").is_none());
        // 幂等：再删一次仍成功
        store.invalidate(&rc).expect("invalidate again");
    }

    #[test]
    fn entries_are_keyed_per_rc_path() {
        let (store, tmp) = temp_store();
        let rc_a = tmp.path().join("a").join(".envrc.ps1");
        let rc_b = tmp.path().join("b").join(".envrc.ps1");
        store.save(&sample_entry(&rc_a)).expect("save a");

        assert!(store.load(&rc_a).expect("load").is_some());
        assert!(store.load(&rc_b).expect("load").is_none());
    }

    #[test]
    fn save_replaces_existing_entry_atomically() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");

        let first = sample_entry(&rc);
        store.save(&first).expect("save 1");
        let mut second = sample_entry(&rc);
        second.fingerprint.rc_sha256 = "changed".to_string();
        store.save(&second).expect("save 2");

        let loaded = store.load(&rc).expect("load").expect("entry exists");
        assert_eq!(loaded.fingerprint.rc_sha256, "changed");

        // 临时文件不残留：目录里只有一个 .json 条目
        let leftovers: Vec<_> = fs::read_dir(&store.cache_dir)
            .expect("read cache dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files should remain");
    }

    #[test]
    fn corrupted_entry_degrades_to_none() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");
        // 直接写一个损坏文件（模拟异常中断 / 外部损坏）
        let path = store.cache_path(&rc);
        fs::create_dir_all(&store.cache_dir).expect("mkdir");
        fs::write(&path, b"{ not valid json").expect("write garbage");

        assert!(store.load(&rc).expect("load").is_none());
    }

    #[test]
    fn unknown_schema_degrades_to_none() {
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");
        let mut entry = sample_entry(&rc);
        entry.schema = CACHE_SCHEMA + 1;
        store.save(&entry).expect("save future schema");

        assert!(store.load(&rc).expect("load").is_none());
    }

    #[test]
    fn fingerprint_field_round_trips_through_entry() {
        // CacheEntry 序列化必须保真 Fingerprint（含 watch_states 与哨兵 modified=None）
        let (store, tmp) = temp_store();
        let rc = tmp.path().join("proj").join(".envrc.ps1");
        let mut entry = sample_entry(&rc);
        entry.fingerprint.sourced_files = vec![FileFingerprint {
            path: PathBuf::from("C:\\x\\s.ps1"),
            size: 3,
            modified: None,
        }];
        store.save(&entry).expect("save");

        let loaded = store.load(&rc).expect("load").expect("exists");
        assert_eq!(loaded.fingerprint, entry.fingerprint);
    }
}
