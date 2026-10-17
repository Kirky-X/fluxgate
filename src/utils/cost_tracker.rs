// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! CostTracker — 累积式成本统计器。
//!
//! 金额以 micro USD（USD × 1e6）整数原子累加，避免浮点误差；按 vendor /
//! model 分桶，支持高并发 record。

use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

const MICRO_PER_USD: f64 = 1_000_000.0;

/// 成本快照 — 某时刻的累积统计
#[derive(Debug, Clone)]
pub struct CostSnapshot {
    pub total_usd: f64,
    pub by_vendor: HashMap<String, f64>,
    pub by_model: HashMap<String, f64>,
}

/// 成本统计器 — 线程安全，支持高并发 record
pub struct CostTracker {
    total_micro_usd: AtomicU64,
    by_vendor: DashMap<String, AtomicU64>,
    by_model: DashMap<String, AtomicU64>,
}

impl CostTracker {
    pub fn new() -> Self {
        Self {
            total_micro_usd: AtomicU64::new(0),
            by_vendor: DashMap::new(),
            by_model: DashMap::new(),
        }
    }

    /// 记录一次成本（USD，内部转 micro_usd 整数原子累加）。
    ///
    /// 用 `.round()` 而非 `as u64` 截断，避免浮点表示误差导致的截断损失。
    pub fn record(&self, vendor: &str, model: &str, cost_usd: f64) {
        if cost_usd <= 0.0 {
            return;
        }
        let micro_usd = (cost_usd * MICRO_PER_USD).round() as u64;
        if micro_usd == 0 {
            // 极小 cost（如 1e-10 USD）四舍五入为 0 — 静默丢弃
            return;
        }
        self.total_micro_usd.fetch_add(micro_usd, Ordering::Relaxed);

        // by_vendor
        self.by_vendor
            .entry(vendor.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(micro_usd, Ordering::Relaxed);

        // by_model
        self.by_model
            .entry(model.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(micro_usd, Ordering::Relaxed);
    }

    /// 当前总成本（USD）
    pub fn total_usd(&self) -> f64 {
        self.total_micro_usd.load(Ordering::Relaxed) as f64 / MICRO_PER_USD
    }

    /// 指定 vendor 的累计成本（USD）。未命中返回 0.0。
    pub fn by_vendor_usd(&self, vendor: &str) -> f64 {
        self.by_vendor
            .get(vendor)
            .map(|c| c.load(Ordering::Relaxed) as f64 / MICRO_PER_USD)
            .unwrap_or(0.0)
    }

    /// 指定 model 的累计成本（USD）。未命中返回 0.0。
    pub fn by_model_usd(&self, model: &str) -> f64 {
        self.by_model
            .get(model)
            .map(|c| c.load(Ordering::Relaxed) as f64 / MICRO_PER_USD)
            .unwrap_or(0.0)
    }

    /// 生成快照 — 含 total + by_vendor + by_model 三组数据
    pub fn snapshot(&self) -> CostSnapshot {
        let mut by_vendor = HashMap::with_capacity(self.by_vendor.len());
        for entry in self.by_vendor.iter() {
            by_vendor.insert(
                entry.key().clone(),
                entry.value().load(Ordering::Relaxed) as f64 / MICRO_PER_USD,
            );
        }
        let mut by_model = HashMap::with_capacity(self.by_model.len());
        for entry in self.by_model.iter() {
            by_model.insert(
                entry.key().clone(),
                entry.value().load(Ordering::Relaxed) as f64 / MICRO_PER_USD,
            );
        }
        CostSnapshot {
            total_usd: self.total_usd(),
            by_vendor,
            by_model,
        }
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}
