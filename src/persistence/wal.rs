//! 预写日志（WAL）的追加、同步、恢复与压缩。

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::domain::{Command, Store};

use super::error::PersistenceError;
use super::format::{
    FormatError, MAX_RECORD_BYTES, decode_header, decode_record, encode_header, encode_record,
};

/// 追加写日志。每条记录带有递增序列号和校验和。
pub struct Wal {
    path: PathBuf,
    file: File,
    next_seq: u64,
    failed: bool,
}

impl Wal {
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Store), PersistenceError> {
        Self::open_with_store(path, Store::new(), 0)
    }

    /// 在已经加载快照的 Store 上继续重放 WAL。
    pub fn open_with_store(
        path: impl AsRef<Path>,
        store: Store,
        snapshot_seq: u64,
    ) -> Result<(Self, Store), PersistenceError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| PersistenceError::io("create data directory", parent, error))?;
        }

        create_wal_if_missing(&path, snapshot_seq)?;
        let (store, next_seq) = recover_store(&path, store, snapshot_seq)?;
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|error| PersistenceError::io("open WAL for append", &path, error))?;

        Ok((
            Self {
                path,
                file,
                next_seq,
                failed: false,
            },
            store,
        ))
    }

    pub fn append(&mut self, command: &Command) -> Result<u64, PersistenceError> {
        if self.failed {
            return Err(PersistenceError::Unavailable {
                path: self.path.clone(),
            });
        }

        let seq = self.next_seq;
        let record = encode_record(seq, command).map_err(|error| match error {
            FormatError::UnsupportedCommand => PersistenceError::UnsupportedCommand,
            FormatError::Invalid(reason) => PersistenceError::Corrupt {
                path: self.path.clone(),
                line: 0,
                offset: 0,
                reason,
            },
        })?;

        if let Err(source) = self.file.write_all(record.as_bytes()) {
            self.failed = true;
            return Err(PersistenceError::io("append WAL", &self.path, source));
        }
        if let Err(source) = self.file.flush() {
            self.failed = true;
            return Err(PersistenceError::io("flush WAL", &self.path, source));
        }
        if let Err(source) = self.file.sync_data() {
            self.failed = true;
            return Err(PersistenceError::io("sync WAL", &self.path, source));
        }

        self.next_seq = self.next_seq.checked_add(1).ok_or_else(|| {
            self.failed = true;
            PersistenceError::Corrupt {
                path: self.path.clone(),
                line: 0,
                offset: 0,
                reason: "sequence number overflow".into(),
            }
        })?;
        Ok(seq)
    }

    /// 当前已经成功持久化的最后一条序列号。
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// 返回当前 WAL 文件占用的字节数，供状态展示和后续自动压缩策略使用。
    pub fn size_bytes(&self) -> Result<u64, PersistenceError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| PersistenceError::io("read WAL metadata", &self.path, error))
    }

    /// 快照成功后，把 WAL 压缩成只含 base_seq 的新文件头。
    ///
    /// 如果压缩前崩溃，旧 WAL 仍可重放；如果压缩后崩溃，快照包含 base_seq
    /// 之前的完整状态，因此不会丢失数据。
    pub fn compact(&mut self) -> Result<(), PersistenceError> {
        let base_seq = self.last_seq();
        let temporary_path = self.path.with_extension("log.tmp");
        let header = encode_header(base_seq);

        let mut temporary_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary_path)
            .map_err(|error| {
                PersistenceError::io("create temporary WAL", &temporary_path, error)
            })?;
        temporary_file
            .write_all(header.as_bytes())
            .map_err(|error| PersistenceError::io("write temporary WAL", &temporary_path, error))?;
        temporary_file
            .sync_all()
            .map_err(|error| PersistenceError::io("sync temporary WAL", &temporary_path, error))?;
        drop(temporary_file);

        fs::rename(&temporary_path, &self.path)
            .map_err(|error| PersistenceError::io("replace WAL", &self.path, error))?;
        self.file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                self.failed = true;
                PersistenceError::io("reopen compacted WAL", &self.path, error)
            })?;
        self.next_seq = base_seq + 1;
        Ok(())
    }
}

fn create_wal_if_missing(path: &Path, base_seq: u64) -> Result<(), PersistenceError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(encode_header(base_seq).as_bytes())
                .map_err(|error| PersistenceError::io("write WAL header", path, error))?;
            file.sync_all()
                .map_err(|error| PersistenceError::io("sync WAL header", path, error))?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(PersistenceError::io("create WAL", path, error)),
    }
}

fn recover_store(
    path: &Path,
    mut store: Store,
    snapshot_seq: u64,
) -> Result<(Store, u64), PersistenceError> {
    let file = File::open(path).map_err(|error| PersistenceError::io("open WAL", path, error))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let header_size = reader
        .read_until(b'\n', &mut buffer)
        .map_err(|error| PersistenceError::io("read WAL header", path, error))?;

    if !buffer.ends_with(b"\n") {
        return Err(corrupt(path, 1, 0, "truncated WAL header"));
    }
    buffer.pop();
    let header = std::str::from_utf8(&buffer)
        .map_err(|_| corrupt(path, 1, 0, "WAL header is not valid UTF-8"))?;
    let base_seq = decode_header(header).map_err(|error| match error {
        FormatError::Invalid(reason) => corrupt(path, 1, 0, reason),
        FormatError::UnsupportedCommand => corrupt(path, 1, 0, "unsupported command in WAL header"),
    })?;
    if base_seq > snapshot_seq {
        return Err(corrupt(
            path,
            1,
            0,
            format!(
                "WAL starts after sequence {base_seq}, but snapshot only contains {snapshot_seq}"
            ),
        ));
    }

    let mut expected_seq = base_seq
        .checked_add(1)
        .ok_or_else(|| corrupt(path, 1, 0, "WAL base sequence overflow"))?;
    let mut line_number = 2_usize;
    let mut offset = header_size as u64;

    loop {
        buffer.clear();
        let record_offset = offset;
        let bytes_read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| PersistenceError::io("read WAL record", path, error))?;
        if bytes_read == 0 {
            break;
        }
        offset += bytes_read as u64;

        if bytes_read > MAX_RECORD_BYTES {
            return Err(corrupt(
                path,
                line_number,
                record_offset,
                "record exceeds the maximum size",
            ));
        }
        if !buffer.ends_with(b"\n") {
            return Err(corrupt(
                path,
                line_number,
                record_offset,
                "truncated final record",
            ));
        }
        buffer.pop();

        let line = std::str::from_utf8(&buffer).map_err(|_| {
            corrupt(
                path,
                line_number,
                record_offset,
                "record is not valid UTF-8",
            )
        })?;
        let (seq, command) = decode_record(line).map_err(|error| match error {
            FormatError::Invalid(reason) => corrupt(path, line_number, record_offset, reason),
            FormatError::UnsupportedCommand => corrupt(
                path,
                line_number,
                record_offset,
                "unsupported command in WAL",
            ),
        })?;

        if seq != expected_seq {
            return Err(corrupt(
                path,
                line_number,
                record_offset,
                format!("expected sequence {expected_seq}, found {seq}"),
            ));
        }

        // 快照已经包含的历史记录只做格式与序列校验，不重复应用。
        if seq > snapshot_seq {
            store.execute(command).map_err(|error| {
                corrupt(
                    path,
                    line_number,
                    record_offset,
                    format!("invalid mutation history: {error}"),
                )
            })?;
        }

        expected_seq = expected_seq
            .checked_add(1)
            .ok_or_else(|| corrupt(path, line_number, record_offset, "sequence number overflow"))?;
        line_number += 1;
    }

    if expected_seq - 1 < snapshot_seq {
        return Err(corrupt(
            path,
            line_number,
            offset,
            format!(
                "WAL ends at sequence {}, before snapshot sequence {snapshot_seq}",
                expected_seq - 1
            ),
        ));
    }

    Ok((store, expected_seq))
}

fn corrupt(path: &Path, line: usize, offset: u64, reason: impl Into<String>) -> PersistenceError {
    PersistenceError::Corrupt {
        path: path.to_path_buf(),
        line,
        offset,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::domain::Reply;
    use crate::persistence::format::encode_record;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kv-storage-wal-test-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn wal_path(&self) -> PathBuf {
            self.0.join("wal.log")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_a_new_wal_and_recovers_mutations() {
        let test_dir = TestDir::new();
        let path = test_dir.wal_path();
        let (mut wal, store) = Wal::open(&path).unwrap();
        assert_eq!(store.status().key_count, 0);

        wal.append(&Command::Set {
            key: "course".into(),
            value: "Rust".into(),
        })
        .unwrap();
        wal.append(&Command::Update {
            key: "course".into(),
            value: "Advanced Rust".into(),
        })
        .unwrap();
        drop(wal);

        let (_, mut recovered) = Wal::open(&path).unwrap();
        assert_eq!(
            recovered
                .execute(Command::Get {
                    key: "course".into()
                })
                .unwrap(),
            Reply::Value("Advanced Rust".into())
        );
    }

    #[test]
    fn compacted_wal_continues_after_the_snapshot_sequence() {
        let test_dir = TestDir::new();
        let path = test_dir.wal_path();
        let (mut wal, mut store) = Wal::open(&path).unwrap();
        let command = Command::Set {
            key: "course".into(),
            value: "Rust".into(),
        };
        wal.append(&command).unwrap();
        store.execute(command).unwrap();
        wal.compact().unwrap();
        drop(wal);

        let (wal, recovered) = Wal::open_with_store(&path, store, 1).unwrap();
        assert_eq!(wal.last_seq(), 1);
        assert_eq!(recovered.get("course"), Ok("Rust"));
    }

    #[test]
    fn rejects_a_truncated_final_record() {
        let test_dir = TestDir::new();
        let path = test_dir.wal_path();
        let (wal, _) = Wal::open(&path).unwrap();
        drop(wal);

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"1\tS\t00").unwrap();
        file.sync_all().unwrap();

        assert!(matches!(
            Wal::open(&path),
            Err(PersistenceError::Corrupt { .. })
        ));
    }

    #[test]
    fn rejects_a_checksum_mismatch() {
        let test_dir = TestDir::new();
        let path = test_dir.wal_path();
        let mut record = encode_record(
            1,
            &Command::Set {
                key: "course".into(),
                value: "Rust".into(),
            },
        )
        .unwrap();
        let checksum_index = record.len() - 2;
        let replacement = if &record[checksum_index..checksum_index + 1] == "0" {
            "1"
        } else {
            "0"
        };
        record.replace_range(checksum_index..checksum_index + 1, replacement);

        let mut contents = encode_header(0).into_bytes();
        contents.extend_from_slice(record.as_bytes());
        fs::write(&path, contents).unwrap();

        assert!(matches!(
            Wal::open(&path),
            Err(PersistenceError::Corrupt { .. })
        ));
    }

    #[test]
    fn rejects_an_invalid_mutation_history() {
        let test_dir = TestDir::new();
        let path = test_dir.wal_path();
        let record = encode_record(
            1,
            &Command::Update {
                key: "missing".into(),
                value: "value".into(),
            },
        )
        .unwrap();

        let mut contents = encode_header(0).into_bytes();
        contents.extend_from_slice(record.as_bytes());
        fs::write(&path, contents).unwrap();

        assert!(matches!(
            Wal::open(&path),
            Err(PersistenceError::Corrupt { .. })
        ));
    }
}
