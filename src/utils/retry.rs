// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 智能重试机制模块
//!
//! 根据 PRD 设计，实现按状态码分类的重试策略：
//! - 400, 401, 403, 404: 标记厂商 Unavailable，不重试
//! - 429: 指数回退重试（1s, 2s, 4s, 8s, 16s）
//! - 500, 502, 503: 重试 3 次，间隔 1s
//! - 网络超时: 重试 2 次

use crate::error::NexusFlowError;
use std::time::Duration;

/// 错误分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// 不可重试 - 标记厂商不可用
    Fatal,
    /// 可重试 - 指数回退（429）
    RetryableExponential,
    /// 可重试 - 固定间隔（500/502/503）
    RetryableFixed,
    /// 可重试 - 简短重试（超时）
    RetryableShort,
}

impl ErrorCategory {
    /// 根据 HTTP 状态码判断错误分类
    pub fn from_status_code(status: u16) -> Self {
        match status {
            // 客户端错误 - 不重试
            400 | 401 | 403 | 404 => Self::Fatal,
            // 限流 - 指数回退
            429 => Self::RetryableExponential,
            // 服务器错误 - 固定间隔重试
            500 | 502 | 503 => Self::RetryableFixed,
            // 其他错误
            _ => Self::Fatal,
        }
    }

    /// 判断是否为超时错误
    pub fn from_error_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("connection refused")
            || lower.contains("connection reset")
        {
            Self::RetryableShort
        } else {
            Self::Fatal
        }
    }

    /// 是否可重试
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::Fatal)
    }

    /// 最大重试次数
    pub fn max_retries(&self) -> u32 {
        match self {
            Self::Fatal => 0,
            Self::RetryableExponential => 5,
            Self::RetryableFixed => 3,
            Self::RetryableShort => 2,
        }
    }
}

/// 重试策略
#[derive(Debug, Clone)]
pub struct RetryStrategy {
    pub max_retries: u32,
    pub backoff: BackoffStrategy,
}

impl RetryStrategy {
    /// 根据错误分类创建重试策略
    pub fn for_category(category: ErrorCategory) -> Self {
        match category {
            ErrorCategory::Fatal => Self {
                max_retries: 0,
                backoff: BackoffStrategy::None,
            },
            ErrorCategory::RetryableExponential => Self {
                max_retries: 5,
                backoff: BackoffStrategy::Exponential {
                    base_ms: 1000,
                    max_ms: 16000,
                },
            },
            ErrorCategory::RetryableFixed => Self {
                max_retries: 3,
                backoff: BackoffStrategy::Fixed { interval_ms: 1000 },
            },
            ErrorCategory::RetryableShort => Self {
                max_retries: 2,
                backoff: BackoffStrategy::Fixed { interval_ms: 500 },
            },
        }
    }
}

/// 回退策略
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// 无回退
    None,
    /// 指数回退
    Exponential { base_ms: u64, max_ms: u64 },
    /// 固定间隔
    Fixed { interval_ms: u64 },
}

impl BackoffStrategy {
    /// 计算指定尝试次数的延迟
    pub fn delay(&self, attempt: u32) -> Duration {
        match self {
            Self::None => Duration::from_millis(0),
            Self::Exponential { base_ms, max_ms } => {
                let exp_delay = base_ms * 2u64.pow(attempt.min(4));
                Duration::from_millis(exp_delay.min(*max_ms))
            }
            Self::Fixed { interval_ms } => Duration::from_millis(*interval_ms),
        }
    }
}

/// 提取 HTTP 状态码
pub fn extract_status_code(error: &str) -> u16 {
    // 尝试从错误消息中提取状态码
    // 例如: "VendorUnavailable: anthropic_main (429: Rate limit exceeded)"
    if let Some(start) = error.find('(') {
        let rest = &error[start + 1..];
        if let Some(end) = rest.find(':') {
            if let Ok(code) = rest[..end].trim().parse::<u16>() {
                return code;
            }
        }
        // 尝试直接解析括号内的数字
        if let Ok(code) = rest.split_whitespace().next().unwrap_or("").parse::<u16>() {
            return code;
        }
    }
    500 // 默认返回服务器错误
}

/// 判断错误是否可重试
pub fn is_retryable_error(error: &NexusFlowError) -> bool {
    match error {
        NexusFlowError::VendorUnavailable { vendor, .. } => {
            let code = extract_status_code(vendor);
            let category = ErrorCategory::from_status_code(code);
            category.is_retryable()
        }
        NexusFlowError::Timeout { .. } => true,
        _ => false,
    }
}

/// 执行带重试的请求
pub async fn with_retry<F, Fut, T>(
    mut f: F,
    initial_error: Option<NexusFlowError>,
) -> Result<T, NexusFlowError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, NexusFlowError>>,
{
    // 确定初始错误类别
    let initial_category = if let Some(ref err) = initial_error {
        match err {
            NexusFlowError::Timeout { .. } => ErrorCategory::RetryableShort,
            NexusFlowError::VendorUnavailable { vendor, .. } => {
                let code = extract_status_code(vendor);
                ErrorCategory::from_status_code(code)
            }
            _ => ErrorCategory::Fatal,
        }
    } else {
        return (f)().await;
    };

    let strategy = RetryStrategy::for_category(initial_category);

    if strategy.max_retries == 0 {
        return Err(initial_error.unwrap_or(NexusFlowError::Protocol {
            source: "Unknown error".into(),
            inner: None,
        }));
    }

    let mut last_error = initial_error;

    for attempt in 0..strategy.max_retries {
        // 执行请求
        match (f)().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                // 检查错误是否仍然可重试
                if !is_retryable_error(&e) {
                    return Err(e);
                }
                last_error = Some(e);

                // 计算延迟
                let delay = strategy.backoff.delay(attempt);
                if delay > Duration::from_secs(0) {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or(NexusFlowError::Protocol {
        source: "Max retries exceeded".into(),
        inner: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_category_from_status() {
        assert_eq!(ErrorCategory::from_status_code(400), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(401), ErrorCategory::Fatal);
        assert_eq!(
            ErrorCategory::from_status_code(429),
            ErrorCategory::RetryableExponential
        );
        assert_eq!(
            ErrorCategory::from_status_code(500),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(502),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(503),
            ErrorCategory::RetryableFixed
        );
    }

    #[test]
    fn test_backoff_delay() {
        let exponential = BackoffStrategy::Exponential {
            base_ms: 1000,
            max_ms: 16000,
        };

        assert_eq!(exponential.delay(0), Duration::from_millis(1000));
        assert_eq!(exponential.delay(1), Duration::from_millis(2000));
        assert_eq!(exponential.delay(2), Duration::from_millis(4000));
        assert_eq!(exponential.delay(3), Duration::from_millis(8000));
        assert_eq!(exponential.delay(4), Duration::from_millis(16000));
        assert_eq!(exponential.delay(5), Duration::from_millis(16000)); // capped

        let fixed = BackoffStrategy::Fixed { interval_ms: 1000 };
        assert_eq!(fixed.delay(0), Duration::from_millis(1000));
        assert_eq!(fixed.delay(10), Duration::from_millis(1000));
    }

    #[test]
    fn test_extract_status_code() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
        assert_eq!(extract_status_code("no status code here"), 500); // default
    }
}
