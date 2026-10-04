// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// 应用层错误处理模块 — Database 等应用面变体留存本 crate；路由核心
// 变体集（GateError）进 llm-gate（design.md D1.3），经 `Gate` 包装携带。
// `From<dbnexus::DbError>` 受孤儿规则约束必须与被实现的枚举同 crate，
// 这是 Database 变体不下沉 llm-gate 的结构性原因。
#[cfg(feature = "web-server")]
use axum::response::IntoResponse;
#[cfg(feature = "web-server")]
use serde_json::json;
use thiserror::Error;

use crate::i18n::t_args;

pub use llm_gate::error::GateError;

/// 错误双轨：`#[error]` Display 恒英文规范串（日志 / 错误链），HTTP 展示
/// 走 [`NexusFlowError::user_message()`]（FTL，随 locale，见 i18n 基线 §4）。
/// `Gate` 变体 `transparent` 透传 llm-gate 英文规范串。
#[derive(Error, Debug)]
pub enum NexusFlowError {
    #[error("Database error: {message}")]
    Database { message: String },

    #[error(transparent)]
    Gate(#[from] GateError),
}

#[cfg(feature = "web-server")]
impl IntoResponse for NexusFlowError {
    fn into_response(self) -> axum::response::Response {
        // recId=19(c)：AllVendorsThrottled 额外携带 Retry-After 响应头
        let retry_after_secs = match &self {
            NexusFlowError::Gate(e) => e.retry_after_secs(),
            NexusFlowError::Database { .. } => None,
        };
        let status = match &self {
            NexusFlowError::Database { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::Gate(e) => axum::http::StatusCode::from_u16(e.status_code())
                .expect("GateError::status_code must yield a valid HTTP status code"),
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
            NexusFlowError::Database { message } => {
                t_args("error-database", &[("message", message.clone())])
            }
            NexusFlowError::Gate(e) => e.user_message(),
        }
    }

    /// 返回固定的错误类型代码字符串（不暴露内部枚举结构）
    fn error_code(&self) -> &'static str {
        match self {
            NexusFlowError::Database { .. } => "database_error",
            NexusFlowError::Gate(e) => e.error_code(),
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

#[cfg(feature = "sdforge-integration")]
impl From<sdforge::config::ConfigError> for NexusFlowError {
    fn from(e: sdforge::config::ConfigError) -> Self {
        NexusFlowError::Gate(GateError::Config {
            message: e.to_string(),
        })
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
        let response = NexusFlowError::from(GateError::Validation {
            field: "model".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_error_vendor_unavailable_to_response() {
        let response = NexusFlowError::from(GateError::VendorUnavailable {
            vendor: "openai".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_error_timeout_to_response() {
        let response = NexusFlowError::from(GateError::Timeout {
            vendor: "openai".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn test_error_rate_limited_to_response() {
        let response = NexusFlowError::from(GateError::RateLimited {
            vendor: "anthropic".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_error_config_to_response() {
        let response = NexusFlowError::from(GateError::Config {
            message: "bad config".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_protocol_to_response() {
        let response = NexusFlowError::from(GateError::Protocol {
            source: "openai".to_string(),
            inner: None,
        })
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
        let response = NexusFlowError::from(GateError::Routing {
            message: "no vendor".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_all_vendors_throttled_to_response_with_retry_after() {
        // recId=19(c)：AllVendorsThrottled → 429 + Retry-After 头（秒数
        // 原样带出）+ 固定错误代码
        let response = NexusFlowError::from(GateError::AllVendorsThrottled {
            retry_after_secs: 30,
        })
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

    #[tokio::test]
    async fn test_error_response_body_structure() {
        let err = NexusFlowError::from(GateError::Validation {
            field: "model".to_string(),
        });
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
    #[cfg(feature = "sdforge-integration")]
    async fn test_error_from_config_error() {
        let cfg_err = sdforge::config::ConfigError::FileNotFound {
            path: "/etc/config.toml".to_string(),
        };
        let err: NexusFlowError = cfg_err.into();
        match err {
            NexusFlowError::Gate(GateError::Config { message }) => {
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
            _ => panic!("From<ConfigError> should produce Gate(Config) variant"),
        }
    }
}
