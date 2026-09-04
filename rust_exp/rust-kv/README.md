# Rust 键值存储

当前版本包含领域层、JSON 快照 + WAL 持久化、本地命令行，以及 TCP
服务器和客户端。网络部分支持多个客户端同时保持连接：服务器为每个连接创建
一个工作线程，并通过 `Arc<Mutex<Engine>>` 安全共享存储引擎。

## 目录结构

```text
src/
├── domain/          # 纯业务规则：命令、响应、错误和内存 Store
├── persistence/     # JSON 快照、WAL、校验与损坏检测
├── engine.rs        # 协调业务操作和持久化顺序
├── protocol.rs      # TCP 逐行文本协议
├── server.rs        # 多客户端并发服务器
├── client.rs        # TCP 客户端与交互界面
├── metrics.rs       # 服务器运行指标
├── bin/
│   ├── kv-server.rs # 服务器程序入口
│   └── kv-client.rs # 客户端程序入口
├── main.rs          # 不需要网络的本地单机版入口
└── lib.rs           # 公共模块入口
```

`data/` 和 `server-data/` 是运行时数据目录，保存真实快照和 WAL；`target/`
是 Cargo 自动生成的编译目录，可以随时删除并重新构建。这三类目录都不会提交
到 Git。

## 运行

```bash
cargo run
```

默认数据保存在：

```text
data/
├── snapshot.json
├── snapshot.bak
└── wal.log
```

也可以指定独立数据目录：

```bash
cargo run -- --data-dir ./my-data
```

修改命令会先追加并同步 WAL，再修改内存。执行 `SAVE` 或正常退出时生成
`snapshot.json` 并压缩 WAL；重新启动时先加载快照，再重放快照之后的 WAL。
数据文件损坏时程序拒绝启动，不会静默创建空数据库。

## TCP 服务器和客户端

先在第一个终端启动服务器：

```bash
cargo run --bin kv-server -- --addr 127.0.0.1:7878 --data-dir ./server-data --checkpoint-after 1000
```

再在第二个终端启动客户端：

```bash
cargo run --bin kv-client -- --addr 127.0.0.1:7878
```

请求和响应都使用以换行符分隔的简单文本协议，单条消息最大 64 KiB。
客户端发送 `EXIT` 或 `QUIT` 后只断开当前连接，服务器会继续等待下一个客户端。
修改操作仍然先同步写入 WAL；`SAVE` 会生成 `snapshot.json` 并压缩 WAL。
不同客户端的网络收发可以并行；内存修改、WAL 写入和快照操作按单条命令串行，
避免并发写入破坏数据文件。

服务器会使用原子计数器记录活跃连接、累计连接、命令成功与失败、收发字节和
运行时间。执行 STATUS 可以同时查看这些运行指标、键数量以及 WAL 文件大小；
客户端会按运行、持久化、连接、命令和流量分组展示。每次客户端命令执行后，
服务器终端会立即打印客户端地址、操作摘要、执行结果和耗时，便于现场观察。
指标属于当前服务器进程的运行状态，重启后重新计数，不写入数据库。

服务器默认每累计 1000 次成功修改自动生成 JSON 快照，并在快照安全落盘后压缩
WAL。可以使用 --checkpoint-after N 修改阈值，设置为 0 时关闭自动快照。
自动快照失败不会撤销已经写入 WAL 的命令：客户端会收到维护警告，完整 WAL
继续保留，服务器会在后续命令达到条件时重试。

可用命令：

```text
SET key value
UPDATE key value
GET key
DELETE key
KEYS
STATUS
SAVE
HELP
EXIT
```

## 检查

```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
