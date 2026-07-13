// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! FallbackChain — 按优先级链式回退策略。
//!
//! 设计要点：
//! - `priority: u8` 数值越小优先级越高（1 = primary，2 = secondary，…）
//! - 触发策略（`FallbackTrigger`）决定哪些错误会激活 fallback
//! - `next_after_failure` 返回 priority 数值大于 failed 的最小 priority target
//!   —— 即"降级到下一档"，所有 fallback 用尽时返回 None

use crate::error::NexusFlowError;
use crate::utils::retry::extract_status_code;

/// 单个 fallback 目标 — vendor + model + 优先级
#[derive(Debug, Clone)]
pub struct FallbackTarget {
    pub vendor_id: String,
    pub model_id: String,
    pub priority: u8,
}

/// 触发 fallback 的错误类型策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackTrigger {
    /// 仅 HTTP 5xx 状态码触发
    Http5xx,
    /// 仅超时错误触发
    Timeout,
    /// 仅限流错误触发（RateLimited variant 或 429 状态码）
    RateLimited,
    /// 仅 CircuitBreaker Open 错误触发
    CircuitOpen,
    /// 任意错误都触发
    All,
}

/// FallbackChain — 一组按优先级排序的回退目标 + 触发策略
pub struct FallbackChain {
    pub targets: Vec<FallbackTarget>,
    pub trigger: FallbackTrigger,
}

impl FallbackChain {
    pub fn new(targets: Vec<FallbackTarget>, trigger: FallbackTrigger) -> Self {
        Self { targets, trigger }
    }

    /// 判断当前错误是否应触发 fallback
    pub fn should_trigger(&self, error: &NexusFlowError) -> bool {
        match self.trigger {
            FallbackTrigger::All => true,
            FallbackTrigger::Timeout => matches!(error, NexusFlowError::Timeout { .. }),
            FallbackTrigger::RateLimited => match error {
                NexusFlowError::RateLimited { .. } => true,
                NexusFlowError::VendorUnavailable { vendor } => {
                    !is_circuit_open(vendor) && extract_status_code(vendor) == 429
                }
                _ => false,
            },
            FallbackTrigger::CircuitOpen => match error {
                NexusFlowError::VendorUnavailable { vendor } => is_circuit_open(vendor),
                _ => false,
            },
            FallbackTrigger::Http5xx => match error {
                NexusFlowError::VendorUnavailable { vendor } => {
                    // 排除 circuit open 错误（由 CircuitOpen 触发器处理）
                    !is_circuit_open(vendor)
                        && matches!(extract_status_code(vendor), 500 | 502 | 503)
                }
                _ => false,
            },
        }
    }

    /// 在 failed vendor 失败后返回下一个 fallback target。
    ///
    /// 逻辑：
    /// 1. 若错误不匹配触发策略 → None
    /// 2. 找到 failed vendor 的 priority
    /// 3. 在所有 priority 数值大于 failed 的 target 中，选 priority 最小的
    /// 4. 若无更大 priority 的 target（链已用尽）→ None
    pub fn next_after_failure(
        &self,
        failed: &str,
        error: &NexusFlowError,
    ) -> Option<&FallbackTarget> {
        if !self.should_trigger(error) {
            return None;
        }
        let failed_priority = self
            .targets
            .iter()
            .find(|t| t.vendor_id == failed)
            .map(|t| t.priority)?;
        self.targets
            .iter()
            .filter(|t| t.priority > failed_priority)
            .min_by_key(|t| t.priority)
    }
}

/// 检查 vendor 字符串是否包含 "circuit open" 标记
fn is_circuit_open(vendor: &str) -> bool {
    vendor.contains("circuit open")
}
