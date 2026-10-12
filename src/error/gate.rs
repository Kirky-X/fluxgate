// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 路由核心错误：从原 `NexusFlowError` 拆出的核心变体集（冻结）。
//! `Database` 等应用面变体留应用面（受孤儿规则约束）；应用错误经 `From<GateError>` 包装。
//! 错误双轨：`#[error]` Display 恒英文规范串，HTTP 展示走 [`GateError::user_message()`]
//!（FTL，随 locale）；状态码/错误码/Retry-After 以固有方法暴露，由应用面 IntoResponse 消费。

use thiserror::Error;

use crate::i18n::t_args;

/// 路由核心错误变体集（冻结）。
#[derive(Error, Debug)]
pub enum GateError {
    #[error("Configuration error: {message}")]
    Config { message: String },

    #[error("Protocol conversion error: {source}")]
    Protocol {
        source: String,
        #[source]
        inner: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Vendor unavailable: {vendor}")]
    VendorUnavailable { vendor: String },

    #[error("Request timeout: {vendor}")]
    Timeout { vendor: String },

    #[error("Rate limited: {vendor}")]
    RateLimited { vendor: String },

    #[error("Request validation failed: {field}")]
    Validation { field: String },

    #[error("Routing error: {message}")]
    Routing { message: String },

    /// `[routing].retry_after_hint` 开启时抛出：fallback 耗尽且全部候选因限流被排除，
    /// 映射 429 + Retry-After；开关关闭时永不产生本变体。
    #[error("All candidate vendors rate limited: retry after {retry_after_secs}s")]
    AllVendorsThrottled { retry_after_secs: u64 },
}

impl GateError {
    /// 固定客户端状态映射（u16：本 crate 不引 http 类型，应用层转 `StatusCode`）。
    pub fn status_code(&self) -> u16 {
        match self {
            GateError::Validation { .. } => 400,
            GateError::VendorUnavailable { .. } => 503,
            GateError::Timeout { .. } => 504,
            GateError::RateLimited { .. } => 429,
            GateError::Config { .. } => 500,
            GateError::Protocol { .. } => 502,
            GateError::Routing { .. } => 500,
            GateError::AllVendorsThrottled { .. } => 429,
        }
    }

    /// 本地化用户消息（展示轨道）：随 locale 查 FTL 目录，写入响应体 error.message。
    pub fn user_message(&self) -> String {
        match self {
            GateError::Config { message } => {
                t_args("error-config", &[("message", message.clone())])
            }
            GateError::Protocol { source, .. } => {
                t_args("error-protocol", &[("source", source.clone())])
            }
            GateError::VendorUnavailable { vendor } => {
                t_args("error-vendor-unavailable", &[("vendor", vendor.clone())])
            }
            GateError::Timeout { vendor } => t_args("error-timeout", &[("vendor", vendor.clone())]),
            GateError::RateLimited { vendor } => {
                t_args("error-rate-limited", &[("vendor", vendor.clone())])
            }
            GateError::Validation { field } => {
                t_args("error-validation", &[("field", field.clone())])
            }
            GateError::Routing { message } => {
                t_args("error-routing", &[("message", message.clone())])
            }
            GateError::AllVendorsThrottled { retry_after_secs } => t_args(
                "error-all-vendors-throttled",
                &[("retry_after_secs", retry_after_secs.to_string())],
            ),
        }
    }

    /// 固定的错误类型代码字符串（不暴露内部枚举结构）
    pub fn error_code(&self) -> &'static str {
        match self {
            GateError::Config { .. } => "config_error",
            GateError::Protocol { .. } => "protocol_error",
            GateError::VendorUnavailable { .. } => "vendor_unavailable",
            GateError::Timeout { .. } => "timeout",
            GateError::RateLimited { .. } => "rate_limited",
            GateError::Validation { .. } => "validation_error",
            GateError::Routing { .. } => "routing_error",
            GateError::AllVendorsThrottled { .. } => "all_vendors_throttled",
        }
    }

    /// AllVendorsThrottled 携带的 Retry-After 头秒数，其余变体 None。
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            GateError::AllVendorsThrottled { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for GateError {
    fn from(_e: tokio::sync::oneshot::error::RecvError) -> Self {
        GateError::Timeout {
            vendor: "unknown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_message_uses_ftl_catalog() {
        // 键必须在 FTL 目录注册（缺键时 t_args 回退键名本身），占位符必须被实参替换
        let message = GateError::Config {
            message: "bad key".to_string(),
        }
        .user_message();
        assert_ne!(
            message, "error-config",
            "键 'error-config' 未注册进 FTL 目录"
        );
        assert!(
            message.contains("bad key"),
            "占位符 $message 必须被替换: {message}"
        );

        let throttled = GateError::AllVendorsThrottled {
            retry_after_secs: 7,
        }
        .user_message();
        assert_ne!(
            throttled, "error-all-vendors-throttled",
            "键 'error-all-vendors-throttled' 未注册进 FTL 目录"
        );
        assert!(
            throttled.contains('7'),
            "占位符 $retry_after_secs 必须被替换: {throttled}"
        );
    }

    #[test]
    fn test_status_code_table() {
        let cases: Vec<(GateError, u16)> = vec![
            (
                GateError::Validation {
                    field: "model".into(),
                },
                400,
            ),
            (
                GateError::VendorUnavailable {
                    vendor: "openai".into(),
                },
                503,
            ),
            (
                GateError::Timeout {
                    vendor: "openai".into(),
                },
                504,
            ),
            (
                GateError::RateLimited {
                    vendor: "openai".into(),
                },
                429,
            ),
            (
                GateError::Config {
                    message: "x".into(),
                },
                500,
            ),
            (
                GateError::Protocol {
                    source: "openai".into(),
                    inner: None,
                },
                502,
            ),
            (
                GateError::Routing {
                    message: "x".into(),
                },
                500,
            ),
            (
                GateError::AllVendorsThrottled {
                    retry_after_secs: 1,
                },
                429,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.status_code(), expected, "{err:?}");
        }
    }

    #[test]
    fn test_error_code_values() {
        assert_eq!(
            GateError::Validation {
                field: "model".into()
            }
            .error_code(),
            "validation_error"
        );
        assert_eq!(
            GateError::AllVendorsThrottled {
                retry_after_secs: 1
            }
            .error_code(),
            "all_vendors_throttled"
        );
    }

    #[test]
    fn test_retry_after_secs_only_for_all_throttled() {
        assert_eq!(
            GateError::AllVendorsThrottled {
                retry_after_secs: 30
            }
            .retry_after_secs(),
            Some(30)
        );
        assert_eq!(
            GateError::RateLimited {
                vendor: "openai".into()
            }
            .retry_after_secs(),
            None
        );
    }

    #[tokio::test]
    async fn test_error_from_recv_error() {
        // 构造 RecvError：发送端被丢弃后，接收端 await 会返回 RecvError
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);
        let recv_err = rx.await.unwrap_err();

        let err: GateError = recv_err.into();
        match err {
            GateError::Timeout { vendor } => {
                assert_eq!(
                    vendor, "unknown",
                    "From<RecvError> should set vendor to 'unknown'"
                );
            }
            _ => panic!("From<RecvError> should produce Timeout variant"),
        }
    }
}
