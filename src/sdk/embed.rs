//! Embedding 请求/响应类型与客户端方法。

use serde::Deserialize;

use crate::sdk::client::NexusFlowClient;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::NexusFlowSdkError;

/// Embedding 请求。
#[derive(Debug, Clone)]
pub struct EmbedRequest {
    pub model: String,
    pub input: Vec<String>,
    pub config: RequestConfig,
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
    pub embedding: Vec<f32>,
}

/// Token 用量。
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

/// embedding 请求 wire 载荷：类型化单次序列化（同 [`ChatPayload`] 语义）。
#[derive(serde::Serialize)]
struct EmbedPayload<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(flatten)]
    params: crate::sdk::config::OpenAiParams,
}

impl NexusFlowClient {
    /// 发送 embedding 请求。POST /v1/embeddings。
    pub async fn embed(&self, req: &EmbedRequest) -> Result<EmbedResponse, NexusFlowSdkError> {
        let merged = self.default_config.merge(&req.config);

        let payload = EmbedPayload {
            model: &req.model,
            input: &req.input,
            params: merged.openai_params(),
        };

        let url = self
            .base_url
            .join("v1/embeddings")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let embed_resp: EmbedResponse = resp.json().await?;
        Ok(embed_resp)
    }
}
