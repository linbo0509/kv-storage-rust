//! 领域层返回的成功结果。

/// SET 命令用于区分首次创建和覆盖写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOutcome {
    /// 创建了新键。
    Created,
    /// 覆盖已有键，并返回旧值。
    Overwritten { old_value: String },
}

/// 与存储内容相关、可持久化推导出的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStatus {
    /// 当前键的总数。
    pub key_count: usize,
}

/// 一条业务命令的成功响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Set(SetOutcome),
    Updated { old_value: String },
    Value(String),
    Deleted { value: String },
    Keys(Vec<String>),
    Status(StoreStatus),
}
