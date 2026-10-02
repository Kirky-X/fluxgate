// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// 错误处理模块
#[cfg(feature = "web-server")]
use axum::response::IntoResponse;
#[cfg(feature = "web-server")]
use serde_json::json;
use thiserror::Error;

use crate::i18n::t_args;

/// 错误双轨：`#[error]` Display 恒英文规范串（日志 / 错误链），HTTP 展示
/// 走 [`NexusFlowError::user_message()`]（FTL，随 locale，见 i18n 基线 §4）。
#[derive(Error, Debug)]
pub enum NexusFlowError {
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

    #[error("Database error: {message}")]
    Database { message: String },

    #[error("Request validation failed: {field}")]
    Validation { field: String },

    #[error("Routing error: {message}")]
    Routing { message: String },

    /// recId=19(c)：换路 fallback 耗尽且全部候选因限流被排除 —
    /// `[routing].retry_after_hint` 开启时由 api 层收尾函数抛出，
    /// IntoResponse 映射 429 + Retry-After 头（reset 秒数取各 vendor
    /// 窗口重置剩余的 min）。默认开关关闭时永不产生本变体（原错误
    /// 冒泡保持 503 语义）。
    #[error("All candidate vendors rate limited: retry after {retry_after_secs}s")]
    AllVendorsThrottled { retry_after_secs: u64 },
}

#[cfg(feature = "web-server")]
impl IntoResponse for NexusFlowError {
    fn into_response(self) -> axum::response::Response {
        // recId=19(c)：AllVendorsThrottled 额外携带 Retry-After 响应头
        let retry_after_secs = match &self {
            NexusFlowError::AllVendorsThrottled { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        };
        let status = match &self {
            NexusFlowError::Validation { .. } => axum::http::StatusCode::BAD_REQUEST,
            NexusFlowError::VendorUnavailable { .. } => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            NexusFlowError::Timeout { .. } => axum::http::StatusCode::GATEWAY_TIMEOUT,
            NexusFlowError::RateLimited { .. } => axum::http::StatusCode::TOO_MANY_REQUESTS,
            NexusFlowError::Config { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::Protocol { .. } => axum::http::StatusCode::BAD_GATEWAY,
            NexusFlowError::Database { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::Routing { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::AllVendorsThrottled { .. } => axum::http::StatusCode::TOO_MANY_REQUESTS,
        };

        let body = json!({
            "error": {
                "message": self.user_message(),
                "type": self.error_code(),
            }
        });

        let mut response = (status, axum::Json(body)).into_response();
        if let Some(secs) = retry_after_secs {
            if let Ok(value) = secs.to_string().parse() {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
        }
        response
    }
}

#[cfg(feature = "web-server")]
impl NexusFlowError {
    /// 本地化用户消息（展示轨道）：随 locale 查 FTL 目录；与英文规范
    /// Display 分轨，写入 HTTP 响应体 `error.message`。
    fn user_message(&self) -> String {
        match self {
            NexusFlowError::Config { message } => {
                t_args("error-config", &[("message", message.clone())])
            }
            NexusFlowError::Protocol { source, .. } => {
                t_args("error-protocol", &[("source", source.clone())])
            }
            NexusFlowError::VendorUnavailable { vendor } => {
                t_args("error-vendor-unavailable", &[("vendor", vendor.clone())])
            }
            NexusFlowError::Timeout { vendor } => {
                t_args("error-timeout", &[("vendor", vendor.clone())])
            }
            NexusFlowError::RateLimited { vendor } => {
                t_args("error-rate-limited", &[("vendor", vendor.clone())])
            }
            NexusFlowError::Database { message } => {
                t_args("error-database", &[("message", message.clone())])
            }
            NexusFlowError::Validation { field } => {
                t_args("error-validation", &[("field", field.clone())])
            }
            NexusFlowError::Routing { message } => {
                t_args("error-routing", &[("message", message.clone())])
            }
            NexusFlowError::AllVendorsThrottled { retry_after_secs } => t_args(
                "error-all-vendors-throttled",
                &[("retry_after_secs", retry_after_secs.to_string())],
            ),
        }
    }

    /// 返回固定的错误类型代码字符串（不暴露内部枚举结构）
    fn error_code(&self) -> &'static str {
        match self {
            NexusFlowError::Config { .. } => "config_error",
            NexusFlowError::Protocol { .. } => "protocol_error",
            NexusFlowError::VendorUnavailable { .. } => "vendor_unavailable",
            NexusFlowError::Timeout { .. } => "timeout",
            NexusFlowError::RateLimited { .. } => "rate_limited",
            NexusFlowError::Database { .. } => "database_error",
            NexusFlowError::Validation { .. } => "validation_error",
            NexusFlowError::Routing { .. } => "routing_error",
            NexusFlowError::AllVendorsThrottled { .. } => "all_vendors_throttled",
        }
    }
}

#[cfg(feature = "database")]
impl From<dbnexus::DbError> for NexusFlowError {
    fn from(e: dbnexus::DbError) -> Self {
        NexusFlowError::Database {
            message: t_args("error-database-source", &[("error", e.to_string())]),
        }
    }
}

impl From<std::io::Error> for NexusFlowError {
    fn from(e: std::io::Error) -> Self {
        NexusFlowError::Database {
            message: t_args("error-io-source", &[("error", e.to_string())]),
        }
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for NexusFlowError {
    fn from(_e: tokio::sync::oneshot::error::RecvError) -> Self {
        NexusFlowError::Timeout {
            vendor: "unknown".to_string(),
        }
    }
}

#[cfg(feature = "sdforge-integration")]
impl From<sdforge::config::ConfigError> for NexusFlowError {
    fn from(e: sdforge::config::ConfigError) -> Self {
        NexusFlowError::Config {
            message: e.to_string(),
        }
    }
}

#[cfg(all(test, feature = "web-server"))]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    /// 从 Response 中提取 JSON body
    async fn extract_body(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("failed to read body");
        serde_json::from_slice(&bytes).expect("body is not valid JSON")
    }

    #[tokio::test]
    async fn test_error_validation_to_response() {
        let response = NexusFlowError::Validation {
            field: "model".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_error_vendor_unavailable_to_response() {
        let response = NexusFlowError::VendorUnavailable {
            vendor: "openai".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_error_timeout_to_response() {
        let response = NexusFlowError::Timeout {
            vendor: "openai".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn test_error_rate_limited_to_response() {
        let response = NexusFlowError::RateLimited {
            vendor: "anthropic".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_error_config_to_response() {
        let response = NexusFlowError::Config {
            message: "bad config".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_protocol_to_response() {
        let response = NexusFlowError::Protocol {
            source: "openai".to_string(),
            inner: None,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn test_error_database_to_response() {
        let response = NexusFlowError::Database {
            message: "connection lost".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_routing_to_response() {
        let response = NexusFlowError::Routing {
            message: "no vendor".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_all_vendors_throttled_to_response_with_retry_after() {
        // recId=19(c)：AllVendorsThrottled → 429 + Retry-After 头（秒数
        // 原样带出）+ 固定错误代码
        let response = NexusFlowError::AllVendorsThrottled {
            retry_after_secs: 30,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .expect("Retry-After header must be present")
            .to_str()
            .expect("Retry-After must be visible ASCII");
        assert_eq!(retry_after, "30");

        let body = extract_body(response).await;
        assert_eq!(body["error"]["type"], "all_vendors_throttled");
        assert!(
            body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("30"),
            "message 应携带重试秒数"
        );
    }

    #[test]
    fn test_user_message_uses_ftl_catalog() {
        // 键必须在 FTL 目录注册（缺键时 t_args 回退返回键名本身）；
        // 占位符必须被实参替换。断言与进程 locale 无关（en/zh 均成立）。
        let message = NexusFlowError::Config {
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

        let throttled = NexusFlowError::AllVendorsThrottled {
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

    #[tokio::test]
    async fn test_error_response_body_structure() {
        let err = NexusFlowError::Validation {
            field: "model".to_string(),
        };
        // error.message 走本地化轨道：必须等于 user_message() 的输出
        let expected_message = err.user_message();
        let response = err.into_response();
        let body = extract_body(response).await;

        // 顶层必须包含 error 对象
        assert!(
            body["error"].is_object(),
            "response body must contain an 'error' object"
        );
        assert_eq!(body["error"]["message"], expected_message);
        // error.type 必须是固定的错误代码字符串（不暴露内部枚举结构）
        let type_str = body["error"]["type"]
            .as_str()
            .expect("error.type must be a string");
        assert_eq!(
            type_str, "validation_error",
            "error.type should be the fixed error code, got: {}",
            type_str
        );
    }

    #[tokio::test]
    async fn test_error_from_dbnexus_error() {
        let db_err = dbnexus::DbError::Config("table missing".to_string());
        let rendered = db_err.to_string();
        let err: NexusFlowError = db_err.into();
        match err {
            NexusFlowError::Database { message } => {
                assert!(
                    message.contains("table missing"),
                    "message should contain the original error text: {}",
                    message
                );
                // 前缀经 FTL 模板（随 locale），与直接查表结果一致
                assert_eq!(
                    message,
                    t_args("error-database-source", &[("error", rendered)]),
                    "message must be the localized 'error-database-source' template"
                );
            }
            _ => panic!("From<DbError> should produce Database variant"),
        }
    }

    #[tokio::test]
    async fn test_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let rendered = io_err.to_string();
        let err: NexusFlowError = io_err.into();
        match err {
            NexusFlowError::Database { message } => {
                assert!(
                    message.contains("file gone"),
                    "message should contain the original error text: {}",
                    message
                );
                // 前缀经 FTL 模板（随 locale），与直接查表结果一致
                assert_eq!(
                    message,
                    t_args("error-io-source", &[("error", rendered)]),
                    "message must be the localized 'error-io-source' template"
                );
            }
            _ => panic!("From<io::Error> should produce Database variant"),
        }
    }

    #[tokio::test]
    async fn test_error_from_recv_error() {
        // 构造 RecvError：发送端被丢弃后，接收端 await 会返回 RecvError
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);
        let recv_err = rx.await.unwrap_err();

        let err: NexusFlowError = recv_err.into();
        match err {
            NexusFlowError::Timeout { vendor } => {
                assert_eq!(
                    vendor, "unknown",
                    "From<RecvError> should set vendor to 'unknown'"
                );
            }
            _ => panic!("From<RecvError> should produce Timeout variant"),
        }
    }

    #[tokio::test]
    #[cfg(feature = "sdforge-integration")]
    async fn test_error_from_config_error() {
        let cfg_err = sdforge::config::ConfigError::FileNotFound {
            path: "/etc/config.toml".to_string(),
        };
        let err: NexusFlowError = cfg_err.into();
        match err {
            NexusFlowError::Config { message } => {
                assert!(
                    message.contains("/etc/config.toml"),
                    "message should contain the original path: {}",
                    message
                );
                assert!(
                    message.contains("File not found"),
                    "message should contain the ConfigError text: {}",
                    message
                );
            }
            _ => panic!("From<ConfigError> should produce Config variant"),
        }
    }
}
