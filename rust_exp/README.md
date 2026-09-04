# Rust 与 Java G1 对照实验

本目录提供一套与主项目隔离的纯内存 KV 对照实验，用于观察 Rust 与 Java G1 在
持续对象替换压力下的延迟、CPU、常驻内存和 GC 行为。实验关闭 WAL、快照和逐条
操作日志，不会读取或修改主项目的数据。

## 实验组成

```text
rust_exp/
├── rust-kv/                  # Rust 服务端和两端共用的压测客户端
├── java-kv/                  # Java 17 纯内存服务端
├── run_formal_experiment.py  # 多轮交替实验与资源采样
├── generate_dashboard.py     # 汇总结果并生成四宫格图
├── demo.sh                   # 课堂快速演示入口
└── results/                  # 已归档的正式实验结果
```

两种服务端使用相同的逐行 TCP 协议、每连接一线程模型、`HashMap` 和全局互斥锁，
并由同一个 Rust 压测客户端发送完全相同的请求。

## 环境要求

- Rust stable 工具链；
- JDK 17 或更高版本，能够使用 G1 GC；
- Python 3.10 或更高版本；
- macOS 或带有 `ps` 命令的类 Unix 系统。资源采样脚本目前依赖 `ps`。

## 快速演示

在仓库根目录执行：

```bash
./rust_exp/demo.sh
```

默认执行 1 轮、预热 3 秒、测量 10 秒。可以通过环境变量调整：

```bash
ROUNDS=2 WARMUP_SECONDS=5 DURATION_SECONDS=15 RATE=60000 ./rust_exp/demo.sh
```

演示结果写入 `rust_exp/results/demo-<时间戳>/`，该目录已被 Git 忽略。

## 正式实验

### 1. 构建 Rust 服务端和统一压测客户端

```bash
cargo build --release \
  --manifest-path rust_exp/rust-kv/Cargo.toml \
  --bin kv-memory-server \
  --bin kv-bench
```

### 2. 编译 Java 17 服务端

使用 Maven：

```bash
mvn -q -f rust_exp/java-kv/pom.xml compile
```

没有 Maven 时也可以直接使用 `javac`：

```bash
mkdir -p rust_exp/java-kv/target/classes
javac --release 17 \
  -d rust_exp/java-kv/target/classes \
  rust_exp/java-kv/src/main/java/experiment/JavaKvServer.java
```

### 3. 交替运行五轮实验

```bash
python3 rust_exp/run_formal_experiment.py \
  --rounds 5 \
  --warmup 10 \
  --duration 30 \
  --rate 60000 \
  --clients 32 \
  --keys 50000 \
  --value-size 1024
```

脚本交替运行 Rust 与 Java，Java 固定使用 256 MiB 堆和 G1 GC，并以 250 ms 间隔
采集两端服务进程的 CPU 与 RSS。结果默认写入
`rust_exp/results/formal-<时间戳>/`。

### 4. 汇总并生成图表

```bash
python3 rust_exp/generate_dashboard.py rust_exp/results/formal-<时间戳>
```

输出包括：

- `formal-summary.md`：五轮总体结果中位数；
- `overall-results.csv`：每轮总体指标；
- `dashboard-data.json`：四宫格图使用的逐秒中位数；
- `performance-dashboard.svg`：CPU、平均延迟、P95 和最大延迟四宫格；
- 每轮 Rust/Java 压测 CSV、资源采样、服务日志和 Java GC 日志。

## 已归档结果

仓库保留了一组五轮正式实验：

- [实验摘要](results/formal-20260903-123413/formal-summary.md)
- [总体指标](results/formal-20260903-123413/overall-results.csv)
- [性能四宫图](results/formal-20260903-123413/performance-dashboard.svg)

保留原始 CSV、元数据、资源采样和每轮日志，便于复核汇总结论。日志内的绝对输出
路径只是实验发生时的历史记录，不影响在其他目录复现。

## 结论边界

当前实验只支持以下有限判断：在 32 客户端、50000 个键、1 KiB value、256 MiB
Java 堆和持续覆盖写入的配置下，Rust 没有 GC 引起的集中式暂停，P99、P99.9 和
最大延迟比 Java G1 更稳定。逐秒数据与 GC 暂停只能说明时间相关性，不能把每个
延迟峰值都直接归因于 GC，也不能据此宣称 Rust 在所有负载下都更快。
