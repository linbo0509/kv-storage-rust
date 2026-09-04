//! 多客户端 TCP 服务器。
//!
//! 每个连接由独立线程负责网络收发；所有线程通过 `Arc<Mutex<Engine>>` 共享
//! 引擎，保证内存、WAL 和快照的修改顺序一致。

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::domain::Command;
use crate::engine::{Engine, EngineError};
use crate::metrics::ServerMetrics;
use crate::protocol::{
    Request, format_error, format_reply, format_server_status, parse_request, read_frame,
    terminate_frame,
};

/// TCP 服务器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub address: String,
    pub data_dir: PathBuf,
    /// 累计多少次修改后自动生成快照；0 表示关闭。
    pub checkpoint_after_writes: u64,
}

pub const DEFAULT_CHECKPOINT_AFTER_WRITES: u64 = 1_000;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:7878".into(),
            data_dir: PathBuf::from("data"),
            checkpoint_after_writes: DEFAULT_CHECKPOINT_AFTER_WRITES,
        }
    }
}

type SharedEngine = Arc<Mutex<Engine>>;
type SharedMetrics = Arc<ServerMetrics>;

/// 启动服务器；每接受一个连接，就创建一个独立工作线程。
///
/// 网络读取和响应发送不占用引擎锁，只有执行单条命令时才进入临界区。
pub fn run(config: ServerConfig) -> Result<(), ServerError> {
    let listener = TcpListener::bind(&config.address)
        .map_err(|source| ServerError::io("绑定监听地址", source))?;
    let engine = Engine::open(&config.data_dir).map_err(ServerError::Engine)?;

    println!("KV 服务器已启动：{}", config.address);
    println!("数据目录：{}", config.data_dir.display());
    println!("已恢复 {} 个键", engine.status().key_count);
    println!("当前为多客户端线程模式");

    // Arc 负责跨线程共享所有权，Mutex 保证内存、WAL 和快照作为一个整体串行修改。
    let engine = Arc::new(Mutex::new(engine));
    let metrics = Arc::new(ServerMetrics::new());
    let mut worker_id = 0_u64;

    loop {
        let (stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) => {
                // 一次 accept 失败不应导致整个服务进程退出。
                eprintln!("接受客户端连接失败：{error}");
                continue;
            }
        };

        println!("客户端已连接：{peer}");
        worker_id = worker_id.wrapping_add(1);
        let worker_engine = Arc::clone(&engine);
        let worker_metrics = Arc::clone(&metrics);
        let checkpoint_after_writes = config.checkpoint_after_writes;
        let thread_name = format!("kv-client-{worker_id}");

        match thread::Builder::new().name(thread_name).spawn(move || {
            if let Err(error) = handle_client(
                stream,
                &worker_engine,
                &worker_metrics,
                checkpoint_after_writes,
            ) {
                eprintln!("客户端 {peer} 会话异常：{error}");
            }
            println!("客户端已断开：{peer}");
        }) {
            Ok(_worker) => {
                // JoinHandle 被丢弃后线程继续独立运行；优雅关闭阶段再统一管理工作线程。
            }
            Err(error) => {
                // 线程资源暂时不足只影响当前连接，监听循环仍然继续。
                eprintln!("无法为客户端 {peer} 创建工作线程：{error}");
            }
        }
    }
}

/// 接受并处理恰好一个客户端，主要供集成测试和后续服务器扩展复用。
#[cfg(test)]
fn serve_one(listener: TcpListener, data_dir: impl AsRef<Path>) -> Result<(), ServerError> {
    let engine = Arc::new(Mutex::new(
        Engine::open(data_dir).map_err(ServerError::Engine)?,
    ));
    let metrics = Arc::new(ServerMetrics::new());
    let (stream, _) = listener
        .accept()
        .map_err(|source| ServerError::io("接受客户端连接", source))?;
    handle_client(stream, &engine, &metrics, DEFAULT_CHECKPOINT_AFTER_WRITES)
}

fn handle_client(
    stream: TcpStream,
    engine: &SharedEngine,
    metrics: &SharedMetrics,
    checkpoint_after_writes: u64,
) -> Result<(), ServerError> {
    let peer = stream
        .peer_addr()
        .map_err(|source| ServerError::io("读取客户端地址", source))?;
    let _connection = metrics.connection_opened();
    let reader_stream = stream
        .try_clone()
        .map_err(|source| ServerError::io("复制客户端连接", source))?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    loop {
        let line = match read_frame(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                // 超长帧、非法 UTF-8 或半条命令都无法安全地继续解析当前连接。
                metrics.record_command(0);
                metrics.record_command_failure();
                let response = format_error("PROTOCOL", &error.to_string());
                eprintln!("[实时反馈] 客户端={peer} | 协议错误={error}");
                let _ = write_response(&mut writer, &response, metrics);
                return Ok(());
            }
        };
        metrics.record_command(line.len() as u64 + 1);
        let started_at = Instant::now();

        let request = match parse_request(&line) {
            Ok(request) => request,
            Err(error) => {
                // 命令格式错误只影响本次请求，连接仍然可继续使用。
                metrics.record_command_failure();
                let response = format_error("INVALID_COMMAND", &error.to_string());
                let operation = format!("INVALID input={:?}", truncate_for_log(&line));
                log_operation(peer, &operation, &response, started_at.elapsed());
                write_response(&mut writer, &response, metrics)?;
                continue;
            }
        };

        let operation = describe_request(&request);
        let should_close = matches!(request, Request::Quit);
        let response = handle_request(request, engine, metrics, checkpoint_after_writes);
        if response.starts_with("ERR\t") {
            metrics.record_command_failure();
        }
        log_operation(peer, &operation, &response, started_at.elapsed());
        write_response(&mut writer, &response, metrics)?;
        if should_close {
            return Ok(());
        }
    }
}

/// 生成适合服务器终端展示的操作摘要，不打印可能很长的 value。
fn describe_request(request: &Request) -> String {
    match request {
        Request::Execute(Command::Set { key, .. }) => format!("SET key={key}"),
        Request::Execute(Command::Update { key, .. }) => format!("UPDATE key={key}"),
        Request::Execute(Command::Get { key }) => format!("GET key={key}"),
        Request::Execute(Command::Delete { key }) => format!("DELETE key={key}"),
        Request::Execute(Command::Keys) => "KEYS".into(),
        Request::Execute(Command::Status) => "STATUS".into(),
        Request::Save => "SAVE".into(),
        Request::Quit => "QUIT".into(),
    }
}

/// 把协议响应压缩为“成功/失败”摘要，避免 STATUS 的长响应刷满服务器终端。
fn summarize_response(response: &str) -> String {
    let mut fields = response.split('\t');
    match fields.next() {
        Some("ERR") => format!("失败({})", fields.next().unwrap_or("UNKNOWN")),
        Some("OK") => format!("成功({})", fields.next().unwrap_or("OK")),
        Some("VALUE") => "成功(已返回值)".into(),
        Some("KEYS") => format!("成功(返回 {} 个键)", fields.next().unwrap_or("?")),
        Some("STATUS") => "成功(已返回状态)".into(),
        _ => "已响应".into(),
    }
}

fn truncate_for_log(line: &str) -> String {
    const MAX_LOG_CHARS: usize = 80;
    let mut chars = line.chars();
    let mut summary: String = chars.by_ref().take(MAX_LOG_CHARS).collect();
    if chars.next().is_some() {
        summary.push('…');
    }
    summary
}

/// 每条命令完成后立即输出并刷新，便于在答辩现场观察客户端操作。
fn log_operation(peer: SocketAddr, operation: &str, response: &str, elapsed: Duration) {
    let latency = if elapsed.as_micros() < 1_000 {
        "<1 ms".into()
    } else {
        format!("{} ms", elapsed.as_millis())
    };
    println!(
        "[实时反馈] 客户端={peer} | 操作={operation} | 结果={} | 耗时={latency}",
        summarize_response(response)
    );
    let _ = io::stdout().flush();
}

fn handle_request(
    request: Request,
    engine: &SharedEngine,
    metrics: &SharedMetrics,
    checkpoint_after_writes: u64,
) -> String {
    match request {
        Request::Execute(Command::Status) => {
            let engine = match engine.lock() {
                Ok(engine) => engine,
                Err(_) => return poisoned_engine_response(),
            };
            let wal_bytes = match engine.wal_size_bytes() {
                Ok(bytes) => bytes,
                Err(error) => return format_error("PERSISTENCE", &error.to_string()),
            };
            format_server_status(
                engine.status().key_count,
                wal_bytes,
                engine.writes_since_checkpoint(),
                checkpoint_after_writes,
                &metrics.snapshot(),
            )
        }
        Request::Execute(command) => {
            let mut engine = match engine.lock() {
                Ok(engine) => engine,
                Err(_) => return poisoned_engine_response(),
            };
            match engine.execute_with_auto_checkpoint(command, checkpoint_after_writes) {
                Ok(outcome) => {
                    let mut response = format_reply(outcome.reply);
                    if let Some(checkpoint) = outcome.auto_checkpoint {
                        match checkpoint {
                            Ok(()) => metrics.record_checkpoint(true),
                            Err(error) => {
                                metrics.record_checkpoint(false);
                                eprintln!("自动快照失败，完整 WAL 已保留：{error}");
                                response.push_str("\tWARNING=AUTO_CHECKPOINT_FAILED");
                            }
                        }
                    }
                    response
                }
                Err(EngineError::Domain(error)) => format_error("DOMAIN", &error.to_string()),
                Err(EngineError::Persistence(error)) => {
                    format_error("PERSISTENCE", &error.to_string())
                }
            }
        }
        Request::Save => {
            let mut engine = match engine.lock() {
                Ok(engine) => engine,
                Err(_) => return poisoned_engine_response(),
            };
            match engine.checkpoint() {
                Ok(()) => {
                    metrics.record_checkpoint(true);
                    "OK	SNAPSHOT_SAVED".into()
                }
                Err(error) => {
                    metrics.record_checkpoint(false);
                    format_error("PERSISTENCE", &error.to_string())
                }
            }
        }
        Request::Quit => "OK	BYE".into(),
    }
}

fn poisoned_engine_response() -> String {
    // 不用 into_inner 强行继续：发生 panic 后无法证明 WAL 与内存仍保持一致。
    format_error(
        "INTERNAL",
        "共享存储引擎锁已中毒，请重启服务器并执行数据恢复",
    )
}

fn write_response(
    stream: &mut TcpStream,
    response: &str,
    metrics: &ServerMetrics,
) -> Result<(), ServerError> {
    stream
        .write_all(terminate_frame(response).as_bytes())
        .map_err(|source| ServerError::io("发送服务器响应", source))?;
    stream
        .flush()
        .map_err(|source| ServerError::io("刷新服务器响应", source))?;
    metrics.record_response(response.len() as u64 + 1);
    Ok(())
}

#[derive(Debug)]
pub enum ServerError {
    Io {
        action: &'static str,
        source: std::io::Error,
    },
    Engine(EngineError),
}

impl ServerError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

impl Display for ServerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { action, source } => write!(formatter, "{action}失败：{source}"),
            Self::Engine(error) => write!(formatter, "存储引擎错误：{error}"),
        }
    }
}

impl Error for ServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Engine(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use crate::client::Client;
    use crate::domain::{Command, Reply};

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kv-storage-server-test-{}-{id}",
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

    fn serve_clients_for_test(listener: TcpListener, data_dir: PathBuf, client_count: usize) {
        let engine = Arc::new(Mutex::new(Engine::open(data_dir).unwrap()));
        let metrics = Arc::new(ServerMetrics::new());
        let mut workers = Vec::with_capacity(client_count);

        for _ in 0..client_count {
            let (stream, _) = listener.accept().unwrap();
            let worker_engine = Arc::clone(&engine);
            let worker_metrics = Arc::clone(&metrics);
            workers.push(thread::spawn(move || {
                handle_client(
                    stream,
                    &worker_engine,
                    &worker_metrics,
                    DEFAULT_CHECKPOINT_AFTER_WRITES,
                )
            }));
        }

        for worker in workers {
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn one_client_can_use_the_server_and_data_survives() {
        let test_dir = TestDir::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let data_dir = test_dir.path().to_path_buf();

        let server = thread::spawn(move || serve_one(listener, data_dir));
        let mut client = Client::connect(&address.to_string()).unwrap();

        assert_eq!(
            client.send("SET course Rust 程序设计").unwrap(),
            "OK	CREATED"
        );
        assert_eq!(client.send("GET course").unwrap(), "VALUE	Rust 程序设计");
        assert!(
            client
                .send("UNKNOWN")
                .unwrap()
                .starts_with("ERR	INVALID_COMMAND	")
        );
        let status = client.send("STATUS").unwrap();
        assert!(status.starts_with("STATUS\trunning\tkeys=1\t"));
        assert!(status.contains("\tactive_clients=1\t"));
        assert!(status.contains("\ttotal_connections=1\t"));
        assert!(status.contains("\tcommands_total=4\t"));
        assert!(status.contains("\tcommands_succeeded=3\t"));
        assert!(status.contains("\tcommands_failed=1\t"));
        assert!(status.contains("\tcheckpoint_attempts=0\t"));
        assert!(status.contains("\tcheckpoint_failures=0\t"));
        assert!(status.contains("\twrites_since_checkpoint=1\t"));
        assert!(status.contains("\tcheckpoint_after_writes=1000\t"));
        assert!(status.contains("\tbytes_received="));
        assert!(status.contains("\tbytes_sent="));
        assert!(status.contains("\twal_bytes="));
        assert!(status.contains("\tuptime_seconds="));
        assert_eq!(client.send("SAVE").unwrap(), "OK	SNAPSHOT_SAVED");
        assert_eq!(client.send("QUIT").unwrap(), "OK	BYE");

        server.join().unwrap().unwrap();
        assert!(test_dir.path().join("snapshot.json").is_file());

        let mut recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(
            recovered
                .execute(Command::Get {
                    key: "course".into(),
                })
                .unwrap(),
            Reply::Value("Rust 程序设计".into())
        );
    }

    #[test]
    fn multiple_clients_can_write_concurrently_without_losing_data() {
        const CLIENT_COUNT: usize = 6;
        const KEYS_PER_CLIENT: usize = 10;

        let test_dir = TestDir::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let data_dir = test_dir.path().to_path_buf();
        let server = thread::spawn(move || {
            serve_clients_for_test(listener, data_dir, CLIENT_COUNT);
        });

        // 屏障让所有客户端连接后同时开始，避免测试退化成顺序执行。
        let start = Arc::new(Barrier::new(CLIENT_COUNT));
        let mut clients = Vec::with_capacity(CLIENT_COUNT);
        for client_id in 0..CLIENT_COUNT {
            let address = address.clone();
            let start = Arc::clone(&start);
            clients.push(thread::spawn(move || {
                let mut client = Client::connect(&address).unwrap();
                start.wait();

                assert!(
                    client
                        .send(&format!("SET shared client-{client_id}"))
                        .unwrap()
                        .starts_with("OK\t")
                );

                for key_id in 0..KEYS_PER_CLIENT {
                    let key = format!("client-{client_id}-key-{key_id}");
                    let value = format!("value-{client_id}-{key_id}");
                    assert_eq!(
                        client.send(&format!("SET {key} {value}")).unwrap(),
                        "OK\tCREATED"
                    );
                    assert_eq!(
                        client.send(&format!("GET {key}")).unwrap(),
                        format!("VALUE\t{value}")
                    );
                }
                assert_eq!(client.send("QUIT").unwrap(), "OK\tBYE");
            }));
        }

        for client in clients {
            client.join().unwrap();
        }
        server.join().unwrap();

        // 不依赖快照，直接重放并发写入产生的 WAL，验证记录没有交叉或丢失。
        let recovered = Engine::open(test_dir.path()).unwrap();
        assert_eq!(
            recovered.status().key_count,
            CLIENT_COUNT * KEYS_PER_CLIENT + 1
        );
    }

    #[test]
    fn server_automatically_snapshots_and_compacts_wal() {
        let test_dir = TestDir::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let data_dir = test_dir.path().to_path_buf();

        let server = thread::spawn(move || {
            let engine = Arc::new(Mutex::new(Engine::open(data_dir).unwrap()));
            let metrics = Arc::new(ServerMetrics::new());
            let (stream, _) = listener.accept().unwrap();
            handle_client(stream, &engine, &metrics, 2).unwrap();
        });

        let mut client = Client::connect(&address).unwrap();
        assert_eq!(client.send("SET course Rust").unwrap(), "OK\tCREATED");
        assert_eq!(client.send("SET teacher 李老师").unwrap(), "OK\tCREATED");

        let status = client.send("STATUS").unwrap();
        assert!(status.contains("\tcheckpoint_attempts=1\t"));
        assert!(status.contains("\tcheckpoint_failures=0\t"));
        assert!(status.contains("\twrites_since_checkpoint=0\t"));
        assert!(status.contains("\tcheckpoint_after_writes=2\t"));
        assert_eq!(client.send("QUIT").unwrap(), "OK\tBYE");
        server.join().unwrap();

        assert!(test_dir.path().join("snapshot.json").is_file());
        assert_eq!(
            fs::read_to_string(test_dir.path().join("wal.log")).unwrap(),
            "RUST_KV_WAL\t2\t2\n"
        );
        assert_eq!(Engine::open(test_dir.path()).unwrap().status().key_count, 2);
    }

    #[test]
    fn poisoned_engine_lock_returns_an_error_instead_of_panicking() {
        let test_dir = TestDir::new();
        let engine = Arc::new(Mutex::new(Engine::open(test_dir.path()).unwrap()));
        let metrics = Arc::new(ServerMetrics::new());
        let worker_engine = Arc::clone(&engine);

        let failed_worker = thread::spawn(move || {
            let _guard = worker_engine.lock().unwrap();
            panic!("模拟工作线程在持锁期间异常退出");
        });
        assert!(failed_worker.join().is_err());

        assert_eq!(
            handle_request(
                Request::Execute(Command::Status),
                &engine,
                &metrics,
                DEFAULT_CHECKPOINT_AFTER_WRITES,
            ),
            "ERR\tINTERNAL\t共享存储引擎锁已中毒，请重启服务器并执行数据恢复"
        );
    }
}
