//! SDK 统一错误类型。
//!
//! 用 thiserror 派生，支持 `?` 操作符从 reqwest::Error 和 SystemTimeError 转换。

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NexusFlowSdkError {
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

    #[error("timeout: {source}")]
    Timeout {
        #[from]
        source: std::time::SystemTimeError,
    },

    #[error("stream error: {message}")]
    Stream { message: String },

    #[error("config error: {message}")]
    Config { message: String },
}
