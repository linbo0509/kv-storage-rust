//! 领域层和持久化层之间的一致性协调器。
//!
//! 修改命令遵循“校验 → 同步写 WAL → 修改内存”的顺序；检查点遵循
//! “安全写快照 → 压缩 WAL”的顺序。

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;

use crate::domain::{Command, DomainError, Reply, Store, StoreStatus};
use crate::persistence::{PersistenceError, SnapshotRepository, Wal};

/// 协调领域操作、JSON 快照和 WAL，保证修改命令先落盘再进入内存。
pub struct Engine {
    store: Store,
    wal: Wal,
    snapshots: SnapshotRepository,
    writes_since_checkpoint: u64,
}

impl Engine {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, EngineError> {
        let data_dir = data_dir.as_ref();
        let snapshots = SnapshotRepository::new(data_dir.join("snapshot.json"));
        let loaded = snapshots.load()?;
        let (store, snapshot_seq) = match loaded {
            Some(snapshot) => (snapshot.store, snapshot.last_seq),
            None => (Store::new(), 0),
        };
        let (wal, store) = Wal::open_with_store(data_dir.join("wal.log"), store, snapshot_seq)?;
        let writes_since_checkpoint = wal.last_seq().saturating_sub(snapshot_seq);
        Ok(Self {
            store,
            wal,
            snapshots,
            writes_since_checkpoint,
        })
    }

    pub fn execute(&mut self, command: Command) -> Result<Reply, EngineError> {
        let mutation = is_mutation(&command);
        if mutation {
            self.store.validate(&command)?;
            self.wal.append(&command)?;
        }

        let reply = self.store.execute(command)?;
        if mutation {
            self.writes_since_checkpoint = self.writes_since_checkpoint.saturating_add(1);
        }
        Ok(reply)
    }

    /// 执行业务命令，并在累计修改达到阈值时自动生成快照、压缩 WAL。
    ///
    /// 自动维护失败不会否定已经成功写入 WAL 的业务命令，因此使用嵌套结果
    /// 单独报告。阈值为 0 时关闭自动快照。
    pub fn execute_with_auto_checkpoint(
        &mut self,
        command: Command,
        checkpoint_after_writes: u64,
    ) -> Result<ExecutionOutcome, EngineError> {
        let reply = self.execute(command)?;
        let auto_checkpoint = if checkpoint_after_writes > 0
            && self.writes_since_checkpoint >= checkpoint_after_writes
        {
            Some(self.checkpoint())
        } else {
            None
        };

        Ok(ExecutionOutcome {
            reply,
            auto_checkpoint,
        })
    }

    #[must_use]
    pub fn status(&self) -> StoreStatus {
        self.store.status()
    }

    pub fn wal_size_bytes(&self) -> Result<u64, EngineError> {
        Ok(self.wal.size_bytes()?)
    }

    #[must_use]
    pub fn writes_since_checkpoint(&self) -> u64 {
        self.writes_since_checkpoint
    }

    /// 生成完整 JSON 快照，成功后压缩 WAL。
    pub fn checkpoint(&mut self) -> Result<(), EngineError> {
        let last_seq = self.wal.last_seq();
        self.snapshots.save(&self.store, last_seq)?;
        self.wal.compact()?;
        self.writes_since_checkpoint = 0;
        Ok(())
    }

    /// 清空全部数据并重新初始化持久化文件。
    ///
    /// 先清空内存，再复用检查点把「空快照 + 空 WAL」落盘。序列号保持单调递增
    /// 而非归零，以免快照与 WAL 在无法原子切换时留下不一致窗口。
    pub fn clear(&mut self) -> Result<usize, EngineError> {
        let cleared = self.store.clear();
        self.checkpoint()?;
        Ok(cleared)
    }
}

/// `execute_with_auto_checkpoint` 的执行结果。
///
/// 自动快照的结果单独存放，即使失败也不否定已经成功写入 WAL 的业务命令。
pub struct ExecutionOutcome {
    /// 业务命令的成功响应。
    pub reply: Reply,
    /// 本次执行后是否触发了自动快照，以及其结果。
    pub auto_checkpoint: Option<Result<(), EngineError>>,
}

fn is_mutation(command: &Command) -> bool {
    matches!(
        command,
        Command::Set { .. } | Command::Update { .. } | Command::Delete { .. }
    )
}

#[derive(Debug)]
pub enum EngineError {
    Domain(DomainError),
    Persistence(PersistenceError),
}

impl Display for EngineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(error) => Display::fmt(error, formatter),
            Self::Persistence(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for EngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Persistence(error) => Some(error),
        }
    }
}

impl From<DomainError> for EngineError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

impl From<PersistenceError> for EngineError {
    fn from(error: PersistenceError) -> Self {
        Self::Persistence(error)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kv-storage-engine-test-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mutations_survive_engine_restart() {
        let test_dir = TestDir::new();

        {
            let mut engine = Engine::open(test_dir.path()).unwrap();
            engine
                .execute(Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                })
                .unwrap();
            engine
                .execute(Command::Update {
                    key: "course".into(),
                    value: "Advanced Rust".into(),
                })
                .unwrap();
        }

        let mut recovered = Engine::open(test_dir.path()).unwrap();
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
    fn rejected_mutations_are_not_written_to_the_wal() {
        let test_dir = TestDir::new();
        let mut engine = Engine::open(test_dir.path()).unwrap();

        assert!(matches!(
            engine.execute(Command::Update {
                key: "missing".into(),
                value: "value".into(),
            }),
            Err(EngineError::Domain(DomainError::NotFound { .. }))
        ));
        drop(engine);

        let recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(recovered.status().key_count, 0);
    }

    #[test]
    fn checkpoint_creates_a_snapshot_and_compacts_the_wal() {
        let test_dir = TestDir::new();
        let mut engine = Engine::open(test_dir.path()).unwrap();
        engine
            .execute(Command::Set {
                key: "course".into(),
                value: "Rust".into(),
            })
            .unwrap();
        engine.checkpoint().unwrap();

        assert!(test_dir.path().join("snapshot.json").is_file());
        let wal_text = fs::read_to_string(test_dir.path().join("wal.log")).unwrap();
        assert_eq!(wal_text, "RUST_KV_WAL\t2\t1\n");

        let mut recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(
            recovered
                .execute(Command::Get {
                    key: "course".into()
                })
                .unwrap(),
            Reply::Value("Rust".into())
        );
    }

    #[test]
    fn recovery_combines_snapshot_and_later_wal_records() {
        let test_dir = TestDir::new();
        {
            let mut engine = Engine::open(test_dir.path()).unwrap();
            engine
                .execute(Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                })
                .unwrap();
            engine.checkpoint().unwrap();
            engine
                .execute(Command::Set {
                    key: "teacher".into(),
                    value: "李老师".into(),
                })
                .unwrap();
        }

        let mut recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(recovered.status().key_count, 2);
        assert_eq!(
            recovered
                .execute(Command::Get {
                    key: "teacher".into()
                })
                .unwrap(),
            Reply::Value("李老师".into())
        );
    }

    #[test]
    fn automatic_checkpoint_compacts_wal_at_the_write_threshold() {
        let test_dir = TestDir::new();
        let mut engine = Engine::open(test_dir.path()).unwrap();

        let first = engine
            .execute_with_auto_checkpoint(
                Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                },
                2,
            )
            .unwrap();
        assert!(first.auto_checkpoint.is_none());
        assert_eq!(engine.writes_since_checkpoint(), 1);

        let second = engine
            .execute_with_auto_checkpoint(
                Command::Set {
                    key: "teacher".into(),
                    value: "李老师".into(),
                },
                2,
            )
            .unwrap();
        assert!(matches!(second.auto_checkpoint, Some(Ok(()))));
        assert_eq!(engine.writes_since_checkpoint(), 0);
        assert!(test_dir.path().join("snapshot.json").is_file());
        assert_eq!(
            fs::read_to_string(test_dir.path().join("wal.log")).unwrap(),
            "RUST_KV_WAL\t2\t2\n"
        );

        let recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(recovered.status().key_count, 2);
        assert_eq!(recovered.writes_since_checkpoint(), 0);
    }

    #[test]
    fn recovered_wal_records_count_towards_the_next_checkpoint() {
        let test_dir = TestDir::new();
        {
            let mut engine = Engine::open(test_dir.path()).unwrap();
            engine
                .execute(Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                })
                .unwrap();
            engine
                .execute(Command::Set {
                    key: "teacher".into(),
                    value: "李老师".into(),
                })
                .unwrap();
        }

        let mut recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(recovered.writes_since_checkpoint(), 2);
        let outcome = recovered
            .execute_with_auto_checkpoint(Command::Status, 2)
            .unwrap();
        assert!(matches!(outcome.auto_checkpoint, Some(Ok(()))));
        assert_eq!(recovered.writes_since_checkpoint(), 0);
    }

    #[test]
    fn clear_empties_data_and_persists_an_empty_state() {
        let test_dir = TestDir::new();
        {
            let mut engine = Engine::open(test_dir.path()).unwrap();
            engine
                .execute(Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                })
                .unwrap();
            engine
                .execute(Command::Set {
                    key: "teacher".into(),
                    value: "李老师".into(),
                })
                .unwrap();

            let cleared = engine.clear().unwrap();
            assert_eq!(cleared, 2);
            assert_eq!(engine.status().key_count, 0);
        }

        let mut recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(recovered.status().key_count, 0);
        recovered
            .execute(Command::Set {
                key: "fresh".into(),
                value: "value".into(),
            })
            .unwrap();
        assert_eq!(recovered.status().key_count, 1);
    }
}
