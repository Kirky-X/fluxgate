// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! FallbackChain — 按优先级链式回退策略。
//!
//! 设计要点：
//! - `priority: u8` 数值越小优先级越高（1 = primary，2 = secondary，…）
//! - 触发策略（`FallbackTrigger`）决定哪些错误会激活 fallback
//! - `next_after_failure` 返回 priority 数值大于 failed 的最小 priority target
//!   —— 即"降级到下一档"，所有 fallback 用尽时返回 None

use crate::error::GateError;
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
    pub fn should_trigger(&self, error: &GateError) -> bool {
        match self.trigger {
            FallbackTrigger::All => true,
            FallbackTrigger::Timeout => matches!(error, GateError::Timeout { .. }),
            FallbackTrigger::RateLimited => match error {
                GateError::RateLimited { .. } => true,
                GateError::VendorUnavailable { vendor } => {
                    !is_circuit_open(vendor) && extract_status_code(vendor) == 429
                }
                _ => false,
            },
            FallbackTrigger::CircuitOpen => match error {
                GateError::VendorUnavailable { vendor } => is_circuit_open(vendor),
                _ => false,
            },
            FallbackTrigger::Http5xx => match error {
                GateError::VendorUnavailable { vendor } => {
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
    pub fn next_after_failure(&self, failed: &str, error: &GateError) -> Option<&FallbackTarget> {
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

/// 回退尝试链（WP4 可观测性）：按尝试顺序记录 vendor id（含最终命中者）。
/// 由 API 层换路循环填充（初始候选先记， 每次重派前记新候选），成功响应经
/// `x-nexusflow-attempts`（总数）/`x-nexusflow-fallbacks`（逗号分隔链）出站。
/// **分配门控（审查 L4）**：构造期注入 `[resilience].expose_fallback_headers`
/// 开关——关闭时 `record` 短路，热路径零分配（修复「注释宣称零额外分配但
/// record 仍 to_string」的不实）。
#[derive(Debug, Clone, Default)]
pub struct AttemptTrail {
    trail: Vec<String>,
    exposed: bool,
}

impl AttemptTrail {
    /// `exposed` = 响应头开关（构造期读定，本请求口径一致；热更对下一请求
    /// 生效）。`Default`（exposed=false）即零分配哨兵形态。
    pub fn new(exposed: bool) -> Self {
        Self {
            trail: Vec::new(),
            exposed,
        }
    }

    /// 记录一次尝试（初始候选与每次重派目标各记一次，按时间序）。
    /// 开关关闭时 no-op（不分配）。
    pub fn record(&mut self, vendor_id: &str) {
        if !self.exposed {
            return;
        }
        self.trail.push(vendor_id.to_string());
    }

    /// 总尝试次数（单跳直连 = 1）。
    pub fn attempts(&self) -> usize {
        self.trail.len()
    }

    /// 尝试链（逗号分隔，含最终命中者；空链返回空串）。
    pub fn fallbacks_value(&self) -> String {
        self.trail.join(",")
    }

    /// 响应头开关（构造期读定）。
    pub fn exposed(&self) -> bool {
        self.exposed
    }
}

/// 检查 vendor 字符串是否包含 "circuit open" 标记
fn is_circuit_open(vendor: &str) -> bool {
    vendor.contains("circuit open")
}

#[cfg(test)]
mod trail_tests {
    use super::*;

    /// 开关关闭：record 零分配（链空、attempts=0）；开启：照常记录。
    #[test]
    fn record_gates_allocation_on_expose_switch() {
        let mut disabled = AttemptTrail::new(false);
        disabled.record("v-a");
        disabled.record("v-b");
        assert_eq!(disabled.attempts(), 0, "开关关闭不得记录");
        assert_eq!(disabled.fallbacks_value(), "");

        let mut enabled = AttemptTrail::new(true);
        enabled.record("v-a");
        enabled.record("v-b");
        assert_eq!(enabled.attempts(), 2);
        assert_eq!(enabled.fallbacks_value(), "v-a,v-b");
        assert!(enabled.exposed());
        assert!(!disabled.exposed());
    }
}
