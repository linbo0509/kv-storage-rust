//! 可持久化键值存储的核心库。
//!
//! 模块按照“领域模型 → 持久化引擎 → TCP 协议与应用层”分层：领域层不依赖
//! 文件或网络，因此既能被本地命令行复用，也能被服务器复用。

pub mod client;
pub mod domain;
pub mod engine;
pub mod metrics;
pub mod persistence;
pub mod protocol;
pub mod server;
