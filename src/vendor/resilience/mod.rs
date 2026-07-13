// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 韧性栈模块 — CircuitBreaker / FallbackChain / Retry。
//!
//! ConnectionPool 位于 `crate::vendor::pool`（vendor 顶层模块），
//! 不在 resilience 子模块内 — pool 不依赖 resilience feature。

pub mod circuit_breaker;
pub mod fallback;
pub mod retry;

pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, IsRetryable, State,
};
pub use fallback::{FallbackChain, FallbackTarget, FallbackTrigger};
pub use retry::with_retry;
