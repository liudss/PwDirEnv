//! Authorize top-level RC content using SHA-256 and a lexical absolute path. Records use same-directory temporary files and rename; this is not a filesystem sandbox.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};

use pwdir_protocol::{TrustRecord, PROTOCOL_SCHEMA};

use crate::pathutil::normalize_path_str;
use crate::PwDirError;

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn file_sha256(path: &Path) -> Result<String, PwDirError> {
    Ok(sha256_hex(&fs::read(path)?))
}

/// Make absolute and normalize separators; do not resolve symlinks or file identity.
pub fn canonical_path(p: &Path) -> PathBuf {
    let absolute = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(_) => p.to_path_buf(),
        }
    };
    PathBuf::from(normalize_path_str(&absolute.to_string_lossy()))
}

pub(crate) fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

pub struct TrustStore {
    allow_dir: PathBuf,
}

impl TrustStore {
    pub fn new(allow_dir: PathBuf) -> Self {
        Self { allow_dir }
    }

    pub fn open() -> Result<Self, PwDirError> {
        Ok(Self::new(crate::dirs::allow_dir()?))
    }

    pub fn allow(&self, rc_path: &Path) -> Result<TrustRecord, PwDirError> {
        let bytes = fs::read(rc_path)?;
        let sha256 = sha256_hex(&bytes);
        let canonical = self.canonical_path(rc_path);
        let record = TrustRecord {
            schema: PROTOCOL_SCHEMA,
            path: canonical,
            file_id: None,
            sha256,
            allowed_at: Utc::now(),
        };
        let payload = serde_json::to_vec_pretty(&record)?;

        fs::create_dir_all(&self.allow_dir)?;
        let record_name = self.record_name(rc_path);
        let final_path = self.allow_dir.join(&record_name);
        let tmp_path = self
            .allow_dir
            .join(format!(".{record_name}.tmp-{}", random_suffix()));
        fs::write(&tmp_path, &payload)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(record)
    }

    /// Rehash current bytes. Missing files or records are unapproved, not errors.
    pub fn is_allowed(&self, rc_path: &Path) -> Result<bool, PwDirError> {
        if !rc_path.is_file() {
            return Ok(false);
        }
        match self.record(rc_path)? {
            None => Ok(false),
            Some(record) => {
                let current = file_sha256(rc_path)?;
                Ok(current == record.sha256)
            }
        }
    }

    pub fn deny(&self, rc_path: &Path) -> Result<(), PwDirError> {
        let path = self.allow_dir.join(self.record_name(rc_path));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn record(&self, rc_path: &Path) -> Result<Option<TrustRecord>, PwDirError> {
        let path = self.allow_dir.join(self.record_name(rc_path));
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    pub fn canonical_path(&self, p: &Path) -> PathBuf {
        canonical_path(p)
    }

    fn record_name(&self, rc_path: &Path) -> String {
        let canonical = self.canonical_path(rc_path);
        format!(
            "{}.json",
            sha256_hex(canonical.to_string_lossy().as_bytes())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pwdir_protocol::RC_FILENAME;

    fn temp_store() -> (TrustStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::new(tmp.path().join("allow"));
        (store, tmp)
    }

    fn write_rc(dir: &Path, contents: &[u8]) -> PathBuf {
        let path = dir.join(RC_FILENAME);
        fs::write(&path, contents).expect("write rc fixture");
        path
    }

    #[test]
    fn allow_then_is_allowed() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"$env:FOO = 'bar'");

        let record = store.allow(&rc).expect("allow");
        assert_eq!(record.schema, 1);
        assert!(store.is_allowed(&rc).expect("is_allowed"));
    }

    #[test]
    fn single_byte_change_invalidates_trust() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"$env:FOO = 'bar'");
        store.allow(&rc).expect("allow");

        fs::write(&rc, b"$env:FOO = 'baz'").expect("rewrite one byte");
        assert!(!store.is_allowed(&rc).expect("is_allowed"));
    }

    #[test]
    fn deny_removes_record_and_is_idempotent() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"content");
        store.allow(&rc).expect("allow");

        store.deny(&rc).expect("deny");
        assert!(!store.is_allowed(&rc).expect("is_allowed"));
        assert!(store.record(&rc).expect("record").is_none());

        store.deny(&rc).expect("deny again");
    }

    #[test]
    fn allow_missing_rc_is_io_error() {
        let (store, tmp) = temp_store();
        let ghost = tmp.path().join("missing").join(RC_FILENAME);
        let err = store.allow(&ghost).expect_err("allow must fail");
        assert!(matches!(err, PwDirError::Io(_)));
    }

    #[test]
    fn repeated_allow_overwrites_same_record() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"v1");

        let first = store.allow(&rc).expect("allow 1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = store.allow(&rc).expect("allow 2");

        assert_eq!(first.path, second.path);
        assert_ne!(first.allowed_at, second.allowed_at, "record was replaced");

        let entries: Vec<_> = fs::read_dir(&store.allow_dir)
            .expect("read allow dir")
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn record_fields_are_well_formed() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"$env:OK = '1'");
        store.allow(&rc).expect("allow");

        let record = store
            .record(&rc)
            .expect("record ok")
            .expect("record exists");
        assert_eq!(record.schema, 1);
        assert_eq!(record.sha256.len(), 64);
        assert!(record.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(record.sha256.chars().all(|c| !c.is_ascii_uppercase()));
        assert!(record.allowed_at <= Utc::now());
        assert_eq!(record.path, store.canonical_path(&rc));
        assert!(record.file_id.is_none());
    }

    #[test]
    fn is_allowed_false_without_record() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"never allowed");
        assert!(!store.is_allowed(&rc).expect("is_allowed"));
    }

    #[test]
    fn is_allowed_false_when_rc_deleted() {
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"content");
        store.allow(&rc).expect("allow");

        fs::remove_file(&rc).expect("remove rc");
        assert!(!store.is_allowed(&rc).expect("is_allowed"));
    }

    #[test]
    fn canonical_path_normalizes_absolute_input() {
        let (store, _tmp) = temp_store();
        let normalized = store.canonical_path(Path::new(r"C:\Foo/Bar\.envrc.ps1"));
        assert_eq!(normalized, PathBuf::from(r"C:\Foo\Bar\.envrc.ps1"));
    }

    #[test]
    fn canonical_path_absolutizes_relative_input() {
        let (store, _tmp) = temp_store();
        let normalized = store.canonical_path(Path::new("foo/.envrc.ps1"));
        assert!(normalized.is_absolute());
        assert!(normalized.ends_with(r"foo\.envrc.ps1"));
    }
}
