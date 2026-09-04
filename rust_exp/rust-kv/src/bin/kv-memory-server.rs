//! Rust/Java 对照实验使用的纯内存 Rust KV 服务器。
//!
//! 本程序刻意关闭 WAL、快照、指标和逐条日志，只保留 TCP、命令解析、共享锁和
//! HashMap，避免磁盘或终端 IO 掩盖内存管理差异。

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use kv_storage_rust::domain::{Command, Reply};
use kv_storage_rust::protocol::{
    Request, format_error, format_reply, parse_request, read_frame, terminate_frame,
};

type SharedStore = Arc<Mutex<HashMap<String, String>>>;

fn main() {
    let address = match parse_address(std::env::args().skip(1)) {
        Ok(address) => address,
        Err(message) => {
            eprintln!("参数错误：{message}");
            eprintln!("用法：kv-memory-server [--addr IP:PORT]");
            std::process::exit(2);
        }
    };

    if let Err(error) = run(&address) {
        eprintln!("纯内存服务器错误：{error}");
        std::process::exit(1);
    }
}

fn run(address: &str) -> Result<(), std::io::Error> {
    let listener = TcpListener::bind(address)?;
    let store = Arc::new(Mutex::new(HashMap::new()));

    println!("Rust 纯内存 KV 已启动：{}", listener.local_addr()?);
    println!("实验模式：无 WAL、无快照、无逐条日志");

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("接受连接失败：{error}");
                continue;
            }
        };
        let worker_store = Arc::clone(&store);
        thread::spawn(move || {
            if let Err(error) = handle_client(stream, &worker_store) {
                eprintln!("客户端会话错误：{error}");
            }
        });
    }
    Ok(())
}

fn handle_client(stream: TcpStream, store: &SharedStore) -> Result<(), std::io::Error> {
    stream.set_nodelay(true)?;
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;

    loop {
        let line = match read_frame(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_response(&mut writer, &format_error("PROTOCOL", &error.to_string()))?;
                return Ok(());
            }
        };
        let request = match parse_request(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut writer,
                    &format_error("INVALID_COMMAND", &error.to_string()),
                )?;
                continue;
            }
        };
        let should_close = matches!(request, Request::Quit);
        let response = execute_request(request, store);
        write_response(&mut writer, &response)?;
        if should_close {
            return Ok(());
        }
    }
}

fn execute_request(request: Request, store: &SharedStore) -> String {
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return format_error("INTERNAL", "共享内存锁已中毒"),
    };

    match request {
        Request::Execute(Command::Set { key, value }) => {
            store.insert(key, value);
            "OK".into()
        }
        Request::Execute(Command::Update { key, value }) => match store.get_mut(&key) {
            Some(stored) => {
                *stored = value;
                "OK".into()
            }
            None => format_error("NOT_FOUND", &key),
        },
        Request::Execute(Command::Get { key }) => match store.get(&key) {
            Some(value) => format_reply(Reply::Value(value.clone())),
            None => format_error("NOT_FOUND", &key),
        },
        Request::Execute(Command::Delete { key }) => match store.remove(&key) {
            Some(_) => "OK".into(),
            None => format_error("NOT_FOUND", &key),
        },
        Request::Execute(Command::Keys) => {
            let mut keys: Vec<String> = store.keys().cloned().collect();
            keys.sort();
            format_reply(Reply::Keys(keys))
        }
        Request::Execute(Command::Status) => format!("STATUS\trunning\tkeys={}", store.len()),
        Request::Save => format_error("UNSUPPORTED", "纯内存实验模式不支持 SAVE"),
        Request::Quit => "OK\tBYE".into(),
    }
}

fn write_response(stream: &mut TcpStream, response: &str) -> Result<(), std::io::Error> {
    stream.write_all(terminate_frame(response).as_bytes())?;
    stream.flush()
}

fn parse_address(args: impl Iterator<Item = String>) -> Result<String, String> {
    let args: Vec<String> = args.collect();
    match args.as_slice() {
        [] => Ok("127.0.0.1:7878".into()),
        [flag, address] if flag == "--addr" => Ok(address.clone()),
        _ => Err("只支持可选参数 --addr IP:PORT".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_overwrites_without_growing_the_key_count() {
        let store = Arc::new(Mutex::new(HashMap::new()));
        assert_eq!(
            execute_request(
                Request::Execute(Command::Set {
                    key: "course".into(),
                    value: "Rust".into(),
                }),
                &store,
            ),
            "OK"
        );
        assert_eq!(
            execute_request(
                Request::Execute(Command::Set {
                    key: "course".into(),
                    value: "Java".into(),
                }),
                &store,
            ),
            "OK"
        );
        assert_eq!(store.lock().unwrap().len(), 1);
    }
}
