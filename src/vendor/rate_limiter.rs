// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 限流器模块 — 基于 limiteron 实现
//!
//! `TokenBucket` / `ConcurrencyLimiter` 为配置包装器，内部委托 limiteron::limiters。

use crate::config::RateLimitConfig;
use chrono::{Local, NaiveTime};
use limiteron::limiters::{
    ConcurrencyLimiter as LimiteronConcurrency, FixedWindowLimiter, Limiter,
    ShardedSlidingWindowLimiter, TokenBucketLimiter,
};
use std::sync::Arc;
use std::time::Duration;

/// TimeWindow 时段门控判定（左闭右开）：同日窗口 `start <= now < end`；跨午夜窗口
/// `now >= start || now < end`。纯函数不读时钟，now 由调用方注入（可脱离真实时间单测）。
pub fn time_window_active(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}

pub struct TokenBucket {
    inner: TokenBucketLimiter,
}

impl TokenBucket {
    pub fn new(capacity: usize, requests_per_second: u32) -> Self {
        Self {
            inner: TokenBucketLimiter::new(capacity as u64, requests_per_second as u64),
        }
    }

    /// 桶空则等待直到有令牌。
    pub async fn acquire(&self) {
        while !self.inner.allow(1).await.unwrap_or(false) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub struct ConcurrencyLimiter {
    inner: LimiteronConcurrency,
    semaphore: Arc<tokio::sync::Semaphore>,
    /// 并发上限 — 信号量不暴露固定容量（available_permits 随占用变化）。
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

    /// 当前在途请求数 = 上限 − 可用许可；信号量为权威计数器，流式 permit 随流存活。
    pub fn in_flight(&self) -> usize {
        self.max_concurrency
            .saturating_sub(self.semaphore.available_permits())
    }

    /// 并发上限（构造入参）— 准入的拥塞信号分母（Σ 上限）。
    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    pub async fn acquire(&self) -> ConcurrencyPermit {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("Semaphore closed");
        ConcurrencyPermit { _permit: permit }
    }

    /// token 加权获取 — 原子获取 cost 张许可；cost 钳制 [1, 容量]（超容量会永久等待）。
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

    pub async fn try_acquire(&self) -> bool {
        self.inner.allow(1).await.unwrap_or(false)
    }
}

/// 并发许可 — drop 时自动释放 permit。
pub struct ConcurrencyPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// 窗口内部实现 — `sliding` 开关构造期选定；`pub` 仅为满足 pub enum 变体字段可见性。
pub enum WindowInner {
    /// 固定窗口（默认）——翻越点两侧可各打满配额（瞬时 2×）。
    Fixed(Arc<FixedWindowLimiter>),
    /// 滑动窗口（`sliding: true`）——任意 1×窗长内配额守恒；第二字段 = 窗口全长秒。
    Sliding(Arc<ShardedSlidingWindowLimiter>, u64),
}

/// 高级限流器 — 业务逻辑层：Token/Interval 委托 limiteron 窗口原语（惰性重置 + CAS，
/// `sliding: true` 换 Sharded 滑动窗口）；`TimeWindow` 保留手写（limiteron 无此原语）。
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

    /// 按 sliding 开关选定窗口实现（构造期定死；热改需配置更新 + 实例重建）。
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

    /// 非消费预检：peek 读窗口 remaining，不递增。
    pub async fn check(&self) -> bool {
        match self {
            Self::TimeWindow { start, end } => {
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

    /// 该变体是否承载 token 配额记账语义；非 Token 变体 `consume_tokens` 恒 false（避免误导 underflow 告警）。
    pub fn accounts_tokens(&self) -> bool {
        matches!(self, Self::Token { .. })
    }

    /// 原子扣减 token 配额（CAS 全有全无）：剩余不足时整体拒绝返回 false。
    pub async fn consume_tokens(&self, tokens: u64) -> bool {
        match self {
            Self::Token { inner, .. } => match inner {
                WindowInner::Fixed(w) => w.allow(tokens).await.unwrap_or(false),
                WindowInner::Sliding(w, _) => w.allow(tokens).await.unwrap_or(false),
            },
            _ => false,
        }
    }

    /// 窗口快照：Token/Interval 返回 Some((limit, used, reset_secs))，其余 None。
    pub async fn window_snapshot(&self) -> Option<(u64, u64, u64)> {
        match self {
            Self::Interval { inner } | Self::Token { inner, .. } => {
                let (snap, window_secs) = match inner {
                    WindowInner::Fixed(w) => (w.peek(1).await.ok()?, 0),
                    WindowInner::Sliding(w, window_secs) => (w.peek(1).await.ok()?, *window_secs),
                };
                let mut reset_secs = snap.reset_secs;
                // 空窗 reset_secs=0 会被路由映射成 window_end=now，令空闲 sliding vendor 恒被判「即将结束」——以窗口全长上报修正。
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

    // ======== ConcurrencyLimiter::in_flight（路由负载信号） ========

    #[tokio::test]
    async fn in_flight_tracks_acquired_permits_and_max() {
        let limiter = ConcurrencyLimiter::new(3);
        assert_eq!(limiter.max_concurrency, 3);
        assert_eq!(limiter.in_flight(), 0, "空载在途为 0");

        let p1 = limiter.acquire().await;
        let p2 = limiter.acquire().await;
        assert_eq!(limiter.in_flight(), 2, "acquire 2 个 → in_flight == 2");

        // max=3：3 个 permit 全部同时持有，证明信号量容量 == max
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
