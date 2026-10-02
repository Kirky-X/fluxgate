//! NexusFlow SDK 客户端 — HTTP 调用 /v1/chat/completions。

use std::time::Duration;

use serde_json::json;
use url::Url;

use crate::sdk::chat::{ChatRequest, ChatResponse};
use crate::sdk::config::RequestConfig;
use crate::sdk::error::NexusFlowSdkError;
use crate::sdk::stream::ChatStream;

/// NexusFlow 网关客户端。
pub struct NexusFlowClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Url,
    pub(crate) api_key: String,
    pub(crate) default_config: RequestConfig,
}

// api_key 是 Bearer 凭证：手写 Debug 脱敏，不用 derive（会打印明文）
impl std::fmt::Debug for NexusFlowClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NexusFlowClient")
            .field("http", &self.http)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("default_config", &self.default_config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// api_key 是 Bearer 凭证：Debug 输出必须脱敏，{:?} 打印客户端
    /// （日志/调试）不得泄露明文 key。
    #[test]
    fn debug_output_redacts_api_key() {
        let client = NexusFlowClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .api_key("super-secret-bearer-key")
            .build()
            .unwrap();
        let rendered = format!("{client:?}");
        assert!(
            !rendered.contains("super-secret-bearer-key"),
            "Debug 输出泄露 api_key: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "Debug 输出应显式标注脱敏: {rendered}"
        );
    }
}

/// 客户端构建器。
#[derive(Default)]
pub struct NexusFlowClientBuilder {
    base_url: Option<String>,
    api_key: Option<String>,
    default_config: RequestConfig,
    timeout: Option<Duration>,
}

impl NexusFlowClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn default_config(mut self, config: RequestConfig) -> Self {
        self.default_config = config;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn build(self) -> Result<NexusFlowClient, NexusFlowSdkError> {
        let url_str = self.base_url.ok_or_else(|| NexusFlowSdkError::Config {
            message: "base_url is required".into(),
        })?;
        let base_url = Url::parse(&url_str).map_err(|e| NexusFlowSdkError::Config {
            message: format!("invalid base_url: {e}"),
        })?;
        let api_key = self.api_key.ok_or_else(|| NexusFlowSdkError::Config {
            message: "api_key is required".into(),
        })?;

        let mut builder = reqwest::Client::builder();
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        let http = builder.build()?;

        Ok(NexusFlowClient {
            http,
            base_url,
            api_key,
            default_config: self.default_config,
        })
    }
}

impl NexusFlowClient {
    /// 发送 chat 请求。合并 `default_config` 与 `req.config`，POST /v1/chat/completions。
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, NexusFlowSdkError> {
        let merged = self.default_config.merge(&req.config);

        let mut body = merged.to_openai_json();
        body["model"] = json!(req.model);
        body["messages"] = json!(req.messages);

        let url =
            self.base_url
                .join("v1/chat/completions")
                .map_err(|e| NexusFlowSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let mut request = self.http.post(url).bearer_auth(&self.api_key).json(&body);

        if let Some(v) = &merged.vendor_hint {
            request = request.header("X-Vendor", v);
        }
        if let Some(m) = &merged.model_hint {
            request = request.header("X-Model", m);
        }

        let resp = request.send().await?;
        let status = resp.status();

        if status.as_u16() == 401 {
            return Err(NexusFlowSdkError::Auth {
                message: "unauthorized".into(),
            });
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let v: serde_json::Value = resp.json().await?;

        let choices = v["choices"]
            .as_array()
            .ok_or_else(|| NexusFlowSdkError::Protocol {
                message: "missing choices array".into(),
            })?;
        let choice = choices.first().ok_or_else(|| NexusFlowSdkError::Protocol {
            message: "empty choices array".into(),
        })?;

        Ok(ChatResponse {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            model: v["model"].as_str().unwrap_or_default().to_string(),
            content: choice["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32,
            vendor: v["vendor"].as_str().unwrap_or_default().to_string(),
            finish_reason: choice["finish_reason"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// 发送流式 chat 请求。强制 `stream = true`，POST /v1/chat/completions，返回 ChatStream。
    pub async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, NexusFlowSdkError> {
        let mut merged = self.default_config.merge(&req.config);
        merged.stream = true;

        let mut body = merged.to_openai_json();
        body["model"] = json!(req.model);
        body["messages"] = json!(req.messages);

        let url =
            self.base_url
                .join("v1/chat/completions")
                .map_err(|e| NexusFlowSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let mut request = self.http.post(url).bearer_auth(&self.api_key).json(&body);

        if let Some(v) = &merged.vendor_hint {
            request = request.header("X-Vendor", v);
        }
        if let Some(m) = &merged.model_hint {
            request = request.header("X-Model", m);
        }

        let resp = request.send().await?;
        let status = resp.status();

        if status.as_u16() == 401 {
            return Err(NexusFlowSdkError::Auth {
                message: "unauthorized".into(),
            });
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(ChatStream::from_bytes_stream(resp.bytes_stream()))
    }
}
