//! 同步 TCP 客户端及交互式命令行界面。

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, BufReader, Write};
use std::net::TcpStream;

use crate::protocol::{MAX_FRAME_BYTES, ProtocolError, read_frame, terminate_frame};

/// 同步 TCP 客户端。一条连接上严格按“发送一个请求、读取一个响应”的顺序工作。
pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Client {
    pub fn connect(address: &str) -> Result<Self, ClientError> {
        let writer =
            TcpStream::connect(address).map_err(|source| ClientError::io("连接服务器", source))?;
        let reader_stream = writer
            .try_clone()
            .map_err(|source| ClientError::io("复制服务器连接", source))?;

        Ok(Self {
            reader: BufReader::new(reader_stream),
            writer,
        })
    }

    /// 发送一条命令并等待服务器的一行响应。
    pub fn send(&mut self, command: &str) -> Result<String, ClientError> {
        if command.contains(['\n', '\r']) {
            return Err(ClientError::InvalidCommand(
                "一条命令中不能包含换行符".into(),
            ));
        }
        if command.len() + 1 > MAX_FRAME_BYTES {
            return Err(ClientError::InvalidCommand("命令超过 64 KiB 限制".into()));
        }

        self.writer
            .write_all(terminate_frame(command).as_bytes())
            .map_err(|source| ClientError::io("发送请求", source))?;
        self.writer
            .flush()
            .map_err(|source| ClientError::io("刷新请求", source))?;

        read_frame(&mut self.reader)
            .map_err(ClientError::Protocol)?
            .ok_or(ClientError::ConnectionClosed)
    }
}

/// 启动交互式命令行客户端。
pub fn run_repl(address: &str) -> Result<(), ClientError> {
    let mut client = Client::connect(address)?;
    println!("已连接服务器：{address}");
    print_help();

    loop {
        print!("remote-kv> ");
        io::stdout()
            .flush()
            .map_err(|source| ClientError::io("刷新终端输出", source))?;

        let mut input = String::new();
        let bytes_read = io::stdin()
            .read_line(&mut input)
            .map_err(|source| ClientError::io("读取终端输入", source))?;
        if bytes_read == 0 {
            println!("\n输入已关闭，客户端退出");
            return Ok(());
        }

        let command = input.trim();
        if command.eq_ignore_ascii_case("HELP") {
            print_help();
            continue;
        }
        if command.is_empty() {
            println!("请输入命令；输入 HELP 查看帮助");
            continue;
        }

        let is_quit = command.eq_ignore_ascii_case("QUIT") || command.eq_ignore_ascii_case("EXIT");
        let response = client.send(command)?;
        println!("{}", format_response_for_display(&response));
        if is_quit {
            return Ok(());
        }
    }
}

/// 将服务器的单行协议响应转换为适合终端阅读的文本。
fn format_response_for_display(response: &str) -> String {
    format_status_for_display(response).unwrap_or_else(|| response.replace('\t', "  "))
}

/// STATUS 在线路上保持单行，在客户端按主题分组展示。
fn format_status_for_display(response: &str) -> Option<String> {
    let mut parts = response.split('\t');
    if parts.next()? != "STATUS" {
        return None;
    }

    let state = match parts.next()? {
        "running" => "正常运行",
        other => other,
    };
    let fields: BTreeMap<&str, &str> = parts.filter_map(|field| field.split_once('=')).collect();
    let get = |name| fields.get(name).copied().unwrap_or("未知");
    let checkpoint_threshold = match get("checkpoint_after_writes") {
        "0" => "已关闭".into(),
        value => format!("{value} 次写入"),
    };

    let lines = [
        "========== 服务器状态 ==========".to_string(),
        "[运行]".to_string(),
        format!("  状态：{state}"),
        format!("  已运行：{}", format_duration(get("uptime_seconds"))),
        "[数据与持久化]".to_string(),
        format!("  键数量：{}", get("keys")),
        format!("  WAL 大小：{}", format_bytes(get("wal_bytes"))),
        format!("  快照后写入：{} 次", get("writes_since_checkpoint")),
        format!("  自动快照阈值：{checkpoint_threshold}"),
        format!(
            "  快照尝试 / 失败：{} / {}",
            get("checkpoint_attempts"),
            get("checkpoint_failures")
        ),
        "[连接]".to_string(),
        format!("  当前客户端：{}", get("active_clients")),
        format!("  累计连接：{}", get("total_connections")),
        "[命令]".to_string(),
        format!(
            "  总计 / 成功 / 失败：{} / {} / {}",
            get("commands_total"),
            get("commands_succeeded"),
            get("commands_failed")
        ),
        "[网络流量]".to_string(),
        format!("  接收：{}", format_bytes(get("bytes_received"))),
        format!("  发送：{}", format_bytes(get("bytes_sent"))),
        "================================".to_string(),
    ];
    Some(lines.join("\n"))
}

fn format_bytes(raw: &str) -> String {
    let Ok(bytes) = raw.parse::<u64>() else {
        return raw.to_string();
    };
    if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.2} KiB（{bytes} B）", bytes as f64 / 1_024.0)
    } else {
        format!("{:.2} MiB（{bytes} B）", bytes as f64 / (1_024.0 * 1_024.0))
    }
}

fn format_duration(raw: &str) -> String {
    let Ok(total_seconds) = raw.parse::<u64>() else {
        return raw.to_string();
    };
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours} 小时 {minutes} 分 {seconds} 秒")
    } else if minutes > 0 {
        format!("{minutes} 分 {seconds} 秒")
    } else {
        format!("{seconds} 秒")
    }
}

fn print_help() {
    println!("可用命令：");
    println!("  SET key value     写入或覆盖");
    println!("  UPDATE key value  修改已有键");
    println!("  GET key           查询");
    println!("  DELETE key        删除");
    println!("  KEYS              列出所有键");
    println!("  STATUS            查看状态");
    println!("  SAVE              生成 JSON 快照并压缩 WAL");
    println!("  CLEAR             清空全部数据（需二次确认）");
    println!("  HELP              显示帮助（客户端本地命令）");
    println!("  EXIT / QUIT       断开连接并退出");
}

#[derive(Debug)]
pub enum ClientError {
    Io {
        action: &'static str,
        source: std::io::Error,
    },
    Protocol(ProtocolError),
    ConnectionClosed,
    InvalidCommand(String),
}

impl ClientError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

impl Display for ClientError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { action, source } => write!(formatter, "{action}失败：{source}"),
            Self::Protocol(error) => write!(formatter, "服务器响应格式错误：{error}"),
            Self::ConnectionClosed => write!(formatter, "服务器在返回响应前关闭了连接"),
            Self::InvalidCommand(message) => write!(formatter, "命令无效：{message}"),
        }
    }
}

impl Error for ClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Protocol(error) => Some(error),
            Self::ConnectionClosed | Self::InvalidCommand(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_grouped_for_terminal_display() {
        let response = "STATUS\trunning\tkeys=4\tactive_clients=2\ttotal_connections=5\tcommands_total=10\tcommands_succeeded=7\tcommands_failed=3\tcheckpoint_attempts=2\tcheckpoint_failures=1\twrites_since_checkpoint=6\tcheckpoint_after_writes=1000\tbytes_received=120\tbytes_sent=2048\twal_bytes=1024\tuptime_seconds=65";
        let display = format_response_for_display(response);

        assert!(display.contains("[运行]\n  状态：正常运行\n  已运行：1 分 5 秒"));
        assert!(display.contains("[数据与持久化]"));
        assert!(display.contains("WAL 大小：1.00 KiB（1024 B）"));
        assert!(display.contains("快照尝试 / 失败：2 / 1"));
        assert!(display.contains("[命令]\n  总计 / 成功 / 失败：10 / 7 / 3"));
        assert!(display.contains("发送：2.00 KiB（2048 B）"));
    }

    #[test]
    fn ordinary_response_keeps_the_compact_display() {
        assert_eq!(format_response_for_display("OK\tCREATED"), "OK  CREATED");
    }
}
