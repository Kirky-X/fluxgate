// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! CostCalculator — 基于 (vendor_id, model_id) 维度的定价表查找。
//!
//! 设计：
//! - 内部用 `Arc<RwLock<HashMap<(String, String), PricingEntry>>>` 存储
//! - `from_config` 从 `&[PricingConfig]` 初始化表
//! - `calculate` 按 `(prompt/1000)*in_price + (completion/1000)*out_price` 计算 USD
//! - 未命中返回 0.0（避免阻塞响应）
//! - `update_entry` 运行时动态更新

use crate::config::PricingConfig;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// 单条定价 — 输入/输出 token 单价（每 1k tokens，USD）
#[derive(Debug, Clone, Copy)]
pub struct PricingEntry {
    pub input_per_1k_tokens: f64,
    pub output_per_1k_tokens: f64,
}

impl From<&PricingConfig> for PricingEntry {
    fn from(p: &PricingConfig) -> Self {
        Self {
            input_per_1k_tokens: p.input_per_1k_tokens,
            output_per_1k_tokens: p.output_per_1k_tokens,
        }
    }
}

/// 成本计算器 — 线程安全，支持运行时动态更新定价表
pub struct CostCalculator {
    table: Arc<RwLock<HashMap<(String, String), PricingEntry>>>,
}

impl CostCalculator {
    /// 从 PricingConfig 列表构建定价表
    pub fn from_config(pricing: &[PricingConfig]) -> Self {
        let mut map = HashMap::with_capacity(pricing.len());
        for p in pricing {
            map.insert(
                (p.vendor_id.clone(), p.model_id.clone()),
                PricingEntry::from(p),
            );
        }
        Self {
            table: Arc::new(RwLock::new(map)),
        }
    }

    /// 计算 USD 成本：`(prompt/1000)*in_price + (completion/1000)*out_price`
    /// 未命中 vendor/model 返回 0.0
    pub fn calculate(&self, vendor: &str, model: &str, prompt: u32, completion: u32) -> f64 {
        let guard = match self.table.read() {
            Ok(g) => g,
            Err(_) => {
                // RwLock poisoned — 不应阻塞响应，返回 0.0
                // 上层 tracing::warn! 已记录
                return 0.0;
            }
        };
        let entry = match guard.get(&(vendor.to_string(), model.to_string())) {
            Some(e) => *e,
            None => return 0.0,
        };
        let input_cost = (prompt as f64 / 1000.0) * entry.input_per_1k_tokens;
        let output_cost = (completion as f64 / 1000.0) * entry.output_per_1k_tokens;
        input_cost + output_cost
    }

    /// 动态更新单条定价 entry — 立即生效
    pub fn update_entry(&self, vendor: &str, model: &str, entry: PricingEntry) {
        if let Ok(mut guard) = self.table.write() {
            guard.insert((vendor.to_string(), model.to_string()), entry);
        }
        // poisoned lock 静默忽略 — 上层应自行监控
    }
}

impl Default for CostCalculator {
    fn default() -> Self {
        Self {
            table: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
