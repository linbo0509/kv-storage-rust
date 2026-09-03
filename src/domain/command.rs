//! 键值存储支持的业务命令。

/// 本地命令行和 TCP 服务器共用的业务命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 写入新键，或者覆盖已有键。
    Set { key: String, value: String },
    /// 只修改已有键；键不存在时返回错误。
    Update { key: String, value: String },
    /// 查询指定键。
    Get { key: String },
    /// 删除指定键。
    Delete { key: String },
    /// 按稳定顺序列出全部键。
    Keys,
    /// 查看当前键数量。
    Status,
}
