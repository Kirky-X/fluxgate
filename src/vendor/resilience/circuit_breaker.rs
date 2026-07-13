// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! CircuitBreaker 三态机 — Closed / Open / HalfOpen。
//!
//! 使用 arc-swap 实现无锁状态转换。

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

/// 错误是否可重试 — 由调用方实现。
pub trait IsRetryable {
    fn is_retryable(&self) -> bool;
}

/// 燃断器错误 — 包装内部错误或熔断拒绝。
#[derive(Debug)]
pub enum CircuitBreakerError<E> {
    /// 熔断中，请求被拒绝
    Open,
    /// 内部函数返回的错误
    Inner(E),
}

impl<E> CircuitBreakerError<E> {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn into_inner(self) -> Option<E> {
        match self {
            Self::Inner(e) => Some(e),
            _ => None,
        }
    }
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitBreakerError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "circuit breaker open"),
            Self::Inner(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::error::Error> std::error::Error for CircuitBreakerError<E> {}

/// 燃断器配置。
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub open_timeout: Duration,
    pub half_open_max_calls: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 3,
            open_timeout: Duration::from_secs(30),
            half_open_max_calls: 1,
        }
    }
}

/// 燃断器状态。
#[derive(Debug, Clone)]
pub enum State {
    Closed,
    Open { opened_at: Instant },
    HalfOpen { probe_calls: u32 },
}

/// 无锁燃断器。
pub struct CircuitBreaker {
    state: ArcSwap<State>,
    failure_count: AtomicU64,
    success_count: AtomicU64,
    half_open_calls: AtomicU64,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: ArcSwap::from_pointee(State::Closed),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            half_open_calls: AtomicU64::new(0),
            config,
        }
    }

    /// 读取当前状态克隆。
    pub fn state(&self) -> State {
        self.state.load_full().as_ref().clone()
    }

    /// 尝试从 Open 转 HalfOpen（如果已超时）。
    fn maybe_half_open(&self) {
        let current = self.state.load_full();
        if let State::Open { opened_at } = current.as_ref() {
            if opened_at.elapsed() >= self.config.open_timeout {
                self.half_open_calls.store(0, Ordering::SeqCst);
                self.success_count.store(0, Ordering::SeqCst);
                self.state.store(Arc::new(State::HalfOpen { probe_calls: 0 }));
            }
        }
    }

    /// 包装异步函数调用。
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, CircuitBreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: IsRetryable,
    {
        self.maybe_half_open();
        let current = self.state.load_full();

        match current.as_ref() {
            State::Open { .. } => Err(CircuitBreakerError::Open),

            State::HalfOpen { .. } => {
                let calls = self.half_open_calls.fetch_add(1, Ordering::SeqCst);
                if calls >= self.config.half_open_max_calls as u64 {
                    return Err(CircuitBreakerError::Open);
                }

                let result = f().await;
                match &result {
                    Ok(_) => {
                        let sc = self.success_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if sc >= self.config.success_threshold as u64 {
                            self.failure_count.store(0, Ordering::SeqCst);
                            self.state.store(Arc::new(State::Closed));
                        }
                    }
                    Err(e) if e.is_retryable() => {
                        self.state.store(Arc::new(State::Open {
                            opened_at: Instant::now(),
                        }));
                    }
                    _ => {}
                }
                result.map_err(CircuitBreakerError::Inner)
            }

            State::Closed => {
                let result = f().await;
                match &result {
                    Ok(_) => {
                        self.failure_count.store(0, Ordering::SeqCst);
                    }
                    Err(e) if e.is_retryable() => {
                        let fc = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;
                        if fc >= self.config.failure_threshold as u64 {
                            self.state.store(Arc::new(State::Open {
                                opened_at: Instant::now(),
                            }));
                        }
                    }
                    _ => {}
                }
                result.map_err(CircuitBreakerError::Inner)
            }
        }
    }

    #[doc(hidden)]
    pub fn force_open(&self) {
        self.state.store(Arc::new(State::Open {
            opened_at: Instant::now(),
        }));
    }

    #[doc(hidden)]
    pub fn force_half_open(&self) {
        self.half_open_calls.store(0, Ordering::SeqCst);
        self.success_count.store(0, Ordering::SeqCst);
        self.state.store(Arc::new(State::HalfOpen { probe_calls: 0 }));
    }

    #[doc(hidden)]
    pub fn force_closed(&self) {
        self.failure_count.store(0, Ordering::SeqCst);
        self.success_count.store(0, Ordering::SeqCst);
        self.half_open_calls.store(0, Ordering::SeqCst);
        self.state.store(Arc::new(State::Closed));
    }
}
