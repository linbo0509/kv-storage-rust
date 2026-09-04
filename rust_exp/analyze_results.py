#!/usr/bin/env python3
"""汇总 Rust/Java 压测 CSV，并生成带 Java GC 标记的 SVG 延迟曲线。"""

from __future__ import annotations

import argparse
import csv
import math
import re
from dataclasses import dataclass
from datetime import datetime
from html import escape
from pathlib import Path

GC_LINE = re.compile(
    r"^\[(?P<timestamp>[^\]]+)\].*\[gc\s*\].*Pause .* "
    r"(?P<duration>[0-9]+(?:\.[0-9]+)?)ms$"
)


@dataclass
class Benchmark:
    label: str
    seconds: list[dict[str, int]]
    overall: dict[str, int]


@dataclass
class GcPause:
    timestamp_ms: int
    duration_ms: float


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="生成 Rust/Java 实验 A 对比结果")
    parser.add_argument("--rust", type=Path, required=True, help="Rust 压测 CSV")
    parser.add_argument("--java", type=Path, required=True, help="Java 压测 CSV")
    parser.add_argument("--gc-log", type=Path, required=True, help="Java -Xlog:gc 日志")
    parser.add_argument(
        "--output-dir", type=Path, default=Path("results/comparison"), help="输出目录"
    )
    return parser.parse_args()


def read_benchmark(path: Path) -> Benchmark:
    seconds: list[dict[str, int]] = []
    overall: dict[str, int] | None = None
    label = "unknown"
    with path.open(newline="", encoding="utf-8") as file:
        for row in csv.DictReader(file):
            label = row["implementation"]
            parsed = {
                "timestamp_ms": int(row["timestamp_ms"]),
                "requests": int(row["requests"]),
                "p50_us": int(row["p50_us"]),
                "p95_us": int(row["p95_us"]),
                "p99_us": int(row["p99_us"]),
                "p999_us": int(row["p999_us"]),
                "max_us": int(row["max_us"]),
            }
            if row["scope"] == "overall":
                parsed["throughput_rps"] = round(float(row["throughput_rps"]))
                overall = parsed
            elif row["scope"].startswith("second-"):
                parsed["second"] = int(row["scope"].removeprefix("second-"))
                seconds.append(parsed)
    if overall is None or not seconds:
        raise ValueError(f"{path} 缺少 overall 或逐秒数据")
    seconds.sort(key=lambda row: row["second"])
    return Benchmark(label, seconds, overall)


def read_gc_pauses(path: Path) -> list[GcPause]:
    pauses: list[GcPause] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        match = GC_LINE.match(line)
        if not match:
            continue
        timestamp = datetime.fromisoformat(match.group("timestamp"))
        pauses.append(
            GcPause(
                timestamp_ms=round(timestamp.timestamp() * 1_000),
                duration_ms=float(match.group("duration")),
            )
        )
    return pauses


def write_summary(
    output: Path, rust: Benchmark, java: Benchmark, pauses: list[GcPause]
) -> None:
    rust_overall = rust.overall
    java_overall = java.overall
    java_start = java.seconds[0]["timestamp_ms"]
    java_end = java.seconds[-1]["timestamp_ms"] + 1_000
    measured_pauses = [
        pause for pause in pauses if java_start <= pause.timestamp_ms < java_end
    ]
    total_pause = sum(pause.duration_ms for pause in measured_pauses)
    max_pause = max((pause.duration_ms for pause in measured_pauses), default=0.0)

    def ratio(java_value: int, rust_value: int) -> str:
        return "无法计算" if rust_value == 0 else f"{java_value / rust_value:.2f}×"

    text = f"""# 实验 A 对比摘要

> 本文件由 `analyze_results.py` 自动生成。正式结论应至少基于 5 轮交替实验，
> 不能把单轮结果直接解释为语言的普遍性能结论。

| 指标 | {rust.label} | {java.label} | Java / Rust |
|---|---:|---:|---:|
| 吞吐量（请求/秒） | {rust_overall['throughput_rps']} | {java_overall['throughput_rps']} | {ratio(java_overall['throughput_rps'], rust_overall['throughput_rps'])} |
| P50（us） | {rust_overall['p50_us']} | {java_overall['p50_us']} | {ratio(java_overall['p50_us'], rust_overall['p50_us'])} |
| P95（us） | {rust_overall['p95_us']} | {java_overall['p95_us']} | {ratio(java_overall['p95_us'], rust_overall['p95_us'])} |
| P99（us） | {rust_overall['p99_us']} | {java_overall['p99_us']} | {ratio(java_overall['p99_us'], rust_overall['p99_us'])} |
| P99.9（us） | {rust_overall['p999_us']} | {java_overall['p999_us']} | {ratio(java_overall['p999_us'], rust_overall['p999_us'])} |
| 最大延迟（us） | {rust_overall['max_us']} | {java_overall['max_us']} | {ratio(java_overall['max_us'], rust_overall['max_us'])} |

## Java GC（正式测量窗口内）

- GC 暂停次数：{len(measured_pauses)}
- GC 暂停总时间：{total_pause:.3f} ms
- 单次最大 GC 暂停：{max_pause:.3f} ms

## 阅读方法

1. 查看 `latency-timeline.svg` 中红色竖线与 Java 延迟峰值是否在同一秒出现。
2. 优先比较 P99.9 和最大延迟，平均值或 P50 不能充分反映 GC 停顿。
3. 即使 Java 更慢，也只有“延迟峰值与 GC 时刻重合”才能支持 GC 因果解释。
4. Rust 没有 GC 停顿，但仍可能因线程调度、锁竞争或操作系统产生延迟峰值。
"""
    output.write_text(text, encoding="utf-8")


def write_svg(
    output: Path, rust: Benchmark, java: Benchmark, pauses: list[GcPause]
) -> None:
    width, height = 1_100, 640
    left, right, top, bottom = 90, 35, 55, 80
    plot_width = width - left - right
    plot_height = height - top - bottom
    max_seconds = max(len(rust.seconds), len(java.seconds))
    values = [
        row[field]
        for benchmark in (rust, java)
        for row in benchmark.seconds
        for field in ("p999_us", "max_us")
    ]
    max_log = max(1.0, math.log10(max(values, default=1)))

    def x(second: float) -> float:
        denominator = max(1, max_seconds - 1)
        return left + second / denominator * plot_width

    def y(value: int | float) -> float:
        return top + (1.0 - math.log10(max(1.0, float(value))) / max_log) * plot_height

    def polyline(benchmark: Benchmark, field: str, color: str, dash: str = "") -> str:
        points = " ".join(
            f"{x(row['second']):.1f},{y(row[field]):.1f}" for row in benchmark.seconds
        )
        dash_attr = f' stroke-dasharray="{dash}"' if dash else ""
        return (
            f'<polyline points="{points}" fill="none" stroke="{color}" '
            f'stroke-width="2"{dash_attr}/>'
        )

    java_start = java.seconds[0]["timestamp_ms"]
    java_duration_ms = len(java.seconds) * 1_000
    measured_pauses = [
        pause
        for pause in pauses
        if 0 <= pause.timestamp_ms - java_start < java_duration_ms
    ]

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        '<rect width="100%" height="100%" fill="white"/>',
        '<text x="550" y="30" text-anchor="middle" font-size="22" font-family="sans-serif">实验 A 尾延迟时间序列（对数轴）</text>',
        f'<line x1="{left}" y1="{top + plot_height}" x2="{left + plot_width}" y2="{top + plot_height}" stroke="#333"/>',
        f'<line x1="{left}" y1="{top}" x2="{left}" y2="{top + plot_height}" stroke="#333"/>',
    ]

    for index in range(6):
        log_value = max_log * index / 5
        value = 10**log_value
        y_pos = top + plot_height - index / 5 * plot_height
        parts.append(
            f'<line x1="{left}" y1="{y_pos:.1f}" x2="{left + plot_width}" y2="{y_pos:.1f}" stroke="#ddd"/>'
        )
        parts.append(
            f'<text x="{left - 10}" y="{y_pos + 4:.1f}" text-anchor="end" font-size="12" font-family="sans-serif">{value:.0f}</text>'
        )

    for pause in measured_pauses:
        second = (pause.timestamp_ms - java_start) / 1_000
        x_pos = x(second)
        parts.append(
            f'<line x1="{x_pos:.1f}" y1="{top}" x2="{x_pos:.1f}" y2="{top + plot_height}" stroke="#d62728" stroke-width="1" opacity="0.45"/>'
        )

    parts.extend(
        [
            polyline(rust, "p999_us", "#1f77b4"),
            polyline(java, "p999_us", "#ff7f0e"),
            polyline(rust, "max_us", "#1f77b4", "6 4"),
            polyline(java, "max_us", "#ff7f0e", "6 4"),
            f'<text x="{left + plot_width / 2}" y="{height - 25}" text-anchor="middle" font-size="14" font-family="sans-serif">测量时间（秒）</text>',
            f'<text x="20" y="{top + plot_height / 2}" transform="rotate(-90 20 {top + plot_height / 2})" text-anchor="middle" font-size="14" font-family="sans-serif">延迟（us，对数轴）</text>',
        ]
    )

    legend = [
        (rust.label + " P99.9", "#1f77b4", ""),
        (java.label + " P99.9", "#ff7f0e", ""),
        (rust.label + " Max", "#1f77b4", "6 4"),
        (java.label + " Max", "#ff7f0e", "6 4"),
        ("Java GC Pause", "#d62728", ""),
    ]
    legend_x, legend_y = left + 15, top + 20
    for index, (label, color, dash) in enumerate(legend):
        y_pos = legend_y + index * 23
        dash_attr = f' stroke-dasharray="{dash}"' if dash else ""
        parts.append(
            f'<line x1="{legend_x}" y1="{y_pos}" x2="{legend_x + 30}" y2="{y_pos}" stroke="{color}" stroke-width="2"{dash_attr}/>'
        )
        parts.append(
            f'<text x="{legend_x + 38}" y="{y_pos + 4}" font-size="12" font-family="sans-serif">{escape(label)}</text>'
        )

    parts.append("</svg>")
    output.write_text("\n".join(parts), encoding="utf-8")


def main() -> None:
    args = parse_args()
    rust = read_benchmark(args.rust)
    java = read_benchmark(args.java)
    pauses = read_gc_pauses(args.gc_log)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    summary = args.output_dir / "comparison-summary.md"
    chart = args.output_dir / "latency-timeline.svg"
    write_summary(summary, rust, java, pauses)
    write_svg(chart, rust, java, pauses)
    print(f"对比摘要：{summary}")
    print(f"延迟图表：{chart}")


if __name__ == "__main__":
    main()
