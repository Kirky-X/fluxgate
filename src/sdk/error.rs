// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK 统一错误类型。
//!
//! - `reqwest::Error` 手写分流：`is_timeout()` → [`FluxgateSdkError::Timeout`]，
//!   其余 → [`FluxgateSdkError::Network`]（`#[from]` 只能标注一个变体，手写
//!   实现保证既有 `?` 调用点零改动且超时可判别）。
//! - HTTP 非 2xx 统一构造 [`ApiError`]：status/kind/message/code/param/
//!   request-id/retry-after/x-should-retry 全量可判别。
//! - 流内错误（SSE `type:"error"` 事件等）走 [`FluxgateSdkError::Stream`]，
//!   不构造 `ApiError`（无 HTTP status 语义）。

use std::time::Duration;

use reqwest::header::HeaderMap;
use thiserror::Error;

use crate::sdk::retry::parse_retry_after;

/// SDK 错误。
#[derive(Error, Debug)]
pub enum FluxgateSdkError {
    #[error("network error: {source}")]
    Network { source: reqwest::Error },

    #[error("request timed out: {source}")]
    Timeout { source: reqwest::Error },

    #[error("api error (status {}): {}", .0.status, .0.message)]
    Api(ApiError),

    #[error("protocol error: {message}")]
    Protocol { message: String },

    #[error("stream error: {message}")]
    Stream { message: String },

    #[error("config error: {message}")]
    Config { message: String },
}

impl From<reqwest::Error> for FluxgateSdkError {
    /// 传输层错误分流：超时 → `Timeout`，其余（连接/请求/body）→ `Network`。
    fn from(source: reqwest::Error) -> Self {
        if source.is_timeout() {
            Self::Timeout { source }
        } else {
            Self::Network { source }
        }
    }
}

impl FluxgateSdkError {
    /// 上游非 2xx → [`FluxgateSdkError::Api`]。
    ///
    /// `headers` 必须在消费 body 前获取（reqwest 响应头在 body 消费后仍可读，
    /// 但调用点约定先取头）；`body` 为已读出的原始文本。
    pub(crate) fn from_http_error(status: u16, headers: &HeaderMap, body: &str) -> Self {
        Self::Api(ApiError::from_http_parts(status, headers, body))
    }
}

/// HTTP API 错误（非 2xx）。
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub kind: ApiErrorKind,
    pub message: String,
    pub code: Option<String>,
    pub param: Option<String>,
    pub request_id: Option<String>,
    pub retry_after: Option<Duration>,
    /// `x-should-retry` 头原值。
    pub should_retry: Option<bool>,
}

/// API 错误分类（status 派生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    InvalidRequest,
    Authentication,
    Permission,
    NotFound,
    Conflict,
    PayloadTooLarge,
    Unprocessable,
    RateLimit,
    ServiceUnavailable,
    Overloaded,
    Server,
    Other,
}

impl ApiError {
    /// 可重试判定：`x-should-retry` 显式否决优先；否则
    /// `status ∈ {408,409,429} || status >= 500`。
    pub fn is_retryable(&self) -> bool {
        match self.should_retry {
            Some(false) => false,
            Some(true) => true,
            None => matches!(self.status, 408 | 409 | 429) || self.status >= 500,
        }
    }

    /// 从 HTTP 响应三要素（status / headers / body 文本）构造。
    ///
    /// header 元数据（request-id/retry-after/should-retry）优先解析；body
    /// 依次尝试 `{"error":{message,code,param}}`（含 `error` 为字符串的
    /// 兼容形态），非 JSON 回落原始文本（截断 512 字符防超长 HTML）。
    pub(crate) fn from_http_parts(status: u16, headers: &HeaderMap, body: &str) -> Self {
        let (message, code, param) = parse_error_body(body, status);
        Self {
            status,
            kind: kind_from_status(status),
            message,
            code,
            param,
            request_id: header_str(headers, "x-request-id")
                .or_else(|| header_str(headers, "anthropic-request-id")),
            retry_after: parse_retry_after(headers),
            should_retry: header_str(headers, "x-should-retry").and_then(|raw| {
                let raw = raw.trim();
                if raw.eq_ignore_ascii_case("true") {
                    Some(true)
                } else if raw.eq_ignore_ascii_case("false") {
                    Some(false)
                } else {
                    None
                }
            }),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "api error (status {}): {}", self.status, self.message)
    }
}

/// status → 分类（Anthropic 529 = overloaded 专属）。
fn kind_from_status(status: u16) -> ApiErrorKind {
    match status {
        400 => ApiErrorKind::InvalidRequest,
        401 => ApiErrorKind::Authentication,
        403 => ApiErrorKind::Permission,
        404 => ApiErrorKind::NotFound,
        409 => ApiErrorKind::Conflict,
        413 => ApiErrorKind::PayloadTooLarge,
        422 => ApiErrorKind::Unprocessable,
        429 => ApiErrorKind::RateLimit,
        503 => ApiErrorKind::ServiceUnavailable,
        529 => ApiErrorKind::Overloaded,
        500..=599 => ApiErrorKind::Server,
        _ => ApiErrorKind::Other,
    }
}

pub(crate) fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

/// 错误消息抽取上限（字符）：上游错误面不可信，超长 JSON message 截断，
/// 防止多 MB 文本随 `Display`/`Debug` 进入调用方日志。
pub(crate) const MAX_ERROR_MESSAGE_CHARS: usize = 512;

/// 解析错误体 → (message, code, param)。
fn parse_error_body(body: &str, status: u16) -> (String, Option<String>, Option<String>) {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .unwrap_or(&serde_json::Value::Null);
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| error.as_str());
    if let Some(message) = message {
        return (
            message.chars().take(MAX_ERROR_MESSAGE_CHARS).collect(),
            error
                .get("code")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            error
                .get("param")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        );
    }
    let raw = body.trim();
    if raw.is_empty() {
        (format!("http {status}"), None, None)
    } else {
        (
            raw.chars().take(MAX_ERROR_MESSAGE_CHARS).collect(),
            None,
            None,
        )
    }
}
