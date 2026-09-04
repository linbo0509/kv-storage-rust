//! Rust/Java 共用的实验 A 压测客户端。
//!
//! 所有客户端先预加载键，再执行预热，最后进行固定速率的开环覆盖写入测试。
//! 同一个二进制连接两种服务器，避免压测工具本身不同造成偏差。

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Clone)]
struct Config {
    address: String,
    clients: usize,
    key_count: usize,
    value_size: usize,
    warmup_seconds: u64,
    duration_seconds: u64,
    rate: u64,
    label: String,
    output: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:7878".into(),
            clients: 32,
            key_count: 50_000,
            value_size: 1_024,
            warmup_seconds: 10,
            duration_seconds: 30,
            rate: 60_000,
            label: "unknown".into(),
            output: PathBuf::from("../results/benchmark.csv"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MeasurementStart {
    instant: Instant,
    unix_ms: u128,
}

struct WorkerResult {
    samples_by_second: Vec<Vec<u64>>,
}

#[derive(Debug)]
struct LatencyStats {
    requests: usize,
    average_us: u64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    p999_us: u64,
    max_us: u64,
}

fn main() {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("参数错误：{message}");
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = run(config) {
        eprintln!("压测失败：{error}");
        std::process::exit(1);
    }
}

fn run(config: Config) -> Result<(), AnyError> {
    println!("实验实现：{}", config.label);
    println!("服务器：{}", config.address);
    println!(
        "客户端={}，键={}，value={} B，预热={} s，测试={} s，目标速率={} 请求/秒",
        config.clients,
        config.key_count,
        config.value_size,
        config.warmup_seconds,
        config.duration_seconds,
        config.rate
    );
    println!("正在连接并预加载数据……");

    let config = Arc::new(config);
    let phase_barrier = Arc::new(Barrier::new(config.clients));
    let measurement_start = Arc::new(Mutex::new(None::<MeasurementStart>));
    let mut workers = Vec::with_capacity(config.clients);

    for client_id in 0..config.clients {
        let worker_config = Arc::clone(&config);
        let worker_barrier = Arc::clone(&phase_barrier);
        let worker_start = Arc::clone(&measurement_start);
        workers.push(
            thread::Builder::new()
                .name(format!("bench-client-{client_id}"))
                .spawn(move || {
                    run_worker(client_id, worker_config, worker_barrier, worker_start)
                })?,
        );
    }

    let mut samples_by_second = vec![Vec::new(); config.duration_seconds as usize];
    for worker in workers {
        let mut result = worker
            .join()
            .map_err(|_| io::Error::other("压测工作线程发生 panic"))??;
        for (all_clients, one_client) in samples_by_second
            .iter_mut()
            .zip(result.samples_by_second.iter_mut())
        {
            all_clients.append(one_client);
        }
    }

    let start = measurement_start
        .lock()
        .map_err(|_| io::Error::other("测量起始时间锁已中毒"))?
        .ok_or_else(|| io::Error::other("缺少测量起始时间"))?;

    let mut all_samples = Vec::new();
    for samples in &samples_by_second {
        all_samples.extend_from_slice(samples);
    }
    let overall = calculate_stats(&mut all_samples);
    let throughput = overall.requests as f64 / config.duration_seconds as f64;

    write_csv(&config, start.unix_ms, &mut samples_by_second, &overall)?;
    println!("\n========== 实验结果 ==========");
    println!("总请求数：{}", overall.requests);
    println!("吞吐量：{throughput:.2} 请求/秒");
    println!("平均延迟：{} us", overall.average_us);
    println!("P50：{} us", overall.p50_us);
    println!("P95：{} us", overall.p95_us);
    println!("P99：{} us", overall.p99_us);
    println!("P99.9：{} us", overall.p999_us);
    println!("最大延迟：{} us", overall.max_us);
    println!("CSV：{}", config.output.display());
    println!("==============================");
    Ok(())
}

fn run_worker(
    client_id: usize,
    config: Arc<Config>,
    barrier: Arc<Barrier>,
    measurement_start: Arc<Mutex<Option<MeasurementStart>>>,
) -> Result<WorkerResult, AnyError> {
    let writer = TcpStream::connect(&config.address)?;
    writer.set_nodelay(true)?;
    let reader_stream = writer.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = writer;
    let mut response = String::new();

    let value = "x".repeat(config.value_size);
    let commands: Vec<String> = (client_id..config.key_count)
        .step_by(config.clients)
        .map(|key_id| format!("SET key-{key_id:08} {value}\n"))
        .collect();
    if commands.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "客户端没有可用的键").into());
    }

    for command in &commands {
        send_set(&mut reader, &mut writer, command, &mut response)?;
    }
    barrier.wait();

    let warmup_deadline = Instant::now() + Duration::from_secs(config.warmup_seconds);
    let mut command_index = 0_usize;
    while Instant::now() < warmup_deadline {
        send_set(
            &mut reader,
            &mut writer,
            &commands[command_index],
            &mut response,
        )?;
        command_index = (command_index + 1) % commands.len();
    }

    let ready = barrier.wait();
    if ready.is_leader() {
        let unix_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        *measurement_start
            .lock()
            .map_err(|_| io::Error::other("测量起始时间锁已中毒"))? = Some(MeasurementStart {
            instant: Instant::now(),
            unix_ms,
        });
    }
    barrier.wait();

    let start = measurement_start
        .lock()
        .map_err(|_| io::Error::other("测量起始时间锁已中毒"))?
        .ok_or_else(|| io::Error::other("缺少测量起始时间"))?;
    let mut samples_by_second = vec![Vec::new(); config.duration_seconds as usize];
    let total_requests = config
        .rate
        .checked_mul(config.duration_seconds)
        .ok_or_else(|| io::Error::other("请求总数溢出"))?;
    let mut request_number = client_id as u64;

    while request_number < total_requests {
        let offset_nanos =
            (u128::from(request_number) * 1_000_000_000_u128) / u128::from(config.rate);
        let scheduled_offset = Duration::from_nanos(offset_nanos as u64);
        let scheduled_at = start.instant + scheduled_offset;
        let now = Instant::now();
        if now < scheduled_at {
            thread::sleep(scheduled_at - now);
        }
        send_set(
            &mut reader,
            &mut writer,
            &commands[command_index],
            &mut response,
        )?;
        // 从计划发送时刻起算；负载过高时排队等待也会进入延迟，避免“协调遗漏”。
        let latency_us = Instant::now()
            .saturating_duration_since(scheduled_at)
            .as_micros()
            .min(u64::MAX as u128) as u64;
        let second = scheduled_offset.as_secs() as usize;
        if let Some(samples) = samples_by_second.get_mut(second) {
            samples.push(latency_us);
        }
        command_index = (command_index + 1) % commands.len();
        request_number += config.clients as u64;
    }

    Ok(WorkerResult { samples_by_second })
}

fn send_set(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    command: &str,
    response: &mut String,
) -> Result<(), AnyError> {
    writer.write_all(command.as_bytes())?;
    writer.flush()?;
    response.clear();
    if reader.read_line(response)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "服务器提前关闭连接").into());
    }
    if response.trim_end() != "OK" {
        return Err(
            io::Error::other(format!("服务器返回异常响应：{}", response.trim_end())).into(),
        );
    }
    Ok(())
}

fn calculate_stats(samples: &mut [u64]) -> LatencyStats {
    let total: u128 = samples.iter().map(|&value| u128::from(value)).sum();
    samples.sort_unstable();
    LatencyStats {
        requests: samples.len(),
        average_us: if samples.is_empty() {
            0
        } else {
            (total / samples.len() as u128).min(u64::MAX as u128) as u64
        },
        p50_us: percentile_permille(samples, 500),
        p95_us: percentile_permille(samples, 950),
        p99_us: percentile_permille(samples, 990),
        p999_us: percentile_permille(samples, 999),
        max_us: samples.last().copied().unwrap_or(0),
    }
}

/// 使用整数千分位计算排名，避免 99.9 的浮点表示误差。
fn percentile_permille(samples: &[u64], permille: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let rank = (permille * samples.len()).div_ceil(1_000);
    samples[rank.saturating_sub(1).min(samples.len() - 1)]
}

fn write_csv(
    config: &Config,
    start_unix_ms: u128,
    samples_by_second: &mut [Vec<u64>],
    overall: &LatencyStats,
) -> Result<(), AnyError> {
    if let Some(parent) = config
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(&config.output)?;
    writeln!(
        file,
        "implementation,scope,timestamp_ms,requests,throughput_rps,average_us,p50_us,p95_us,p99_us,p999_us,max_us"
    )?;

    for (second, samples) in samples_by_second.iter_mut().enumerate() {
        let stats = calculate_stats(samples);
        writeln!(
            file,
            "{},second-{second},{},{},{:.2},{},{},{},{},{},{}",
            config.label,
            start_unix_ms + second as u128 * 1_000,
            stats.requests,
            stats.requests as f64,
            stats.average_us,
            stats.p50_us,
            stats.p95_us,
            stats.p99_us,
            stats.p999_us,
            stats.max_us,
        )?;
    }

    writeln!(
        file,
        "{},overall,{start_unix_ms},{},{:.2},{},{},{},{},{},{}",
        config.label,
        overall.requests,
        overall.requests as f64 / config.duration_seconds as f64,
        overall.average_us,
        overall.p50_us,
        overall.p95_us,
        overall.p99_us,
        overall.p999_us,
        overall.max_us,
    )?;
    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut config = Config::default();
    let mut args = args;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} 缺少参数值"))?;
        match flag.as_str() {
            "--addr" => config.address = value,
            "--clients" => config.clients = parse_number(&flag, &value)?,
            "--keys" => config.key_count = parse_number(&flag, &value)?,
            "--value-size" => config.value_size = parse_number(&flag, &value)?,
            "--warmup-seconds" => config.warmup_seconds = parse_number(&flag, &value)?,
            "--duration-seconds" => config.duration_seconds = parse_number(&flag, &value)?,
            "--rate" => config.rate = parse_number(&flag, &value)?,
            "--label" => config.label = value,
            "--output" => config.output = PathBuf::from(value),
            _ => return Err(format!("未知参数：{flag}")),
        }
    }

    if config.clients == 0 {
        return Err("--clients 必须大于 0".into());
    }
    if config.key_count < config.clients {
        return Err("--keys 不能少于客户端数量".into());
    }
    if config.value_size == 0 {
        return Err("--value-size 必须大于 0".into());
    }
    if config.duration_seconds == 0 {
        return Err("--duration-seconds 必须大于 0".into());
    }
    if config.rate == 0 {
        return Err("--rate 必须大于 0".into());
    }
    Ok(config)
}

fn parse_number<T>(flag: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| format!("{flag} 必须是合法非负整数"))
}

fn print_usage() {
    eprintln!(
        "用法：kv-bench [--addr IP:PORT] [--clients N] [--keys N] \
         [--value-size BYTES] [--warmup-seconds N] [--duration-seconds N] [--rate RPS] \
         [--label NAME] [--output PATH]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_tail_percentiles() {
        let mut samples: Vec<u64> = (1..=1_000).collect();
        let stats = calculate_stats(&mut samples);
        assert_eq!(stats.average_us, 500);
        assert_eq!(stats.p50_us, 500);
        assert_eq!(stats.p95_us, 950);
        assert_eq!(stats.p99_us, 990);
        assert_eq!(stats.p999_us, 999);
        assert_eq!(stats.max_us, 1_000);
    }

    #[test]
    fn rejects_more_clients_than_keys() {
        assert!(
            parse_args(
                vec!["--clients".into(), "4".into(), "--keys".into(), "2".into(),].into_iter(),
            )
            .is_err()
        );
    }
}
