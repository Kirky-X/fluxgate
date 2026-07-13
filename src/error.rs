// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// 错误处理模块
use axum::response::IntoResponse;
use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NexusFlowError {
    #[error("配置错误: {message}")]
    Config { message: String },

    #[error("协议转换错误: {source}")]
    Protocol {
        source: String,
        #[source]
        inner: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("厂商不可用: {vendor}")]
    VendorUnavailable { vendor: String },

    #[error("请求超时: {vendor}")]
    Timeout { vendor: String },

    #[error("限流: {vendor}")]
    RateLimited { vendor: String },

    #[error("数据库错误: {message}")]
    Database { message: String },

    #[error("请求验证失败: {field}")]
    Validation { field: String },

    #[error("路由错误: {message}")]
    Routing { message: String },
}

impl IntoResponse for NexusFlowError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            NexusFlowError::Validation { .. } => axum::http::StatusCode::BAD_REQUEST,
            NexusFlowError::VendorUnavailable { .. } => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            NexusFlowError::Timeout { .. } => axum::http::StatusCode::GATEWAY_TIMEOUT,
            NexusFlowError::RateLimited { .. } => axum::http::StatusCode::TOO_MANY_REQUESTS,
            NexusFlowError::Config { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::Protocol { .. } => axum::http::StatusCode::BAD_GATEWAY,
            NexusFlowError::Database { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            NexusFlowError::Routing { .. } => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = json!({
            "error": {
                "message": self.to_string(),
                "type": format!("{:?}", self),
            }
        });

        (status, axum::Json(body)).into_response()
    }
}

impl From<dbnexus::DbError> for NexusFlowError {
    fn from(e: dbnexus::DbError) -> Self {
        NexusFlowError::Database {
            message: format!("Database error: {}", e),
        }
    }
}

impl From<std::io::Error> for NexusFlowError {
    fn from(e: std::io::Error) -> Self {
        NexusFlowError::Database {
            message: format!("IO error: {}", e),
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

impl From<sdforge::config::ConfigError> for NexusFlowError {
    fn from(e: sdforge::config::ConfigError) -> Self {
        NexusFlowError::Config {
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
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
    async fn test_error_response_body_structure() {
        let response = NexusFlowError::Validation {
            field: "model".to_string(),
        }
        .into_response();
        let body = extract_body(response).await;

        // 顶层必须包含 error 对象
        assert!(
            body["error"].is_object(),
            "response body must contain an 'error' object"
        );
        // error.message 必须是字符串，且等于 to_string() 的输出
        assert_eq!(body["error"]["message"], "请求验证失败: model");
        // error.type 必须是字符串，且包含变体名（Debug 格式）
        let type_str = body["error"]["type"]
            .as_str()
            .expect("error.type must be a string");
        assert!(
            type_str.contains("Validation"),
            "error.type should contain the variant name, got: {}",
            type_str
        );
    }

    #[tokio::test]
    async fn test_error_from_dbnexus_error() {
        let db_err = dbnexus::DbError::Config("table missing".to_string());
        let err: NexusFlowError = db_err.into();
        match err {
            NexusFlowError::Database { message } => {
                assert!(
                    message.contains("table missing"),
                    "message should contain the original error text: {}",
                    message
                );
                assert!(
                    message.contains("Database error"),
                    "message should be prefixed with 'Database error': {}",
                    message
                );
            }
            _ => panic!("From<DbError> should produce Database variant"),
        }
    }

    #[tokio::test]
    async fn test_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let err: NexusFlowError = io_err.into();
        match err {
            NexusFlowError::Database { message } => {
                assert!(
                    message.contains("file gone"),
                    "message should contain the original error text: {}",
                    message
                );
                assert!(
                    message.contains("IO error"),
                    "message should be prefixed with 'IO error': {}",
                    message
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
