// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 限流器模块 — 基于 limiteron 实现
//!
//! `TokenBucket` 和 `ConcurrencyLimiter` 作为配置包装器，
//! 内部委托给 `limiteron::limiters` 的对应实现。

use crate::config::RateLimitConfig;
use chrono::{Local, NaiveTime};
use limiteron::limiters::{
    ConcurrencyLimiter as LimiteronConcurrency, FixedWindowLimiter, Limiter,
    ShardedSlidingWindowLimiter, TokenBucketLimiter,
};
use std::sync::Arc;
use std::time::Duration;

/// recId=13：TimeWindow 时段门控的统一判定（含跨午夜窗口）。
///
/// 缺陷背景：旧实现两处（本文件 `check()` 与 `manager::get_window_info`）
/// 均为同日判定——前者 `now >= start && now < end`，后者 `now < start ||
/// now >= end`——当 start > end（如 22:00–06:00 跨午夜配置）时段门控整段
/// 失效。提取为纯函数使两处判定同源（DRY）。
///
/// 语义（左闭右开）：同日窗口（start <= end）`start <= now < end`；
/// 跨午夜窗口（start > end）`now >= start || now < end`。
///
/// 纯函数不读时钟，now 由调用方注入，可脱离真实时间单测。每窗口可选
/// 时区字段（对齐 LiteLLM ZoneInfo）为进阶项，本轮不做。
pub fn time_window_active(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

/// 令牌桶限流器 — 委托给 limiteron::limiters::TokenBucketLimiter
pub struct TokenBucket {
    inner: TokenBucketLimiter,
}

impl TokenBucket {
    pub fn new(capacity: usize, requests_per_second: u32) -> Self {
        Self {
            inner: TokenBucketLimiter::new(capacity as u64, requests_per_second as u64),
        }
    }

    /// 异步获取一个令牌，若桶为空则等待直到有令牌可用
    pub async fn acquire(&self) {
        while !self.inner.allow(1).await.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// 并发控制器 — 委托给 limiteron::limiters::ConcurrencyLimiter
///
/// 使用共享的 `Arc<Semaphore>` 以支持 `acquire_owned()` 跨 spawn 边界持有许可
pub struct ConcurrencyLimiter {
    inner: LimiteronConcurrency,
    semaphore: Arc<tokio::sync::Semaphore>,
    /// recId=6：构造时保存的并发上限 — `in_flight()` 的被减数。
    /// 信号量本身不暴露容量（available_permits 会随占用变化），
    /// 上限必须在构造点留存。
    max_concurrency: usize,
}

impl ConcurrencyLimiter {
    pub fn new(max_concurrency: usize) -> Self {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency));
        Self {
            inner: LimiteronConcurrency::with_dependencies(semaphore.clone(), None),
            semaphore,
            max_concurrency,
        }
    }

    /// recId=6：当前在途请求数 = 上限 − 可用许可（已发放未释放的 permit 数）。
    ///
    /// 零新增状态：信号量本就是权威计数器，acquire/drop 自动增减，流式
    /// permit 随流存活（recId=5④）故对长流真实生效。saturating_sub 防
    /// 防御性钳位（正常路径 available_permits ≤ max）。
    pub fn in_flight(&self) -> usize {
        self.max_concurrency
            .saturating_sub(self.semaphore.available_permits())
    }

    /// recId=16：并发上限（构造入参）— 准入的拥塞信号分母（Σ 上限）。
    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    /// 异步获取一个并发许可，返回持有的 OwnedSemaphorePermit
    pub async fn acquire(&self) -> ConcurrencyPermit {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Semaphore closed");
        ConcurrencyPermit { _permit: permit }
    }

    /// recId=18：token 加权获取 —— 原子获取 cost 张许可（信号量多许可
    /// 语义，与 limiteron ConcurrencyLimiter::acquire(cost) 的
    /// acquire_many 对照 concurrency.rs:193-215）。cost 鉗 [1, 容量]：
    /// 低于 1 按单票据；超过容量按容量截断（否则 acquire_many 永久等待
    /// 死锁——估算 max 可配置大于并配上限）。#6 的 in_flight（容量−可用）
    /// 自动变为 token 加权口径（同一计数点、不同加权维度）。
    pub async fn acquire_weighted(&self, cost: usize) -> ConcurrencyPermit {
        let cost = cost.clamp(1, self.max_concurrency) as u32;
        let permit = self
            .semaphore
            .clone()
            .acquire_many_owned(cost)
            .await
            .expect("Semaphore closed");
        ConcurrencyPermit { _permit: permit }
    }

    /// 非阻塞检查是否可以获取许可（委托给 limiteron ConcurrencyLimiter::allow）
    pub async fn try_acquire(&self) -> bool {
        self.inner.allow(1).await.unwrap_or(false)
    }
}

/// 并发许可 — 持有 OwnedSemaphorePermit，drop 时自动释放
pub struct ConcurrencyPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// recId=11：窗口内部实现 — `RateLimitConfig` 的 `sliding` 开关在构造期
/// 选定（枚举分派而非 `dyn Limiter`，避免对象安全不确定性）。两实现的
/// `allow(cost)`/`peek(cost)`/`RateLimitSnapshot` 同构（limiteron
/// sharded_sliding_window.rs:290-306 与 fixed_window.rs 对照）。
/// `pub` 仅为满足 pub enum 变体字段的可见性约束（构造收敛于
/// `AdvancedRateLimiter::with_clock`/`build_window`，外部不应直接构造）。
pub enum WindowInner {
    /// 固定窗口（默认）——窗口翻越点两侧可各打满配额（瞬时 2×），现状语义
    Fixed(Arc<FixedWindowLimiter>),
    /// 滑动窗口（`sliding: true`）——任意 1×窗口长度内配额守恒，消除
    /// 翻越点双花。第二字段 = 窗口全长秒（空窗 reset_secs 修正用，见
    /// [`Self::window_snapshot`]）。
    Sliding(Arc<ShardedSlidingWindowLimiter>, u64),
}

/// 高级限流器 — 业务逻辑层。
///
/// recId=5：`Token` / `Interval` 变体迁移 `limiteron::limiters::FixedWindowLimiter`——
/// 其 `allow(cost)` 内置 `check_and_reset_window()` + CAS 原子消费
/// （limiteron src/limiters/fixed_window.rs:135-157），一举修掉两个缺陷：
/// 1. 旧 `should_reset` 只匹配 Interval 变体、`_ => false`，Token 窗口永不重置
///    → vendor 被路由永久排除；
/// 2. 旧 check() 与 consume_tokens 分离非原子，可超卖。
///
/// 窗口到期由 `check_and_reset_window` 惰性重置，无需后台 reset worker。
///
/// recId=11：`sliding: true` 时内部换 `ShardedSlidingWindowLimiter`
/// （`with_clock` 支持时钟注入，MockClock 测试路径保持；`allow(cost)`
/// 支持 token 计量消费。明确不用 `SlidingWindowLimiter`——已
/// `#[deprecated]` 且 allow 硬编码 `Instant::now` 破坏时钟注入）。
///
/// `TimeWindow` 变体保留手写：limiteron 无「每日时段」窗口原语
/// （fixed/quota 均为时长窗口），手写为最小实现。
pub enum AdvancedRateLimiter {
    TimeWindow {
        start: NaiveTime,
        end: NaiveTime,
    },
    /// 固定时长请求窗口（无数量上限，仅作窗口标记）
    Interval {
        inner: WindowInner,
    },
    /// Token 配额窗口（窗口惰性重置 + CAS 原子扣减）
    Token {
        max_tokens: u64,
        inner: WindowInner,
    },
    None,
}

impl AdvancedRateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self::with_clock(config, Arc::new(limiteron::SystemClock))
    }

    /// 时钟注入构造（测试用：MockClock 驱动窗口重置回归）。
    #[doc(hidden)]
    pub fn with_clock(config: &RateLimitConfig, clock: Arc<dyn limiteron::Clock>) -> Self {
        match config {
            RateLimitConfig::TimeWindow { start, end } => {
                let start_time = NaiveTime::parse_from_str(start, "%H:%M")
                    .unwrap_or_else(|_| NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                let end_time = NaiveTime::parse_from_str(end, "%H:%M")
                    .unwrap_or_else(|_| NaiveTime::from_hms_opt(23, 59, 59).unwrap());
                Self::TimeWindow {
                    start: start_time,
                    end: end_time,
                }
            }
            RateLimitConfig::Interval { hours, sliding } => Self::Interval {
                // 无数量上限语义：u64::MAX 容量下 allow 恒放行，仅窗口计时生效
                inner: Self::build_window(
                    Duration::from_secs(hours.saturating_mul(3600)),
                    u64::MAX,
                    *sliding,
                    clock,
                ),
            },
            RateLimitConfig::Token {
                max_tokens,
                window_hours,
                sliding,
            } => {
                let hours = window_hours.unwrap_or(24).max(1);
                Self::Token {
                    max_tokens: *max_tokens,
                    inner: Self::build_window(
                        Duration::from_secs(hours * 3600),
                        *max_tokens,
                        *sliding,
                        clock,
                    ),
                }
            }
        }
    }

    /// recId=11：按 sliding 开关选定窗口实现（构造期定死，热更改
    /// sliding 需经 vendor 配置更新 + 实例重建路径）。
    fn build_window(
        window: Duration,
        max: u64,
        sliding: bool,
        clock: Arc<dyn limiteron::Clock>,
    ) -> WindowInner {
        if sliding {
            WindowInner::Sliding(
                Arc::new(ShardedSlidingWindowLimiter::with_clock(window, max, clock)),
                window.as_secs().max(1),
            )
        } else {
            WindowInner::Fixed(Arc::new(FixedWindowLimiter::with_clock(window, max, clock)))
        }
    }

    /// 非消费预检（recId=5②）：peek 读窗口 remaining，不递增。
    /// 原 check() 与 consume_tokens 分离的 check-then-act 超卖窗口随之消除。
    pub async fn check(&self) -> bool {
        match self {
            Self::TimeWindow { start, end } => {
                // recId=13：跨午夜判定统一委托 time_window_active
                let now = Local::now().time();
                time_window_active(now, *start, *end)
            }
            Self::Interval { inner } | Self::Token { inner, .. } => match inner {
                WindowInner::Fixed(w) => w.peek(1).await.map(|snap| snap.allows(1)).unwrap_or(true),
                WindowInner::Sliding(w, _) => {
                    w.peek(1).await.map(|snap| snap.allows(1)).unwrap_or(true)
                }
            },
            Self::None => true,
        }
    }

    /// 该变体是否承载 token 配额记账语义。
    ///
    /// Interval/TimeWindow/None 无配额上限，`consume_tokens` 恒 false 不代表
    /// 「配额将超限被拒」——调用方（worker/passthrough 记账点）应先查本方法，
    /// 非 Token 变体直接跳过记账分支，避免误导性的 quota-underflow 告警
    /// （评审 major：Interval vendor 每个成功请求都打 underflow warn）。
    pub fn accounts_tokens(&self) -> bool {
        matches!(self, Self::Token { .. })
    }

    /// 原子扣减 token 配额（CAS 全有全无）：剩余不足时整体拒绝并返回 false，
    /// 不再像旧 fetch_add 一样无上限累加（超卖）。
    pub async fn consume_tokens(&self, tokens: u64) -> bool {
        match self {
            Self::Token { inner, .. } => match inner {
                WindowInner::Fixed(w) => w.allow(tokens).await.unwrap_or(false),
                WindowInner::Sliding(w, _) => w.allow(tokens).await.unwrap_or(false),
            },
            _ => false,
        }
    }

    /// 窗口快照（recId=5②：给 get_window_info 提供精确 remaining/reset_secs）。
    /// Token/Interval 返回 `Some((limit, used, reset_secs))`；其余变体 None。
    pub async fn window_snapshot(&self) -> Option<(u64, u64, u64)> {
        match self {
            Self::Interval { inner } | Self::Token { inner, .. } => {
                let (snap, window_secs) = match inner {
                    WindowInner::Fixed(w) => (w.peek(1).await.ok()?, 0),
                    WindowInner::Sliding(w, window_secs) => (w.peek(1).await.ok()?, *window_secs),
                };
                let mut reset_secs = snap.reset_secs;
                // recId=11 评审修复（major）：sharded 滑窗空窗（当前窗零消费）
                // 的 reset_secs=0 是「即时恢复」语义，但路由权重面
                // get_window_info 把它映射为 window_end=now →
                // WindowInfo::is_ending_soon(<300s) 对完全空闲的 sliding
                // vendor 恒 true → window_factor 恒 0.5——最健康的零消耗
                // vendor 反被永久打折，方向与 window_factor 语义相反。
                // 修正：空窗（无任何消费）以窗口全长上报（下一次翻转至多
                // 一个全窗之外），与 fixed 空窗 reset>0 的行为对齐。
                if window_secs > 0 && snap.remaining == snap.limit {
                    reset_secs = window_secs;
                }
                Some((
                    snap.limit,
                    snap.limit.saturating_sub(snap.remaining),
                    reset_secs,
                ))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ======== recId=6：ConcurrencyLimiter::in_flight（路由负载信号） ========

    /// acquire n 个 permit → in_flight == n；Drop 后归零；max 与构造入参一致
    ///（max 为 recId=6 新增字段的直接断言）。信号量容量 == max：第 max+1 个
    /// acquire 会阻塞，此处以「max 个全部可同时持有」证明容量正确。
    #[tokio::test]
    async fn in_flight_tracks_acquired_permits_and_max() {
        let limiter = ConcurrencyLimiter::new(3);
        // 新增字段的直接断言（同模块测试可读私有字段）
        assert_eq!(limiter.max_concurrency, 3);
        assert_eq!(limiter.in_flight(), 0, "空载在途为 0");

        let p1 = limiter.acquire().await;
        let p2 = limiter.acquire().await;
        assert_eq!(limiter.in_flight(), 2, "acquire 2 个 → in_flight == 2");

        // max=3：第 3 个 permit 可立即获取（占满），第 4 个会阻塞——
        // 用 try_acquire 不行的语义（inner.allow 消费路径不同），
        // 直接证明信号量容量：3 个 permit 全部持有成功
        let p3 = limiter.acquire().await;
        assert_eq!(
            limiter.in_flight(),
            3,
            "max 个 permit 全部持有 → in_flight == max"
        );

        drop(p1);
        assert_eq!(limiter.in_flight(), 2, "Drop 一个 permit → 递减");
        drop(p2);
        drop(p3);
        assert_eq!(limiter.in_flight(), 0, "全部 Drop 后归零");
    }

    /// max=1 的退化上限：acquire 后 in_flight==1，Drop 归零
    ///（VendorInstance 缺省上限 = 队列容量，显式 1 是 tests/router_test.rs
    /// 集成测试的配置形态）。
    #[tokio::test]
    async fn in_flight_with_single_permit_limit() {
        let limiter = ConcurrencyLimiter::new(1);
        assert_eq!(limiter.max_concurrency, 1);
        let permit = limiter.acquire().await;
        assert_eq!(limiter.in_flight(), 1);
        drop(permit);
        assert_eq!(limiter.in_flight(), 0);
    }
}
