//! 客户端与服务器共用的轻量级逐行文本协议。
//!
//! 每个请求和响应都以换行符结束。协议层只负责分帧、解析和格式化，不直接
//! 访问存储引擎。

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::BufRead;

use crate::domain::{Command, Reply, SetOutcome};
use crate::metrics::MetricsSnapshot;

/// 单条 TCP 文本帧允许的最大字节数，包含末尾换行符。
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// 客户端可以发送给服务器的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Execute(Command),
    Save,
    /// 请求清空全部数据；实际清空需要二次确认。
    Clear,
    /// 二次确认：同意清空。
    Yes,
    /// 二次确认：取消清空。
    No,
    Quit,
}

/// 从 TCP 字节流中读取一条以换行符结尾的完整帧。
///
/// TCP 没有“消息边界”，因此不能假设一次 read 就对应一条命令。
pub fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, ProtocolError> {
    let mut bytes = Vec::new();
    // 对 reader 再借用一次，避免 Read::take 取得底层读取器的所有权。
    let mut limited = std::io::Read::take(reader, (MAX_FRAME_BYTES + 1) as u64);
    let bytes_read = limited
        .read_until(b'\n', &mut bytes)
        .map_err(ProtocolError::Io)?;

    if bytes_read == 0 {
        return Ok(None);
    }
    if bytes_read > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    if !bytes.ends_with(b"\n") {
        return Err(ProtocolError::TruncatedFrame);
    }

    bytes.pop();
    if bytes.ends_with(b"\r") {
        bytes.pop();
    }

    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ProtocolError::InvalidUtf8)
}

/// 解析简单的本地文本协议。key 不允许空白，value 可以包含普通空格。
pub fn parse_request(line: &str) -> Result<Request, ProtocolError> {
    let mut parts = line.split_whitespace();
    let name = parts
        .next()
        .ok_or(ProtocolError::EmptyCommand)?
        .to_ascii_uppercase();

    match name.as_str() {
        "SET" | "UPDATE" => {
            let key = required_part(&mut parts, "key")?;
            let value_parts: Vec<&str> = parts.collect();
            if value_parts.is_empty() {
                return Err(ProtocolError::MissingArgument("value"));
            }
            let value = value_parts.join(" ");
            let command = if name == "SET" {
                Command::Set { key, value }
            } else {
                Command::Update { key, value }
            };
            Ok(Request::Execute(command))
        }
        "GET" | "DELETE" => {
            let key = required_part(&mut parts, "key")?;
            ensure_no_extra_parts(&mut parts)?;
            let command = if name == "GET" {
                Command::Get { key }
            } else {
                Command::Delete { key }
            };
            Ok(Request::Execute(command))
        }
        "KEYS" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Execute(Command::Keys))
        }
        "STATUS" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Execute(Command::Status))
        }
        "SAVE" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Save)
        }
        "CLEAR" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Clear)
        }
        "YES" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Yes)
        }
        "NO" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::No)
        }
        "QUIT" | "EXIT" => {
            ensure_no_extra_parts(&mut parts)?;
            Ok(Request::Quit)
        }
        _ => Err(ProtocolError::UnknownCommand(name)),
    }
}

/// 把领域响应编码为单行文本，避免响应中的控制字符破坏帧边界。
#[must_use]
pub fn format_reply(reply: Reply) -> String {
    match reply {
        Reply::Set(SetOutcome::Created) => "OK\tCREATED".into(),
        Reply::Set(SetOutcome::Overwritten { old_value }) => {
            format!("OK\tOVERWRITTEN\t{}", escape_field(&old_value))
        }
        Reply::Updated { old_value } => {
            format!("OK\tUPDATED\t{}", escape_field(&old_value))
        }
        Reply::Value(value) => format!("VALUE\t{}", escape_field(&value)),
        Reply::Deleted { value } => format!("OK\tDELETED\t{}", escape_field(&value)),
        Reply::Keys(keys) => {
            let mut fields = vec!["KEYS".to_string(), keys.len().to_string()];
            fields.extend(keys.iter().map(|key| escape_field(key)));
            fields.join("\t")
        }
        Reply::Status(status) => format!("STATUS\trunning\tkeys={}", status.key_count),
    }
}

#[must_use]
pub fn format_error(code: &str, message: &str) -> String {
    format!("ERR\t{}\t{}", escape_field(code), escape_field(message))
}

/// 编码服务器级状态；字段使用 key=value，便于客户端展示和脚本解析。
#[must_use]
pub fn format_server_status(
    key_count: usize,
    wal_bytes: u64,
    writes_since_checkpoint: u64,
    checkpoint_after_writes: u64,
    metrics: &MetricsSnapshot,
) -> String {
    let commands_succeeded = metrics
        .commands_total
        .saturating_sub(metrics.commands_failed);
    format!(
        "STATUS\trunning\tkeys={key_count}\tactive_clients={}\ttotal_connections={}\tcommands_total={}\tcommands_succeeded={commands_succeeded}\tcommands_failed={}\tcheckpoint_attempts={}\tcheckpoint_failures={}\twrites_since_checkpoint={writes_since_checkpoint}\tcheckpoint_after_writes={checkpoint_after_writes}\tbytes_received={}\tbytes_sent={}\twal_bytes={wal_bytes}\tuptime_seconds={}",
        metrics.active_clients,
        metrics.total_connections,
        metrics.commands_total,
        metrics.commands_failed,
        metrics.checkpoint_attempts,
        metrics.checkpoint_failures,
        metrics.bytes_received,
        metrics.bytes_sent,
        metrics.uptime_seconds,
    )
}

#[must_use]
pub fn terminate_frame(line: &str) -> String {
    format!("{line}\n")
}

fn required_part<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
    name: &'static str,
) -> Result<String, ProtocolError> {
    parts
        .next()
        .map(str::to_owned)
        .ok_or(ProtocolError::MissingArgument(name))
}

fn ensure_no_extra_parts<'a>(
    parts: &mut impl Iterator<Item = &'a str>,
) -> Result<(), ProtocolError> {
    if parts.next().is_some() {
        Err(ProtocolError::ExtraArguments)
    } else {
        Ok(())
    }
}

fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(std::io::Error),
    EmptyCommand,
    UnknownCommand(String),
    MissingArgument(&'static str),
    ExtraArguments,
    FrameTooLarge,
    TruncatedFrame,
    InvalidUtf8,
}

impl Display for ProtocolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "读取网络帧失败：{error}"),
            Self::EmptyCommand => write!(formatter, "命令不能为空"),
            Self::UnknownCommand(command) => write!(formatter, "未知命令：{command}"),
            Self::MissingArgument(argument) => write!(formatter, "缺少参数：{argument}"),
            Self::ExtraArguments => write!(formatter, "参数过多"),
            Self::FrameTooLarge => write!(formatter, "请求超过 64 KiB 限制"),
            Self::TruncatedFrame => write!(formatter, "连接在完整命令到达前关闭"),
            Self::InvalidUtf8 => write!(formatter, "请求不是合法 UTF-8"),
        }
    }
}

impl Error for ProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::EmptyCommand
            | Self::UnknownCommand(_)
            | Self::MissingArgument(_)
            | Self::ExtraArguments
            | Self::FrameTooLarge
            | Self::TruncatedFrame
            | Self::InvalidUtf8 => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn parses_set_values_containing_spaces() {
        assert_eq!(
            parse_request("SET course Rust 程序设计").unwrap(),
            Request::Execute(Command::Set {
                key: "course".into(),
                value: "Rust 程序设计".into(),
            })
        );
    }

    #[test]
    fn rejects_invalid_command_shapes() {
        assert!(matches!(
            parse_request("GET"),
            Err(ProtocolError::MissingArgument("key"))
        ));
        assert!(matches!(
            parse_request("STATUS extra"),
            Err(ProtocolError::ExtraArguments)
        ));
        assert!(matches!(
            parse_request("UNKNOWN"),
            Err(ProtocolError::UnknownCommand(_))
        ));
    }

    #[test]
    fn reads_one_frame_at_a_time() {
        let mut reader = Cursor::new(b"GET course\nSTATUS\n");
        assert_eq!(read_frame(&mut reader).unwrap(), Some("GET course".into()));
        assert_eq!(read_frame(&mut reader).unwrap(), Some("STATUS".into()));
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }

    #[test]
    fn reply_fields_cannot_break_the_line_protocol() {
        assert_eq!(
            format_reply(Reply::Value("line1\nline2".into())),
            "VALUE\tline1\\nline2"
        );
    }

    #[test]
    fn parses_clear_and_confirmation_tokens() {
        assert_eq!(parse_request("CLEAR").unwrap(), Request::Clear);
        assert_eq!(parse_request("YES").unwrap(), Request::Yes);
        assert_eq!(parse_request("yes").unwrap(), Request::Yes);
        assert_eq!(parse_request("NO").unwrap(), Request::No);
        assert!(matches!(
            parse_request("CLEAR extra"),
            Err(ProtocolError::ExtraArguments)
        ));
        assert!(matches!(
            parse_request("YES extra"),
            Err(ProtocolError::ExtraArguments)
        ));
    }

    #[test]
    fn formats_enhanced_server_status() {
        let metrics = MetricsSnapshot {
            active_clients: 2,
            total_connections: 5,
            commands_total: 10,
            commands_failed: 3,
            checkpoint_attempts: 2,
            checkpoint_failures: 1,
            bytes_received: 120,
            bytes_sent: 240,
            uptime_seconds: 60,
        };

        assert_eq!(
            format_server_status(4, 1024, 6, 1000, &metrics),
            "STATUS\trunning\tkeys=4\tactive_clients=2\ttotal_connections=5\tcommands_total=10\tcommands_succeeded=7\tcommands_failed=3\tcheckpoint_attempts=2\tcheckpoint_failures=1\twrites_since_checkpoint=6\tcheckpoint_after_writes=1000\tbytes_received=120\tbytes_sent=240\twal_bytes=1024\tuptime_seconds=60"
        );
    }
}
