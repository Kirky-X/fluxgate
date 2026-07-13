// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 靟性栈模块 — CircuitBreaker / FallbackChain / Retry / ConnectionPool。

pub mod circuit_breaker;
pub mod fallback;
pub mod pool;
pub mod retry;

pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, IsRetryable};
pub use fallback::{FallbackChain, FallbackTarget, FallbackTrigger};
pub use pool::ConnectionPool;
pub use retry::with_retry;
