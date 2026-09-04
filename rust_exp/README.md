# Rust 与 Java GC 对照实验 A

本目录是完全独立的纯内存 KV 性能实验，不会修改或读写原项目的数据。两种服务
使用相同的逐行 TCP 协议、线程模型、共享互斥锁和键值工作负载；实验关闭 WAL、
快照和逐条操作日志，重点观察 Java GC 对尾延迟的影响。

## 目录

```text
rust_exp/
├── rust-kv/       # 原 Rust 项目的隔离副本，新增两个实验二进制
├── java-kv/       # IntelliJ IDEA 可直接打开的 Maven Java 项目
└── results/       # CSV 和 Java GC 日志
```

实验程序：

- `kv-memory-server`：Rust 纯内存服务器，默认监听 `127.0.0.1:7878`。
- `JavaKvServer`：Java 纯内存服务器，默认监听 `127.0.0.1:7879`。
- `kv-bench`：两种服务器共用的 Rust 压测客户端。

## 一、构建统一压测工具和 Rust 服务

在终端执行：

```bash
cd /Users/wlffffff/code/rust/rust_exp/rust-kv
cargo build --release --bin kv-memory-server --bin kv-bench
```

## 二、运行 Rust 实验

终端 1：

```bash
./target/release/kv-memory-server --addr 127.0.0.1:7878
```

终端 2：

```bash
./target/release/kv-bench \
  --addr 127.0.0.1:7878 \
  --label rust \
  --clients 32 \
  --keys 50000 \
  --value-size 1024 \
  --warmup-seconds 30 \
  --duration-seconds 60 \
  --output ../results/rust.csv
```

## 三、通过 IntelliJ IDEA 运行 Java

1. 在 IntelliJ IDEA 中打开 `java-kv` 文件夹或其中的 `pom.xml`。
2. 等待 IDE 识别 Maven 项目和 JDK。
3. 选择共享运行配置 `Java KV Server (G1 256MiB)`。
4. 点击运行按钮。

该配置固定使用：

```text
-Xms256m
-Xmx256m
-XX:+UseG1GC
```

GC 日志保存到 `results/java-gc.log`。本机当前安装的是 OpenJDK 26，但项目源码
按 Java 17 语法编译，便于在其他机器复现。

Java 服务启动后，在终端执行同一个压测程序：

```bash
cd /Users/wlffffff/code/rust/rust_exp/rust-kv
./target/release/kv-bench \
  --addr 127.0.0.1:7879 \
  --label java-g1 \
  --clients 32 \
  --keys 50000 \
  --value-size 1024 \
  --warmup-seconds 30 \
  --duration-seconds 60 \
  --output ../results/java-g1.csv
```

## 四、公平性要求

- Rust 和 Java 必须使用完全相同的压测参数。
- 一次只运行一个服务器，避免互相争抢 CPU 和内存。
- 正式实验前关闭其他高负载程序。
- 每种实现至少运行 5 次，交替运行顺序。
- 每轮 Java 实验后保存并重命名 `java-gc.log`，避免下一轮覆盖。
- 不要开启原项目的逐条实时日志，也不要加入 WAL；这些 IO 会掩盖 GC 影响。

当前最小版本采用闭环负载：每个客户端收到响应后才发送下一条请求。它适合先完成
课程对照，但存在“协调遗漏”限制；后续若需要发表级数据，再升级为固定速率负载。

## 五、结果说明

CSV 每秒记录一次吞吐量、P50、P95、P99、P99.9 和最大延迟，并包含总体结果。
重点比较：

1. Java GC 日志中的暂停时刻是否与 Java P99/P99.9 尖峰重合。
2. Rust 与 Java 的总体吞吐量。
3. 两者 P99、P99.9 和最大延迟的稳定性。

实验只能支持“Rust 没有 GC 引起的集中停顿，尾延迟更可预测”这一结论，不能仅凭
一次测试宣称 Rust 在所有场景下都比 Java 快。

## 六、自动生成摘要和图表

完成一轮 Rust 与 Java 正式实验后执行：

```bash
cd /Users/wlffffff/code/rust/rust_exp
python3 analyze_results.py \
  --rust results/rust.csv \
  --java results/java-g1.csv \
  --gc-log results/java-gc.log \
  --output-dir results/comparison
```

将生成：

```text
results/comparison/comparison-summary.md
results/comparison/latency-timeline.svg
```

图中实线表示 P99.9，虚线表示最大延迟，红色竖线表示 Java GC 暂停。
