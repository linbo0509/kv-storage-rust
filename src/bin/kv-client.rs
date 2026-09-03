//! `kv-client` 可执行程序：解析连接地址并启动交互式 TCP 客户端。

use kv_storage_rust::client::run_repl;

fn main() {
    let address = match parse_args(std::env::args().skip(1)) {
        Ok(address) => address,
        Err(message) => {
            eprintln!("参数错误：{message}");
            eprintln!("用法：kv-client [--addr IP:PORT]");
            std::process::exit(2);
        }
    };

    if let Err(error) = run_repl(&address) {
        eprintln!("客户端错误：{error}");
        std::process::exit(1);
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<String, String> {
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
    fn uses_default_or_explicit_address() {
        assert_eq!(
            parse_args(Vec::<String>::new().into_iter()).unwrap(),
            "127.0.0.1:7878"
        );
        assert_eq!(
            parse_args(vec!["--addr".into(), "127.0.0.1:9000".into()].into_iter()).unwrap(),
            "127.0.0.1:9000"
        );
    }
}
