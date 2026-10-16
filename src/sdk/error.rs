//! SDK 统一错误类型。
//!
//! 用 thiserror 派生，支持 `?` 操作符从 reqwest::Error 转换
//! （请求超时同走 reqwest::Error 的 Network 变体）。

use thiserror::Error;

#[derive(Error, Debug)]
pub enum FluxgateSdkError {
    #[error("network error: {source}")]
    Network {
        #[from]
        source: reqwest::Error,
    },

    #[error("authentication failed: {message}")]
    Auth { message: String },

    #[error("server error (status {status}): {message}")]
    Server { status: u16, message: String },

    #[error("protocol error: {message}")]
    Protocol { message: String },

    #[error("stream error: {message}")]
    Stream { message: String },

    #[error("config error: {message}")]
    Config { message: String },
}
