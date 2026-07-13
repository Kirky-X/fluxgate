// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 限流器模块 — 基于 limiteron 实现
//!
//! `TokenBucket` 和 `ConcurrencyLimiter` 作为配置包装器，
//! 内部委托给 `limiteron::limiters` 的对应实现。

use crate::config::RateLimitConfig;
use chrono::{Local, NaiveTime};
use limiteron::limiters::{ConcurrencyLimiter as LimiteronConcurrency, Limiter, TokenBucketLimiter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
}

impl ConcurrencyLimiter {
    pub fn new(max_concurrency: usize) -> Self {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrency));
        Self {
            inner: LimiteronConcurrency::with_dependencies(semaphore.clone(), None),
            semaphore,
        }
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

    /// 非阻塞检查是否可以获取许可（委托给 limiteron ConcurrencyLimiter::allow）
    pub async fn try_acquire(&self) -> bool {
        self.inner.allow(1).await.unwrap_or(false)
    }
}

/// 并发许可 — 持有 OwnedSemaphorePermit，drop 时自动释放
pub struct ConcurrencyPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

// 高级限流器实现 — 业务逻辑层，保留手写实现
pub enum AdvancedRateLimiter {
    TimeWindow {
        start: NaiveTime,
        end: NaiveTime,
    },
    Interval {
        interval_secs: u64,
        last_reset: AtomicU64,
    },
    Token {
        max_tokens: u64,
        used_tokens: AtomicU64,
        last_reset: AtomicU64,
    },
    None,
}

impl AdvancedRateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
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
            RateLimitConfig::Interval { hours } => Self::Interval {
                interval_secs: hours * 3600,
                last_reset: AtomicU64::new(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                ),
            },
            RateLimitConfig::Token { max_tokens } => Self::Token {
                max_tokens: *max_tokens,
                used_tokens: AtomicU64::new(0),
                last_reset: AtomicU64::new(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                ),
            },
        }
    }

    pub fn check(&self) -> bool {
        match self {
            Self::TimeWindow { start, end } => {
                let now = Local::now().time();
                now >= *start && now < *end
            }
            Self::Interval { .. } => true,
            Self::Token {
                max_tokens,
                used_tokens,
                ..
            } => used_tokens.load(Ordering::Relaxed) < *max_tokens,
            Self::None => true,
        }
    }

    pub fn consume_tokens(&self, tokens: u64) {
        if let Self::Token { used_tokens, .. } = self {
            used_tokens.fetch_add(tokens, Ordering::Relaxed);
        }
    }

    pub fn reset(&self) {
        match self {
            Self::Interval { last_reset, .. } => {
                last_reset.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                    Ordering::Relaxed,
                );
            }
            Self::Token {
                used_tokens,
                last_reset,
                ..
            } => {
                used_tokens.store(0, Ordering::Relaxed);
                last_reset.store(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                    Ordering::Relaxed,
                );
            }
            _ => {}
        }
    }

    pub fn should_reset(&self) -> bool {
        match self {
            Self::Interval {
                interval_secs,
                last_reset,
            } => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let last = last_reset.load(Ordering::Relaxed);
                now - last >= *interval_secs
            }
            _ => false,
        }
    }
}
