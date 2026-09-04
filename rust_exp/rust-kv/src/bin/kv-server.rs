//! `kv-server` 可执行程序：解析启动参数并运行多客户端服务器。

use std::path::PathBuf;

use kv_storage_rust::server::{ServerConfig, run};

fn main() {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("参数错误：{message}");
            eprintln!("用法：kv-server [--addr IP:PORT] [--data-dir PATH] [--checkpoint-after N]");
            std::process::exit(2);
        }
    };

    if let Err(error) = run(config) {
        eprintln!("服务器启动失败：{error}");
        eprintln!("如果数据文件损坏，原文件不会被静默清空或覆盖。");
        std::process::exit(1);
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<ServerConfig, String> {
    let mut config = ServerConfig::default();
    let mut args = args;

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--addr" => config.address = args.next().ok_or("--addr 缺少 IP:PORT")?,
            "--data-dir" => {
                config.data_dir = PathBuf::from(args.next().ok_or("--data-dir 缺少 PATH")?);
            }
            "--checkpoint-after" => {
                let value = args.next().ok_or("--checkpoint-after 缺少非负整数")?;
                config.checkpoint_after_writes = value
                    .parse()
                    .map_err(|_| "--checkpoint-after 必须是非负整数")?;
            }
            _ => return Err(format!("未知参数：{flag}")),
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_options() {
        let config = parse_args(
            vec![
                "--addr".into(),
                "127.0.0.1:9000".into(),
                "--data-dir".into(),
                "test-data".into(),
                "--checkpoint-after".into(),
                "25".into(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(config.address, "127.0.0.1:9000");
        assert_eq!(config.data_dir, PathBuf::from("test-data"));
        assert_eq!(config.checkpoint_after_writes, 25);
    }
}
