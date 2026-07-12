// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// 错误处理模块
use axum::response::IntoResponse;
use serde_json::json;
use std::result;
use thiserror::Error;

#[allow(dead_code)]
pub type Result<T> = result::Result<T, NexusFlowError>;

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

    #[error("所有厂商均不可用")]
    #[allow(dead_code)]
    AllVendorsUnavailable,

    #[error("请求超时: {vendor}")]
    Timeout { vendor: String },

    #[error("限流: {vendor}")]
    RateLimited { vendor: String },

    #[error("数据库错误: {message}")]
    Database { message: String },

    #[error("请求验证失败: {field}")]
    Validation { field: String },

    #[error("路由错误: {message}")]
    #[allow(dead_code)]
    Routing { message: String },
}

impl NexusFlowError {
    /// 检查错误是否可重试
    #[allow(dead_code)]
    pub fn is_retryable(&self) -> bool {
        match self {
            NexusFlowError::Timeout { .. } => true,
            NexusFlowError::RateLimited { .. } => true,
            NexusFlowError::VendorUnavailable { .. } => false,
            NexusFlowError::Protocol { .. } => true,
            _ => false,
        }
    }
}

impl IntoResponse for NexusFlowError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            NexusFlowError::Validation { .. } => axum::http::StatusCode::BAD_REQUEST,
            NexusFlowError::VendorUnavailable { .. } => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            NexusFlowError::AllVendorsUnavailable { .. } => {
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
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

impl From<sqlx::Error> for NexusFlowError {
    fn from(e: sqlx::Error) -> Self {
        NexusFlowError::Database {
            message: format!("SQLite error: {}", e),
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
