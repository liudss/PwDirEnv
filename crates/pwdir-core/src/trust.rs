//! Trust store：基于内容 SHA-256 的授权记录（M1，ADR-005）。
//!
//! 安全边界（SPEC §6.5）：
//! - trust 基于**内容 hash** 而非 mtime——RC 任何字节变化都立即失效（重新 blocked）；
//! - record 写入使用「同目录临时文件 + rename」原子替换，并发 allow 同一 RC
//!   时不会读到半写的 JSON（Windows 上 `fs::rename` 对已存在目标是原子覆盖）；
//! - record 文件名 = canonical 路径的 SHA-256：规避路径中的非法字符 / 长度 /
//!   编码问题，且同一 RC 的多次 allow 稳定映射到同一文件（覆盖而非堆积）。

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};

use pwdir_protocol::{TrustRecord, PROTOCOL_SCHEMA};

use crate::pathutil::normalize_path_str;
use crate::PwDirError;

/// 计算字节流的 SHA-256（小写 hex 字符串）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 计算文件当前内容的 SHA-256；文件不存在 / 不可读时返回 IO 错误。
pub fn file_sha256(path: &Path) -> Result<String, PwDirError> {
    Ok(sha256_hex(&fs::read(path)?))
}

/// 临时文件随机后缀（进程 ID + 纳秒时钟）：并发写入不互相覆盖。
fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

/// Trust record 存储。`allow_dir` 可注入以便测试；生产路径由 [`crate::dirs`] 解析。
pub struct TrustStore {
    allow_dir: PathBuf,
}

impl TrustStore {
    /// 用显式 allow 目录构造（测试注入 / 高级用法）。
    pub fn new(allow_dir: PathBuf) -> Self {
        Self { allow_dir }
    }

    /// 用默认存储目录构造（`%LOCALAPPDATA%\PwDirEnv\allow`，SPEC §6.3）。
    ///
    /// 注意：刻意不实现 `Default` trait——目录解析失败必须显式传播给调用方，
    /// 静默 fallback 会破坏 trust 存储位置的可预测性（安全边界 > 简洁度）。
    pub fn open() -> Result<Self, PwDirError> {
        Ok(Self::new(crate::dirs::allow_dir()?))
    }

    /// 授权 RC 内容：读取 bytes、计算 SHA-256、原子写入 record 并返回。
    pub fn allow(&self, rc_path: &Path) -> Result<TrustRecord, PwDirError> {
        // 文件不存在 / 不可读 → IO 错误；不存在"授权空内容"的语义
        let bytes = fs::read(rc_path)?;
        let sha256 = sha256_hex(&bytes);
        let canonical = self.canonical_path(rc_path);
        let record = TrustRecord {
            schema: PROTOCOL_SCHEMA,
            path: canonical,
            // Windows file ID 的 rename-hardening 留待后续；不能以 file ID 取代内容 hash（SPEC §6.2）
            file_id: None,
            sha256,
            allowed_at: Utc::now(),
        };
        let payload = serde_json::to_vec_pretty(&record)?;

        fs::create_dir_all(&self.allow_dir)?;
        let record_name = self.record_name(rc_path);
        let final_path = self.allow_dir.join(&record_name);
        // 原子替换：先写临时文件再 rename 覆盖。若直接原地写，
        // 并发读到的可能是半写的 JSON（SPEC §6.5 / §12.4）。
        let tmp_path = self
            .allow_dir
            .join(format!(".{record_name}.tmp-{}", random_suffix()));
        fs::write(&tmp_path, &payload)?;
        fs::rename(&tmp_path, &final_path)?;
        Ok(record)
    }

    /// 判断 RC 当前内容是否处于已授权状态。
    ///
    /// 按当前文件 bytes 重新计算 SHA-256 与 record 比对：内容变了 → false
    /// （重新 blocked）。rc 文件不存在 / 无 record → false（这是正常状态而非错误）。
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

    /// 撤销授权：删除对应 record。record 不存在视为成功（幂等）。
    pub fn deny(&self, rc_path: &Path) -> Result<(), PwDirError> {
        let path = self.allow_dir.join(self.record_name(rc_path));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// 读取 record（不校验 hash），供 status 展示 record 元数据。
    pub fn record(&self, rc_path: &Path) -> Result<Option<TrustRecord>, PwDirError> {
        let path = self.allow_dir.join(self.record_name(rc_path));
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            // 无 record = 从未授权，是正常状态而非错误
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// 绝对化 + 字符串层规范化（`/`→`\`、去尾分隔符）。
    ///
    /// 不使用 `fs::canonicalize`：它会解析 symlink 并产生 `\\?\` verbatim 前缀，
    /// 显示与互操作都麻烦；M1 的 identity 只需要稳定、无歧义的绝对路径字符串。
    /// 非 UTF-8 路径按 lossy 处理（JSON 边界要求 valid Unicode，SPEC §16.1）；
    /// cwd 不可用（被删除）时相对输入原样返回，后续文件读取会自然报 IO 错误。
    pub fn canonical_path(&self, p: &Path) -> PathBuf {
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

    /// record 文件名：canonical 路径的 SHA-256 + `.json`。
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

    /// 构造注入临时目录的 store；返回 TempDir guard 以维持目录生命周期。
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
        // M1 验收核心：allow 之后改一个 byte → 重新 blocked（内容 hash，非 mtime）
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

        // 再次 deny 仍成功（幂等）
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
        // trust 文件名 = 路径 hash → 同一路径两次 allow 覆盖同一 record
        let (store, tmp) = temp_store();
        let rc = write_rc(tmp.path(), b"v1");

        let first = store.allow(&rc).expect("allow 1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = store.allow(&rc).expect("allow 2");

        assert_eq!(first.path, second.path);
        assert_ne!(first.allowed_at, second.allowed_at, "record was replaced");

        // 临时文件不残留、record 不堆积：目录里只有一个 .json
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
