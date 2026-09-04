//! 线程安全的服务器运行指标。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// 服务器运行指标。
///
/// 指标只用于观察，不参与业务正确性，因此使用 Relaxed 内存序即可。
#[derive(Debug)]
pub struct ServerMetrics {
    started_at: Instant,
    active_clients: AtomicUsize,
    total_connections: AtomicU64,
    commands_total: AtomicU64,
    commands_failed: AtomicU64,
    checkpoint_attempts: AtomicU64,
    checkpoint_failures: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent: AtomicU64,
}

impl ServerMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            active_clients: AtomicUsize::new(0),
            total_connections: AtomicU64::new(0),
            commands_total: AtomicU64::new(0),
            commands_failed: AtomicU64::new(0),
            checkpoint_attempts: AtomicU64::new(0),
            checkpoint_failures: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        }
    }

    /// 登记一个已进入会话处理流程的连接。
    ///
    /// 返回的守卫离开作用域时自动减少活跃连接数，因此提前返回和异常断开
    /// 都不会让 active_clients 永久偏大。
    #[must_use]
    pub fn connection_opened(self: &Arc<Self>) -> ConnectionGuard {
        self.active_clients.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        ConnectionGuard {
            metrics: Arc::clone(self),
        }
    }

    pub fn record_command(&self, bytes_received: u64) {
        self.commands_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(bytes_received, Ordering::Relaxed);
    }

    pub fn record_command_failure(&self) {
        self.commands_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_checkpoint(&self, succeeded: bool) {
        self.checkpoint_attempts.fetch_add(1, Ordering::Relaxed);
        if !succeeded {
            self.checkpoint_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_response(&self, bytes_sent: u64) {
        self.bytes_sent.fetch_add(bytes_sent, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            active_clients: self.active_clients.load(Ordering::Relaxed),
            total_connections: self.total_connections.load(Ordering::Relaxed),
            commands_total: self.commands_total.load(Ordering::Relaxed),
            commands_failed: self.commands_failed.load(Ordering::Relaxed),
            checkpoint_attempts: self.checkpoint_attempts.load(Ordering::Relaxed),
            checkpoint_failures: self.checkpoint_failures.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            uptime_seconds: self.started_at.elapsed().as_secs(),
        }
    }
}

impl Default for ServerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 用 RAII 保证连接计数在所有退出路径上都能恢复。
pub struct ConnectionGuard {
    metrics: Arc<ServerMetrics>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.metrics.active_clients.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub active_clients: usize,
    pub total_connections: u64,
    pub commands_total: u64,
    pub commands_failed: u64,
    pub checkpoint_attempts: u64,
    pub checkpoint_failures: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub uptime_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_guard_tracks_active_and_total_connections() {
        let metrics = Arc::new(ServerMetrics::new());
        {
            let _first = metrics.connection_opened();
            let _second = metrics.connection_opened();
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.active_clients, 2);
            assert_eq!(snapshot.total_connections, 2);
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.active_clients, 0);
        assert_eq!(snapshot.total_connections, 2);
    }

    #[test]
    fn command_and_byte_counters_accumulate() {
        let metrics = ServerMetrics::new();
        metrics.record_command(12);
        metrics.record_command(8);
        metrics.record_command_failure();
        metrics.record_checkpoint(true);
        metrics.record_checkpoint(false);
        metrics.record_response(30);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.commands_total, 2);
        assert_eq!(snapshot.commands_failed, 1);
        assert_eq!(snapshot.checkpoint_attempts, 2);
        assert_eq!(snapshot.checkpoint_failures, 1);
        assert_eq!(snapshot.bytes_received, 20);
        assert_eq!(snapshot.bytes_sent, 30);
    }
}
