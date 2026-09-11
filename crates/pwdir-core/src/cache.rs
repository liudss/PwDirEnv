//! Per-RC fingerprint metadata; never persist environment values. Missing or malformed entries are cache misses; other I/O errors propagate.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use pwdir_protocol::Fingerprint;

use crate::trust::{self, random_suffix};
use crate::{dirs, PwDirError};

pub const CACHE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub schema: u32,
    pub rc: PathBuf,
    pub fingerprint: Fingerprint,
    pub evaluated_at: String,
}

pub struct CacheStore {
    cache_dir: PathBuf,
}

impl CacheStore {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    pub fn open() -> Result<Self, PwDirError> {
        Ok(Self::new(dirs::cache_dir()?))
    }

    pub fn load(&self, rc: &Path) -> Result<Option<CacheEntry>, PwDirError> {
        let bytes = match fs::read(self.cache_path(rc)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let entry: CacheEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(_) => return Ok(None),
        };
        if entry.schema != CACHE_SCHEMA {
            return Ok(None);
        }
        Ok(Some(entry))
    }

    pub fn save(&self, entry: &CacheEntry) -> Result<(), PwDirError> {
        fs::create_dir_all(&self.cache_dir)?;
        let payload = serde_json::to_vec_pretty(entry)?;
        let name = self.entry_name(&entry.rc);
        let final_path = self.cache_dir.join(&name);
        let tmp_path = self
            .cache_dir
            .join(format!(".{name}.tmp-{}", random_suffix()));
        fs::write(&tmp_path, &payload)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    pub fn invalidate(&self, rc: &Path) -> Result<(), PwDirError> {
        match fs::remove_file(self.cache_path(rc)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

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
