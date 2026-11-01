// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 配额预算读侧契约（预算读模型 + 读侧端口）。
//!
//! 与 [`crate::storage`] 同构的边界缝：**契约定义在此、实现留在消费方模块**
//! （`middleware::quota::QuotaState` 实现 [`VendorBudgetSource`]）。本模块不
//! 带任何 feature 门控，只依赖始终在位的 [`crate::config`]。
//!
//! 为什么必须独立成根级模块而非放在某个 feature 内：`router` 不携带
//! `web-server`，而 `middleware`（quota 实现所在模块）随 `web-server` 门控——
//! 路由核心若直接引用 `crate::middleware::quota::*`，`--features router` 单独
//! 构建即断（历史缺陷）。反过来把契约塞进 `router` 则让「配额域读模型」成为
//! 路由模块的私有资产，离线回放、SDK、纯观测面等非路由消费者读一个预算数字
//! 都得连带开启 `vendor-management`（reqwest/limiteron）与 `oxcache`。
//!
//! 模块图：`quota_contract → config`；`middleware::quota → quota_contract`；
//! `router → quota_contract`。无回边。
//!
//! 语义边界：本模块不做记账，只做「窗口态 → 一次性快照」的表达与判定。

use crate::config::Config;

/// vendor 预算读模型（router 第 6 因子与 admin 端点共用）：预算窗的
/// 占用/限额/重置/消耗率**同一时刻**快照。语义方法（depleted/normalized_reset/
/// pace_ratio）为确定性纯函数——路由打分不做隐藏 IO。
///
/// # 不变量（实现方必须满足）
///
/// 1. 所有字段取自**同一时刻**（单次时钟读取），跨字段比值才有意义。
/// 2. `tokens_limit == 0` 或 `cost_limit == 0` 表示**该维不判**（不是「限额为零」）。
/// 3. `period_secs >= 1`（pace 的分母）。
/// 4. `reset_secs` 已由实现方钳制为 `>= 0`。
/// 5. 只暴露**当前窗**的占用：账本态停在上一窗时，实现方必须按新窗归零后再
///    返回（否则过期窗的 `used >= limit` 会让 `depleted()` 恒真，消费方据此
///    永久摘除该 vendor 且再无翻转机会）。
/// 6. 每个 vendor 至多一条预算窗（多条时取首条，其余忽略）。
///
/// # 单位
///
/// `cost_used` / `cost_limit` 单位为**毫分**（millicents，1e-5 USD），与配置面
/// `VendorBudgetConfig::cost_limit_millicents` 及 `[vendor_budgets]` 同口径。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VendorBudgetView {
    pub tokens_used: u64,
    pub tokens_limit: u64,
    pub cost_used: u64,
    pub cost_limit: u64,
    /// 距重置秒数（≥0）
    pub reset_secs: u64,
    /// 窗长秒数（pace 的 budget/period 分母）
    pub period_secs: u64,
    /// 窗已流逝秒数（pace 的 used/elapsed 分母）
    pub elapsed_secs: u64,
    /// 窗尚未起算（rolling 无状态）——smart 无重置压力（不加工成）
    pub not_started: bool,
}

impl VendorBudgetView {
    /// 预算耗尽：任一已判维度 used ≥ limit（R-vbr-002 摘除判据）
    pub fn depleted(&self) -> bool {
        (self.tokens_limit > 0 && self.tokens_used >= self.tokens_limit)
            || (self.cost_limit > 0 && self.cost_used >= self.cost_limit)
    }

    /// smart 归一化重置时刻 ∈ [0,1]：reset_secs / period_secs（0 = 即将
    /// 重置 → 加成最大；1 = 无压力）。窗未起算（rolling 无状态）→ 1.0。
    pub fn normalized_reset(&self) -> f64 {
        if self.not_started {
            return 1.0;
        }
        (self.reset_secs as f64 / self.period_secs.max(1) as f64).clamp(0.0, 1.0)
    }

    /// pace 消耗率比 = max_维度 (used/elapsed) / (limit/period)：>1.1 超带、
    /// <0.9 欠带（R-vbr-003）。elapsed=0（窗首秒/未起算）→ 0.0（满额待花）。
    pub fn pace_ratio(&self) -> f64 {
        if self.elapsed_secs == 0 {
            return 0.0;
        }
        let elapsed = self.elapsed_secs as f64;
        let period = self.period_secs.max(1) as f64;
        let mut ratio = 0.0f64;
        if self.tokens_limit > 0 {
            let token_rate =
                (self.tokens_used as f64 / elapsed) / (self.tokens_limit as f64 / period);
            ratio = ratio.max(token_rate);
        }
        if self.cost_limit > 0 {
            let cost_rate = (self.cost_used as f64 / elapsed) / (self.cost_limit as f64 / period);
            ratio = ratio.max(cost_rate);
        }
        ratio
    }
}

/// 预算读侧端口：router 只依赖本 trait，不依赖记账实现。
///
/// `vendor_budget_view` 的语义由实现方定义（真实实现读内存账本态）；
/// 未配置预算的 vendor 返回 `None`（不参与该因子）。
///
/// # 为什么是 `&Config` 而不是 `&VendorBudgetConfig`
///
/// 传整份快照是为**同一代钉住**：router 用同一份 `Arc<Config>` 做候选筛选与
/// 视图读取，热更新期间不会出现「视图取自新一代、候选来自旧一代」的撕裂。
/// 实现方只允许读 `config.vendor_budgets`。
///
/// # 为什么按单 vendor 而不是批量
///
/// 调用方（`Router::quota_factor_views`）自带按 vendor id 的记忆化缓存：同
/// vendor 多 model 候选只付一次加锁与一次时钟读取。批量接口会消灭这层记忆化
/// 的收益，并让「整批同一时刻」的实现细节泄漏到契约上。
pub trait VendorBudgetSource: Send + Sync {
    fn vendor_budget_view(&self, config: &Config, vendor_id: &str) -> Option<VendorBudgetView>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        tokens_used: u64,
        tokens_limit: u64,
        cost_used: u64,
        cost_limit: u64,
    ) -> VendorBudgetView {
        VendorBudgetView {
            tokens_used,
            tokens_limit,
            cost_used,
            cost_limit,
            reset_secs: 30,
            period_secs: 60,
            elapsed_secs: 60,
            not_started: false,
        }
    }

    /// limit=0 表示该维不判，不是「限额为零即耗尽」。
    #[test]
    fn zero_limit_dimension_is_not_judged() {
        assert!(!view(999_999, 0, 0, 0).depleted());
        // 单维超限即可判耗尽
        assert!(view(100, 100, 0, 0).depleted());
        assert!(view(0, 0, 100, 100).depleted());
        // 双维都在限内 → 未耗尽
        assert!(!view(50, 100, 50, 100).depleted());
    }

    #[test]
    fn normalized_reset_clamps_and_handles_not_started() {
        let mut v = view(0, 100, 0, 0);
        v.reset_secs = 0;
        v.period_secs = 60;
        assert_eq!(v.normalized_reset(), 0.0);
        v.reset_secs = 60;
        assert_eq!(v.normalized_reset(), 1.0);
        // 越界钳位
        v.reset_secs = 120;
        assert_eq!(v.normalized_reset(), 1.0);
        // 窗未起算（rolling）→ 视为无重置压力
        v.not_started = true;
        assert_eq!(v.normalized_reset(), 1.0);
    }

    #[test]
    fn pace_ratio_zero_elapsed_yields_zero() {
        let mut v = view(100, 100, 0, 0);
        v.elapsed_secs = 0;
        assert_eq!(v.pace_ratio(), 0.0, "窗首秒不判定消耗率");
    }

    #[test]
    fn pace_ratio_takes_max_over_dimensions() {
        // 每秒用 1 token，预算 60/60s = 1 token/s → ratio 1.0
        let v = view(60, 60, 0, 0);
        assert!((v.pace_ratio() - 1.0).abs() < 1e-9);
        // 成本维更紧：60 毫厘/60s 预算 = 1/s，实际 120 → 2.0 取大
        let v = view(10, 100, 120, 60);
        assert!((v.pace_ratio() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn period_secs_zero_is_treated_as_one() {
        let mut v = view(10, 100, 0, 0);
        v.period_secs = 0;
        // 除零防护：分母取下界 1
        assert!(v.pace_ratio().is_finite());
    }
}
