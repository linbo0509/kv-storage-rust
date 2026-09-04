//! 领域规则产生的错误，不包含网络或磁盘错误。

use std::error::Error;
use std::fmt::{Display, Formatter};

/// 执行业务命令时可能返回的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// 键为空字符串。
    EmptyKey,
    /// 查询、修改或删除的键不存在。
    NotFound { key: String },
}

impl Display for DomainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyKey => write!(formatter, "key must not be empty"),
            Self::NotFound { key } => write!(formatter, "key not found: {key}"),
        }
    }
}

impl Error for DomainError {}
