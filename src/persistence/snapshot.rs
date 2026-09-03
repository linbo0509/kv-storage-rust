//! JSON 快照的校验、读取与原子替换。

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::Store;

use super::PersistenceError;
use super::format::checksum64;

const SNAPSHOT_MAGIC: &str = "RUST_KV_SNAPSHOT";
const SNAPSHOT_VERSION: u16 = 1;
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

/// 管理 snapshot.json 的读取和安全替换。
pub struct SnapshotRepository {
    path: PathBuf,
}

/// 快照加载结果，同时携带快照已经覆盖到的 WAL 序列号。
pub struct LoadedSnapshot {
    pub store: Store,
    pub last_seq: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotFile {
    magic: String,
    version: u16,
    last_seq: u64,
    entries: BTreeMap<String, String>,
    checksum: String,
}

impl SnapshotRepository {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 快照不存在表示首次启动；快照存在但内容异常时必须返回错误。
    pub fn load(&self) -> Result<Option<LoadedSnapshot>, PersistenceError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(PersistenceError::io(
                    "read snapshot metadata",
                    &self.path,
                    error,
                ));
            }
        };

        if !metadata.is_file() {
            return Err(corrupt(&self.path, "snapshot path is not a regular file"));
        }
        if metadata.len() > MAX_SNAPSHOT_BYTES {
            return Err(corrupt(&self.path, "snapshot exceeds the maximum size"));
        }

        let bytes = fs::read(&self.path)
            .map_err(|error| PersistenceError::io("read snapshot", &self.path, error))?;
        let snapshot: SnapshotFile = serde_json::from_slice(&bytes)
            .map_err(|error| corrupt(&self.path, format!("invalid JSON: {error}")))?;

        if snapshot.magic != SNAPSHOT_MAGIC {
            return Err(corrupt(&self.path, "invalid snapshot magic"));
        }
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(corrupt(
                &self.path,
                format!("unsupported snapshot version: {}", snapshot.version),
            ));
        }

        let expected_checksum =
            parse_checksum(&snapshot.checksum).map_err(|reason| corrupt(&self.path, reason))?;
        let actual_checksum =
            calculate_checksum(snapshot.version, snapshot.last_seq, &snapshot.entries);
        if expected_checksum != actual_checksum {
            return Err(corrupt(
                &self.path,
                format!(
                    "checksum mismatch: expected {expected_checksum:016X}, calculated {actual_checksum:016X}"
                ),
            ));
        }

        Ok(Some(LoadedSnapshot {
            store: Store::from_entries(snapshot.entries),
            last_seq: snapshot.last_seq,
        }))
    }

    /// 先写同目录临时文件并同步，再替换正式快照；写失败不会覆盖旧快照。
    pub fn save(&self, store: &Store, last_seq: u64) -> Result<(), PersistenceError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                PersistenceError::io("create snapshot directory", parent, error)
            })?;
        }

        let entries = store.export_entries();
        let checksum = calculate_checksum(SNAPSHOT_VERSION, last_seq, &entries);
        let snapshot = SnapshotFile {
            magic: SNAPSHOT_MAGIC.into(),
            version: SNAPSHOT_VERSION,
            last_seq,
            entries,
            checksum: format!("{checksum:016X}"),
        };
        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|error| corrupt(&self.path, format!("serialize snapshot: {error}")))?;

        let temporary_path = temporary_path(&self.path);
        let mut temporary_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary_path)
            .map_err(|error| {
                PersistenceError::io("create temporary snapshot", &temporary_path, error)
            })?;
        temporary_file.write_all(&bytes).map_err(|error| {
            PersistenceError::io("write temporary snapshot", &temporary_path, error)
        })?;
        temporary_file.flush().map_err(|error| {
            PersistenceError::io("flush temporary snapshot", &temporary_path, error)
        })?;
        temporary_file.sync_all().map_err(|error| {
            PersistenceError::io("sync temporary snapshot", &temporary_path, error)
        })?;
        drop(temporary_file);

        // 在替换前重新读取临时文件，避免把无法解析的快照设为正式文件。
        validate_temporary_snapshot(&temporary_path, last_seq, checksum)?;

        if self.path.exists() {
            let backup_path = self.path.with_extension("bak");
            fs::copy(&self.path, &backup_path)
                .map_err(|error| PersistenceError::io("backup snapshot", &backup_path, error))?;
            File::open(&backup_path)
                .and_then(|file| file.sync_all())
                .map_err(|error| {
                    PersistenceError::io("sync snapshot backup", &backup_path, error)
                })?;
        }

        fs::rename(&temporary_path, &self.path)
            .map_err(|error| PersistenceError::io("replace snapshot", &self.path, error))
    }
}

fn validate_temporary_snapshot(
    path: &Path,
    last_seq: u64,
    checksum: u64,
) -> Result<(), PersistenceError> {
    let bytes = fs::read(path)
        .map_err(|error| PersistenceError::io("verify temporary snapshot", path, error))?;
    let snapshot: SnapshotFile = serde_json::from_slice(&bytes)
        .map_err(|error| corrupt(path, format!("temporary snapshot JSON is invalid: {error}")))?;
    if snapshot.magic != SNAPSHOT_MAGIC
        || snapshot.version != SNAPSHOT_VERSION
        || snapshot.last_seq != last_seq
        || parse_checksum(&snapshot.checksum).ok() != Some(checksum)
    {
        return Err(corrupt(path, "temporary snapshot verification failed"));
    }
    Ok(())
}

fn calculate_checksum(version: u16, last_seq: u64, entries: &BTreeMap<String, String>) -> u64 {
    let mut canonical = Vec::new();
    canonical.extend_from_slice(SNAPSHOT_MAGIC.as_bytes());
    canonical.extend_from_slice(&version.to_le_bytes());
    canonical.extend_from_slice(&last_seq.to_le_bytes());
    for (key, value) in entries {
        canonical.extend_from_slice(&(key.len() as u64).to_le_bytes());
        canonical.extend_from_slice(key.as_bytes());
        canonical.extend_from_slice(&(value.len() as u64).to_le_bytes());
        canonical.extend_from_slice(value.as_bytes());
    }
    checksum64(&canonical)
}

fn parse_checksum(value: &str) -> Result<u64, String> {
    u64::from_str_radix(value, 16).map_err(|_| "invalid snapshot checksum".into())
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn corrupt(path: &Path, reason: impl Into<String>) -> PersistenceError {
    PersistenceError::Corrupt {
        path: path.to_path_buf(),
        line: 0,
        offset: 0,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kv-storage-snapshot-test-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn snapshot_path(&self) -> PathBuf {
            self.0.join("snapshot.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn snapshot_round_trip_preserves_data_and_sequence() {
        let test_dir = TestDir::new();
        let repository = SnapshotRepository::new(test_dir.snapshot_path());
        let mut store = Store::new();
        store.set("课程".into(), "Rust 程序设计".into()).unwrap();

        repository.save(&store, 9).unwrap();
        let loaded = repository.load().unwrap().unwrap();

        assert_eq!(loaded.last_seq, 9);
        assert_eq!(loaded.store.get("课程"), Ok("Rust 程序设计"));
    }

    #[test]
    fn invalid_json_is_not_treated_as_an_empty_database() {
        let test_dir = TestDir::new();
        let path = test_dir.snapshot_path();
        fs::write(&path, b"{not-json").unwrap();
        let repository = SnapshotRepository::new(path);

        assert!(matches!(
            repository.load(),
            Err(PersistenceError::Corrupt { .. })
        ));
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let test_dir = TestDir::new();
        let path = test_dir.snapshot_path();
        let repository = SnapshotRepository::new(&path);
        repository.save(&Store::new(), 0).unwrap();

        let bytes = fs::read(&path).unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        document["entries"]["tampered"] = serde_json::Value::String("value".into());
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        assert!(matches!(
            repository.load(),
            Err(PersistenceError::Corrupt { .. })
        ));
    }
}
