//! 持久化过程中可诊断的错误类型。

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

/// 快照和 WAL 可能返回的错误。
#[derive(Debug)]
pub enum PersistenceError {
    /// 创建、读取、写入或同步文件失败。
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// 文件格式、序列号或校验和异常。
    Corrupt {
        path: PathBuf,
        line: usize,
        offset: u64,
        reason: String,
    },
    /// 尝试把只读命令写入 WAL。
    UnsupportedCommand,
    /// 先前 WAL 写入失败；为避免继续写入不可靠的日志，拒绝后续修改。
    Unavailable { path: PathBuf },
}

impl PersistenceError {
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

impl Display for PersistenceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "{action} {} failed: {source}", path.display()),
            Self::Corrupt {
                path,
                line,
                offset,
                reason,
            } => write!(
                formatter,
                "corrupt data file {} at line {line}, byte {offset}: {reason}",
                path.display()
            ),
            Self::UnsupportedCommand => write!(formatter, "command cannot be stored in the WAL"),
            Self::Unavailable { path } => write!(
                formatter,
                "WAL {} is unavailable after an earlier write failure",
                path.display()
            ),
        }
    }
}

impl Error for PersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Corrupt { .. } | Self::UnsupportedCommand | Self::Unavailable { .. } => None,
        }
    }
}
