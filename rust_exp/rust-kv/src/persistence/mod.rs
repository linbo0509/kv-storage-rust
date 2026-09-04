//! 持久化层：使用 JSON 快照缩短恢复时间，使用 WAL 保证修改先落盘。
//!
//! 启动时先校验并加载快照，再重放其后的 WAL。任何损坏都会显式报错，绝不
//! 把异常数据文件当作空数据库继续运行。

mod error;
mod format;
mod snapshot;
mod wal;

pub use error::PersistenceError;
pub use snapshot::SnapshotRepository;
pub use wal::Wal;
