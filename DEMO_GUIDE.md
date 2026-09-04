# Rust KV 项目答辩演示手册

本手册采用手动粘贴命令的方式。建议提前打开三个终端窗口，并将窗口标题或位置
分别标记为：**服务器**、**客户端 A**、**客户端 B**。

> 以下命令均从项目根目录开始执行。演示数据单独保存在 `demo-manual-data`，
> 不会影响平时使用的 `server-data`。命令面向 macOS / Linux / Git Bash；
> 在 Windows 上请使用 Git Bash 或 WSL 运行。

## 演示前准备

三个窗口都先进入项目根目录：

```bash
cd kv-storage-rust
```

只需在服务器窗口构建一次：

```bash
cargo build --release --bin kv-server --bin kv-client
```

为了保证每次演示从空数据库开始，可在答辩前手动删除旧的 `demo-manual-data`。
现场演示过程中不要删除它，因为重启恢复环节需要使用其中的数据。

---

## 演示一：启动服务器

### 服务器窗口

```bash
./target/release/kv-server \
  --addr 127.0.0.1:17878 \
  --data-dir ./demo-manual-data \
  --checkpoint-after 3
```

讲解要点：

- 采用 TCP 客户端/服务器架构；
- 服务器可以同时接受多个客户端；
- 每 3 次成功写操作自动生成一次快照，方便现场观察；
- 客户端执行命令后，服务器立即打印客户端地址、操作结果和耗时。

---

## 演示二：基础增删改查和异常容错

### 客户端 A 窗口

先启动客户端：

```bash
./target/release/kv-client --addr 127.0.0.1:17878
```

然后逐条粘贴：

```text
SET course rust
GET course
UPDATE course rust-2026
GET course
SET language safe
KEYS
DELETE language
GET language
BROKEN COMMAND
STATUS
```

预期现象：

- `SET`、`GET`、`UPDATE`、`DELETE` 和 `KEYS` 均正常工作；
- 删除后再次查询 `language`，返回 `key not found`，服务器不会崩溃；
- `BROKEN COMMAND` 返回明确的错误信息，连接仍然可以继续使用；
- `STATUS` 分组显示键数量、WAL、连接数、命令统计和网络流量；
- 同时观察服务器窗口，它会逐条输出实时反馈。

暂时不要退出客户端 A，留给并发演示使用。

---

## 演示三：两个客户端并发访问

### 客户端 B 窗口

```bash
./target/release/kv-client --addr 127.0.0.1:17878
```

在客户端 B 粘贴：

```text
SET client-b banana
GET client-b
STATUS
```

### 立即切换到客户端 A 窗口

```text
SET client-a apple
GET client-a
KEYS
STATUS
```

### 再回到客户端 B 窗口

```text
GET client-a
KEYS
```

讲解要点：

- 两个客户端保持两条独立 TCP 连接；
- B 能读取 A 写入的数据，说明它们访问同一个共享存储引擎；
- `STATUS` 的“当前客户端”应显示 2；
- 服务器窗口中的实时日志带有不同的客户端端口，可以证明请求来自不同连接；
- Rust 使用 `Arc<Mutex<Engine>>` 保证共享数据、WAL 和快照不会被并发写坏。

结束两个客户端连接：

```text
EXIT
```

需要在客户端 A、客户端 B 中分别执行一次。

---

## 演示四：JSON 快照和 WAL

重新打开客户端 A：

```bash
./target/release/kv-client --addr 127.0.0.1:17878
```

执行：

```text
SAVE
SET after-save replay-from-wal
STATUS
EXIT
```

### 新建一个临时终端或暂停服务器后查看文件

```bash
ls -lh demo-manual-data
```

```bash
sed -n '1,60p' demo-manual-data/snapshot.json
```

```bash
sed -n '1,30p' demo-manual-data/wal.log
```

讲解要点：

- `snapshot.json` 是完整数据库快照，包含版本、序列号和校验和；
- `SAVE` 成功后旧 WAL 被压缩；
- `after-save` 写入发生在快照之后，因此保留在 WAL 中；
- 恢复时先加载快照，再重放快照之后的 WAL。

---

## 演示五：服务器重启后恢复数据

在服务器窗口按 `Control+C` 停止服务器，然后重新执行：

```bash
./target/release/kv-server \
  --addr 127.0.0.1:17878 \
  --data-dir ./demo-manual-data \
  --checkpoint-after 3
```

重新打开客户端：

```bash
./target/release/kv-client --addr 127.0.0.1:17878
```

逐条查询：

```text
GET course
GET client-a
GET client-b
GET after-save
KEYS
STATUS
EXIT
```

预期结果：服务器启动时提示恢复的键数量，重启前的数据均能正常查询。

---

## 演示六：损坏文件保护

先在服务器窗口按 `Control+C` 停止服务。复制一份演示数据，避免破坏正常数据：

```bash
cp -r demo-manual-data demo-corrupt-data
```

故意破坏副本中的快照：

```bash
printf '%s\n' '{ deliberately broken json' > demo-corrupt-data/snapshot.json
```

尝试从损坏副本启动：

```bash
./target/release/kv-server \
  --addr 127.0.0.1:17879 \
  --data-dir ./demo-corrupt-data \
  --checkpoint-after 3
```

预期结果：服务器明确报告 JSON/数据损坏并拒绝启动，不会把数据库静默清空。
原始的 `demo-manual-data` 保持完好。

---
