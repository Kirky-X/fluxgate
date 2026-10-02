// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 韧性栈模块 — FallbackChain + 错误映射层。
//!
//! 熔断器已整体迁移 `limiteron::circuit`（recId=2）：手写三态机存在
//! HalfOpen 死锁（half_open_calls 探测后不重置，熔断一次即永久拒绝），
//! 库版在每次状态转换统一清零计数（limiteron src/circuit/types.rs
//! finalize_transition）且提供假时钟注入，故删除等价手写。
//! resilience 模块现仅存：
//! - [`FallbackChain`]（供 API 层换路 fallback，recId=7）
//! - [`error_map`]：NexusFlowError ↔ LimiteronError 双向映射
//!   （limiteron `execute`/`ErrorClassifier` 签名锁定 LimiteronError，
//!   非等价手写熔断逻辑）
//!
//! ConnectionPool 位于 `crate::vendor::pool`（vendor 顶层模块），
//! 不在 resilience 子模块内 — pool 不依赖 resilience feature。

pub mod error_map;
pub mod fallback;

pub use error_map::{
    from_limiteron, is_circuit_open_error, to_limiteron, NexusFlowErrorClassifier,
};
pub use fallback::{FallbackChain, FallbackTarget, FallbackTrigger};
