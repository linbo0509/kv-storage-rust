#!/usr/bin/env bash
# Rust / Java GC 对照实验：课堂快速演示脚本
# 只操作 rust_exp 隔离实验目录，不会启动或修改正式 KV 项目。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

ROUNDS="${ROUNDS:-1}"
WARMUP_SECONDS="${WARMUP_SECONDS:-3}"
DURATION_SECONDS="${DURATION_SECONDS:-10}"
RATE="${RATE:-60000}"
CLIENTS="${CLIENTS:-32}"
STAMP="$(date +%Y%m%d-%H%M%S)"
RESULT_DIR="$SCRIPT_DIR/results/demo-$STAMP"

echo "============================================================"
echo " Rust vs Java G1：纯内存 KV 尾延迟演示"
echo "============================================================"
echo "负载：${RATE} req/s，客户端：${CLIENTS}，预热：${WARMUP_SECONDS}s，测量：${DURATION_SECONDS}s"
echo

echo "[1/4] 构建 Rust 服务端和统一压测客户端……"
cargo build --release \
  --manifest-path "$SCRIPT_DIR/rust-kv/Cargo.toml" \
  --bin kv-memory-server \
  --bin kv-bench

echo "[2/4] 编译 Java 服务端（Java 17 语法，G1 GC，256 MiB 堆）……"
mkdir -p "$SCRIPT_DIR/java-kv/target/classes"
javac --release 17 \
  -d "$SCRIPT_DIR/java-kv/target/classes" \
  "$SCRIPT_DIR/java-kv/src/main/java/experiment/JavaKvServer.java"

echo "[3/4] 依次运行 Rust 与 Java；两者使用完全相同的请求……"
"$SCRIPT_DIR/run_formal_experiment.py" \
  --rounds "$ROUNDS" \
  --warmup "$WARMUP_SECONDS" \
  --duration "$DURATION_SECONDS" \
  --rate "$RATE" \
  --clients "$CLIENTS" \
  --keys 50000 \
  --value-size 1024 \
  --cooldown 1 \
  --output "$RESULT_DIR"

echo "[4/4] 汇总结果并绘制四宫格性能图……"
"$SCRIPT_DIR/generate_dashboard.py" "$RESULT_DIR"

# macOS Quick Look 可以把 SVG 转成便于插入 PPT 的 PNG；失败不影响实验结果。
PREVIEW_DIR="$(mktemp -d)"
if command -v qlmanage >/dev/null 2>&1 && \
   qlmanage -t -s 1600 -o "$PREVIEW_DIR" "$RESULT_DIR/performance-dashboard.svg" >/dev/null 2>&1; then
  cp "$PREVIEW_DIR/performance-dashboard.svg.png" "$RESULT_DIR/performance-dashboard.png"
fi
rm -rf "$PREVIEW_DIR"

echo
echo "===================== 演示结果 ====================="
sed -n '1,24p' "$RESULT_DIR/formal-summary.md"
echo
echo "四宫格图：$RESULT_DIR/performance-dashboard.svg"
if [[ -f "$RESULT_DIR/performance-dashboard.png" ]]; then
  echo "PPT 用 PNG：$RESULT_DIR/performance-dashboard.png"
fi
echo "完整数据：$RESULT_DIR"
echo "===================================================="
