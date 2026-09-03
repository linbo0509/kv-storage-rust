//! 本地单机版命令行入口。
//!
//! 它与 TCP 服务器复用同一个 Engine 和命令协议，适合在不启动网络服务时
//! 学习、调试领域逻辑与持久化流程。

use std::io::{self, Write};
use std::path::PathBuf;

use kv_storage_rust::domain::{Command, DomainError, Reply, SetOutcome};
use kv_storage_rust::engine::{Engine, EngineError};
use kv_storage_rust::protocol::{Request, parse_request};

enum LocalCommand {
    Domain(Command),
    Save,
    Help,
    Exit,
}

fn main() {
    let data_dir = match parse_data_dir(std::env::args().skip(1)) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("启动失败：{message}");
            std::process::exit(2);
        }
    };
    let mut engine = match Engine::open(&data_dir) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("启动失败：{error}");
            eprintln!("数据文件未被清空或覆盖，请检查后重试。");
            std::process::exit(2);
        }
    };

    println!("Rust 本地键值存储（JSON 快照 + WAL 版）");
    println!("数据目录：{}", data_dir.display());
    println!("已恢复 {} 个键", engine.status().key_count);
    print_help();

    loop {
        print!("kv> ");
        if let Err(error) = io::stdout().flush() {
            eprintln!("无法刷新终端输出：{error}");
            break;
        }

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                save_snapshot(&mut engine);
                println!("\n输入已关闭，程序退出");
                break;
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("无法读取命令：{error}");
                continue;
            }
        }

        let command = match parse_local_command(&input) {
            Ok(command) => command,
            Err(message) => {
                println!("ERR INVALID_COMMAND {message}");
                continue;
            }
        };

        match command {
            LocalCommand::Domain(command) => match engine.execute(command) {
                Ok(reply) => print_reply(reply),
                Err(EngineError::Domain(error)) => print_domain_error(error),
                Err(EngineError::Persistence(error)) => {
                    println!("ERR PERSISTENCE {error}")
                }
            },
            LocalCommand::Save => save_snapshot(&mut engine),
            LocalCommand::Help => print_help(),
            LocalCommand::Exit => {
                save_snapshot(&mut engine);
                println!("BYE");
                break;
            }
        }
    }
}

fn save_snapshot(engine: &mut Engine) {
    match engine.checkpoint() {
        Ok(()) => println!("OK SNAPSHOT_SAVED"),
        Err(error) => {
            // 快照失败时 WAL 仍保留完整记录，因此数据不会被静默清空。
            println!("ERR SNAPSHOT {error}");
        }
    }
}

fn parse_data_dir(args: impl Iterator<Item = String>) -> Result<PathBuf, String> {
    let args: Vec<String> = args.collect();
    match args.as_slice() {
        [] => Ok(PathBuf::from("data")),
        [flag, path] if flag == "--data-dir" => Ok(PathBuf::from(path)),
        _ => Err("用法：kv_storage_rust [--data-dir PATH]".into()),
    }
}

fn parse_local_command(input: &str) -> Result<LocalCommand, String> {
    let input = input.trim();
    if input.eq_ignore_ascii_case("HELP") {
        return Ok(LocalCommand::Help);
    }

    // 本地版和网络版共用同一套解析规则，避免两处语法逐渐不一致。
    match parse_request(input).map_err(|error| error.to_string())? {
        Request::Execute(command) => Ok(LocalCommand::Domain(command)),
        Request::Save => Ok(LocalCommand::Save),
        Request::Quit => Ok(LocalCommand::Exit),
    }
}

fn print_reply(reply: Reply) {
    match reply {
        Reply::Set(SetOutcome::Created) => println!("OK CREATED"),
        Reply::Set(SetOutcome::Overwritten { old_value }) => {
            println!("OK OVERWRITTEN old={old_value}")
        }
        Reply::Updated { old_value } => println!("OK UPDATED old={old_value}"),
        Reply::Value(value) => println!("VALUE {value}"),
        Reply::Deleted { value } => println!("OK DELETED value={value}"),
        Reply::Keys(keys) => {
            println!("KEYS {}", keys.len());
            for key in keys {
                println!("  {key}");
            }
        }
        Reply::Status(status) => println!("STATUS running keys={}", status.key_count),
    }
}

fn print_domain_error(error: DomainError) {
    match error {
        DomainError::EmptyKey => println!("ERR EMPTY_KEY key 不能为空"),
        DomainError::NotFound { key } => println!("ERR NOT_FOUND {key}"),
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
    println!("  HELP              显示帮助");
    println!("  EXIT              退出");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_value_containing_spaces() {
        let LocalCommand::Domain(Command::Set { key, value }) =
            parse_local_command("SET course Rust 程序设计").unwrap()
        else {
            panic!("expected SET command");
        };

        assert_eq!(key, "course");
        assert_eq!(value, "Rust 程序设计");
    }

    #[test]
    fn rejects_missing_and_extra_arguments() {
        assert!(parse_local_command("SET course").is_err());
        assert!(parse_local_command("GET course extra").is_err());
        assert!(parse_local_command("STATUS extra").is_err());
        assert!(parse_local_command("SAVE extra").is_err());
    }

    #[test]
    fn parses_the_optional_data_directory() {
        assert_eq!(
            parse_data_dir(Vec::<String>::new().into_iter()).unwrap(),
            PathBuf::from("data")
        );
        assert_eq!(
            parse_data_dir(vec!["--data-dir".into(), "test-data".into()].into_iter()).unwrap(),
            PathBuf::from("test-data")
        );
    }
}
