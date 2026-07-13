// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 全局指标收集
//!
//! 线程安全的请求计数、延迟累计与错误率统计，按厂商维度记录。
//! 数据通过原子操作写入，快照通过 RwLock 读出，适用于 hot path。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// 全局指标 — 所有 VendorManager 实例共享一份
pub struct GlobalMetrics {
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    total_duration_ns: AtomicU64,
    per_vendor: RwLock<HashMap<String, VendorMetrics>>,
}

impl GlobalMetrics {
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            total_duration_ns: AtomicU64::new(0),
            per_vendor: RwLock::new(HashMap::new()),
        }
    }

    /// 记录一次请求结果。`duration` 通常由 `Instant::now()` 差值计算。
    pub fn record_request(&self, vendor: &str, success: bool, duration: std::time::Duration) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
        self.total_duration_ns
            .fetch_add(duration.as_nanos() as u64, Ordering::Relaxed);

        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.requests += 1;
            if success {
                entry.successes += 1;
            } else {
                entry.failures += 1;
            }
            entry.total_duration_ns = entry.total_duration_ns.saturating_add(duration.as_nanos() as u64);
            entry.last_request_at = Some(now_unix_ts());
        }
    }

    /// 记录一次错误（不增加请求计数，仅记录最近错误信息）
    pub fn record_error(&self, vendor: &str, error: &str) {
        if let Ok(mut guard) = self.per_vendor.write() {
            let entry = guard
                .entry(vendor.to_string())
                .or_insert_with(|| VendorMetrics::with_vendor(vendor));
            entry.last_error = Some(error.to_string());
        }
    }

    /// 错误率 (0.0 - 1.0)，无请求时返回 0.0
    pub fn error_rate(&self) -> f64 {
        let total = self.total_requests.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        let failed = self.failed_requests.load(Ordering::Relaxed);
        failed as f64 / total as f64
    }

    /// 生成可序列化快照
    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.total_requests.load(Ordering::Relaxed);
        let success = self.successful_requests.load(Ordering::Relaxed);
        let failed = self.failed_requests.load(Ordering::Relaxed);
        let total_ns = self.total_duration_ns.load(Ordering::Relaxed);

        let avg_duration_ms = if total == 0 {
            0.0
        } else {
            (total_ns as f64 / total as f64) / 1_000_000.0
        };

        let vendors = self
            .per_vendor
            .read()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default();

        MetricsSnapshot {
            total_requests: total,
            successful_requests: success,
            failed_requests: failed,
            error_rate: self.error_rate(),
            avg_duration_ms,
            vendors,
        }
    }

    /// 重置所有指标（主要用于测试）
    pub fn reset(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        self.successful_requests.store(0, Ordering::Relaxed);
        self.failed_requests.store(0, Ordering::Relaxed);
        self.total_duration_ns.store(0, Ordering::Relaxed);
        if let Ok(mut guard) = self.per_vendor.write() {
            guard.clear();
        }
    }
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 单个厂商的指标
#[derive(Debug, Clone, Serialize)]
pub struct VendorMetrics {
    pub vendor: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_duration_ns: u64,
    pub last_request_at: Option<i64>,
    pub last_error: Option<String>,
}

impl VendorMetrics {
    fn new() -> Self {
        Self {
            vendor: String::new(),
            requests: 0,
            successes: 0,
            failures: 0,
            total_duration_ns: 0,
            last_request_at: None,
            last_error: None,
        }
    }

    fn with_vendor(vendor: &str) -> Self {
        let mut m = Self::new();
        m.vendor = vendor.to_string();
        m
    }

    pub fn avg_duration_ms(&self) -> f64 {
        if self.requests == 0 {
            return 0.0;
        }
        (self.total_duration_ns as f64 / self.requests as f64) / 1_000_000.0
    }
}

/// 指标快照 — 可序列化返回给客户端
#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub error_rate: f64,
    pub avg_duration_ms: f64,
    pub vendors: Vec<VendorMetrics>,
}

fn now_unix_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_empty_metrics_snapshot() {
        let m = GlobalMetrics::new();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert_eq!(snap.successful_requests, 0);
        assert_eq!(snap.failed_requests, 0);
        assert_eq!(snap.error_rate, 0.0);
        assert_eq!(snap.avg_duration_ms, 0.0);
        assert!(snap.vendors.is_empty());
    }

    #[test]
    fn test_record_success_updates_counters() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(150));
        m.record_request("openai", true, Duration::from_millis(250));

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 2);
        assert_eq!(snap.successful_requests, 2);
        assert_eq!(snap.failed_requests, 0);
        assert_eq!(snap.error_rate, 0.0);
        // avg = (150 + 250) / 2 = 200ms
        assert!((snap.avg_duration_ms - 200.0).abs() < 1e-3);
    }

    #[test]
    fn test_record_failure_updates_error_rate() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("openai", false, Duration::from_millis(50));
        m.record_request("openai", false, Duration::from_millis(50));

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 3);
        assert_eq!(snap.successful_requests, 1);
        assert_eq!(snap.failed_requests, 2);
        assert!((snap.error_rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_per_vendor_metrics_tracked_separately() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("openai", true, Duration::from_millis(200));
        m.record_request("anthropic", false, Duration::from_millis(300));

        let snap = m.snapshot();
        assert_eq!(snap.vendors.len(), 2);

        let openai = snap
            .vendors
            .iter()
            .find(|v| v.vendor == "openai")
            .expect("openai metrics present");
        assert_eq!(openai.requests, 2);
        assert_eq!(openai.successes, 2);
        assert!((openai.avg_duration_ms() - 150.0).abs() < 1e-3);

        let anthropic = snap
            .vendors
            .iter()
            .find(|v| v.vendor == "anthropic")
            .expect("anthropic metrics present");
        assert_eq!(anthropic.requests, 1);
        assert_eq!(anthropic.failures, 1);
    }

    #[test]
    fn test_record_error_does_not_increment_request_count() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_error("openai", "timeout");

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.vendors.len(), 1);
        assert_eq!(snap.vendors[0].last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn test_reset_clears_all_state() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("anthropic", false, Duration::from_millis(50));
        assert_eq!(m.snapshot().total_requests, 2);

        m.reset();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert!(snap.vendors.is_empty());
    }

    #[test]
    fn test_concurrent_writes_are_safe() {
        let m = std::sync::Arc::new(GlobalMetrics::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let m_clone = m.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    m_clone.record_request("openai", i % 3 != 0, Duration::from_millis(10));
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 1000);
        assert_eq!(snap.successful_requests + snap.failed_requests, 1000);
    }

    #[test]
    fn test_error_rate_zero_when_no_requests() {
        let m = GlobalMetrics::new();
        assert_eq!(m.error_rate(), 0.0);
    }

    #[test]
    fn test_vendor_metrics_avg_duration_zero_when_no_requests() {
        let v = VendorMetrics::new();
        assert_eq!(v.avg_duration_ms(), 0.0);
    }
}
