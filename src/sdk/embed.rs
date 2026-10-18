// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Embedding 请求/响应类型与客户端方法。

use serde::{Deserialize, Serialize};

use crate::sdk::client::FluxgateClient;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::transport::send_with_retry;

/// Embedding 输入（官方 `input` 四形状：单字符串 / 字符串数组 /
/// token 整数数组 / token 数组的数组）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbedInput {
    /// 单条字符串
    Text(String),
    /// 批量字符串
    Strings(Vec<String>),
    /// 单条 token 序列
    Tokens(Vec<u32>),
    /// 批量 token 序列
    NestedTokens(Vec<Vec<u32>>),
}

impl From<String> for EmbedInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for EmbedInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<String>> for EmbedInput {
    fn from(value: Vec<String>) -> Self {
        Self::Strings(value)
    }
}

impl From<Vec<u32>> for EmbedInput {
    fn from(value: Vec<u32>) -> Self {
        Self::Tokens(value)
    }
}

impl From<Vec<Vec<u32>>> for EmbedInput {
    fn from(value: Vec<Vec<u32>>) -> Self {
        Self::NestedTokens(value)
    }
}

/// 输出编码格式（官方词表：`float` / `base64`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedEncodingFormat {
    /// 默认：f32 数组
    Float,
    /// base64 字符串（响应 `embedding` 为原串，SDK 不代解码）
    Base64,
}

/// Embedding 请求。
#[derive(Debug, Clone)]
pub struct EmbedRequest {
    pub model: String,
    pub input: EmbedInput,
    /// 输出向量维度（模型支持时生效；官方 `dimensions`）
    pub dimensions: Option<u32>,
    /// 输出编码格式（默认 float；base64 时响应 embedding 为字符串）
    pub encoding_format: Option<EmbedEncodingFormat>,
    pub config: RequestConfig,
}

impl EmbedRequest {
    /// 便捷构造：模型 + 输入（`impl Into<EmbedInput>`：&str / String /
    /// Vec<String> / Vec<u32> / Vec<Vec<u32>>）+ 默认配置。
    pub fn new(model: impl Into<String>, input: impl Into<EmbedInput>) -> Self {
        Self {
            model: model.into(),
            input: input.into(),
            dimensions: None,
            encoding_format: None,
            config: RequestConfig::default(),
        }
    }
}

/// Embedding 响应。
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedResponse {
    pub model: String,
    pub data: Vec<EmbedData>,
    pub usage: EmbedUsage,
}

/// 单条 embedding 数据。
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedData {
    pub index: usize,
    pub embedding: EmbeddingValue,
}

/// 单条 embedding 向量：f32 数组（默认编码）或 base64 字符串
///（`encoding_format: base64`；untagged 判别）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingValue {
    /// `encoding_format: float`（网关默认）
    Floats(Vec<f32>),
    /// `encoding_format: base64`：原始 base64 串（SDK 不代解码，保真）
    Base64(String),
}

/// Token 用量。
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

/// embedding 请求 wire 载荷：类型化单次序列化（同 [`ChatPayload`] 语义）。
#[derive(Serialize)]
struct EmbedPayload<'a> {
    model: &'a str,
    input: &'a EmbedInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding_format: Option<EmbedEncodingFormat>,
    #[serde(flatten)]
    params: crate::sdk::config::OpenAiParams,
}

impl FluxgateClient {
    /// 发送 embedding 请求。POST /v1/embeddings。
    pub async fn embed(&self, req: &EmbedRequest) -> Result<EmbedResponse, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);

        let payload = EmbedPayload {
            model: &req.model,
            input: &req.input,
            dimensions: req.dimensions,
            encoding_format: req.encoding_format,
            params: merged.openai_params(),
        };

        let url = self
            .base_url
            .join("v1/embeddings")
            .map_err(|e| FluxgateSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            // 非流式总超时逐请求设置：请求级 config.timeout 覆盖客户端默认
            .timeout(merged.timeout.unwrap_or(self.effective_timeout));
        let request = self.apply_request_headers(request, &merged);
        let policy = self.retry_policy_for(&merged);
        let resp = send_with_retry(&request, &policy).await?;

        let embed_resp: EmbedResponse = resp.json().await?;
        Ok(embed_resp)
    }
}
