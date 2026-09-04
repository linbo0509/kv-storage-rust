//! 纯业务领域层：定义命令、响应、错误和内存存储。
//!
//! 本模块不执行网络和文件 IO，便于单独测试核心键值操作。

mod command;
mod error;
mod reply;
mod store;

pub use command::Command;
pub use error::DomainError;
pub use reply::{Reply, SetOutcome, StoreStatus};
pub use store::Store;
