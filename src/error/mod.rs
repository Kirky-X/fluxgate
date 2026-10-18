// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

// 应用层错误处理：Database 等应用面变体定义在本模块，路由核心变体集（GateError）
// 在子模块 gate 并经 `Gate` 包装携带（`From<dbnexus::DbError>` 受孤儿规则约束须同 crate）。
#[cfg(feature = "web-server")]
use axum::response::IntoResponse;
#[cfg(feature = "web-server")]
use serde_json::json;
use thiserror::Error;

use crate::i18n::t_args;

mod gate;

pub use gate::GateError;

/// 错误双轨：`#[error]` Display 恒英文规范串（日志/错误链），HTTP 展示走 `user_message()`
///（FTL，随 locale）；`Gate` 变体 `transparent` 透传英文规范串。
#[derive(Error, Debug)]
pub enum FluxgateError {
    #[error("Database error: {message}")]
    Database { message: String },

    #[error(transparent)]
    Gate(#[from] GateError),

    /// 密钥保密柜（key-vault）：Display 走 VaultError 英文规范串。
    #[cfg(feature = "vault")]
    #[error(transparent)]
    Vault(#[from] crate::vault::VaultError),
}

#[cfg(feature = "web-server")]
impl IntoResponse for FluxgateError {
    fn into_response(self) -> axum::response::Response {
        // AllVendorsThrottled 额外携带 Retry-After 响应头
        let retry_after_secs = match &self {
            FluxgateError::Gate(e) => e.retry_after_secs(),
            FluxgateError::Database { .. } => None,
            #[cfg(feature = "vault")]
            FluxgateError::Vault(_) => None,
        };
        let status = match &self {
            FluxgateError::Database { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            FluxgateError::Gate(e) => axum::http::StatusCode::from_u16(e.status_code())
                .expect("GateError::status_code must yield a valid HTTP status code"),
            #[cfg(feature = "vault")]
            FluxgateError::Vault(e) => match e {
                crate::vault::VaultError::NotFound(_) => axum::http::StatusCode::NOT_FOUND,
                crate::vault::VaultError::UnknownVendor(_) => axum::http::StatusCode::CONFLICT,
                _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            },
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
impl FluxgateError {
    /// 本地化用户消息（展示轨道）：随 locale 查 FTL 目录。
    pub(crate) fn user_message(&self) -> String {
        match self {
            FluxgateError::Database { message } => {
                t_args("error-database", &[("message", message.clone())])
            }
            FluxgateError::Gate(e) => e.user_message(),
            #[cfg(feature = "vault")]
            FluxgateError::Vault(e) => match e {
                crate::vault::VaultError::NotFound(id) => {
                    t_args("error-vault-not-found", &[("id", id.clone())])
                }
                crate::vault::VaultError::UnknownVendor(vendor) => {
                    t_args("error-vault-unknown-vendor", &[("vendor", vendor.clone())])
                }
                other => t_args("error-vault", &[("message", other.to_string())]),
            },
        }
    }

    /// 固定的错误类型代码字符串（不暴露内部枚举结构）
    fn error_code(&self) -> &'static str {
        match self {
            FluxgateError::Database { .. } => "database_error",
            FluxgateError::Gate(e) => e.error_code(),
            #[cfg(feature = "vault")]
            FluxgateError::Vault(_) => "vault_error",
        }
    }
}

#[cfg(feature = "database")]
impl From<dbnexus::DbError> for FluxgateError {
    fn from(e: dbnexus::DbError) -> Self {
        FluxgateError::Database {
            message: t_args("error-database-source", &[("error", e.to_string())]),
        }
    }
}

impl From<std::io::Error> for FluxgateError {
    fn from(e: std::io::Error) -> Self {
        FluxgateError::Database {
            message: t_args("error-io-source", &[("error", e.to_string())]),
        }
    }
}

#[cfg(feature = "sdforge-integration")]
impl From<sdforge::config::ConfigError> for FluxgateError {
    fn from(e: sdforge::config::ConfigError) -> Self {
        FluxgateError::Gate(GateError::Config {
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

    async fn extract_body(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("failed to read body");
        serde_json::from_slice(&bytes).expect("body is not valid JSON")
    }

    #[tokio::test]
    async fn test_error_validation_to_response() {
        let response = FluxgateError::from(GateError::Validation {
            field: "model".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_error_vendor_unavailable_to_response() {
        let response = FluxgateError::from(GateError::VendorUnavailable {
            vendor: "openai".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_error_timeout_to_response() {
        let response = FluxgateError::from(GateError::Timeout {
            vendor: "openai".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn test_error_rate_limited_to_response() {
        let response = FluxgateError::from(GateError::RateLimited {
            vendor: "anthropic".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_error_config_to_response() {
        let response = FluxgateError::from(GateError::Config {
            message: "bad config".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_protocol_to_response() {
        let response = FluxgateError::from(GateError::Protocol {
            source: "openai".to_string(),
            inner: None,
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn test_error_database_to_response() {
        let response = FluxgateError::Database {
            message: "connection lost".to_string(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_routing_to_response() {
        let response = FluxgateError::from(GateError::Routing {
            message: "no vendor".to_string(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_error_all_vendors_throttled_to_response_with_retry_after() {
        // AllVendorsThrottled → 429 + Retry-After 头：秒数原样带出 + 固定错误代码
        let response = FluxgateError::from(GateError::AllVendorsThrottled {
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
        let err = FluxgateError::from(GateError::Validation {
            field: "model".to_string(),
        });
        // error.message 走本地化轨道：必须等于 user_message() 的输出
        let expected_message = err.user_message();
        let response = err.into_response();
        let body = extract_body(response).await;

        assert!(
            body["error"].is_object(),
            "response body must contain an 'error' object"
        );
        assert_eq!(body["error"]["message"], expected_message);
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
        let err: FluxgateError = db_err.into();
        match err {
            FluxgateError::Database { message } => {
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
        let err: FluxgateError = io_err.into();
        match err {
            FluxgateError::Database { message } => {
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
        let err: FluxgateError = cfg_err.into();
        match err {
            FluxgateError::Gate(GateError::Config { message }) => {
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
