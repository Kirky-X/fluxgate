// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 基于 backon 的重试实现 — 替代 v0.1 手写版本。
//!
//! 使用 `ExponentialBuilder::default().with_jitter().with_max_times(3)` 配置：
//! - 默认最小延迟 1s，最大延迟 60s
//! - 启用 jitter 避免惊群
//! - 最多重试 3 次（共 4 次尝试）
//!
//! 默认行为：所有错误都会重试（与 backon 默认一致）。如需自定义重试条件，
//! 直接使用 `backon::Retryable` trait 配合 `.when(|e: &E| bool)`。

use std::future::Future;

use backon::{ExponentialBuilder, Retryable};

/// 基于 backon 的指数回退重试。
///
/// - 默认重试所有错误（backon 行为）
/// - 最多 3 次重试（共 4 次尝试）
/// - 启用 jitter 避免惊群
///
/// # 示例
///
/// ```ignore
/// use nexusflow::vendor::resilience::with_retry;
///
/// let result: Result<String, MyAppError> = with_retry(|| async {
///     fetch_data().await
/// }).await;
/// ```
pub async fn with_retry<F, Fut, T, E>(f: F) -> Result<T, E>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, E>> + Send,
    E: std::fmt::Debug + Send + 'static,
{
    f.retry(ExponentialBuilder::default().with_jitter().with_max_times(3))
        .await
}

/// 重导出 — 兼容 `vendor/manager.rs` 调用 `is_retryable_error(&NexusFlowError)`。
pub use crate::utils::retry::is_retryable_error;
