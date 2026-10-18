// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OpenAI Responses API 客户端：类型、非流式/流式调用与检索删除。

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::{BoxStream, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::sdk::client::FluxgateClient;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;

/// Responses 请求（`input` 为字符串或 item 数组，原样透传）。
#[derive(Debug, Clone, Default)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Value,
    pub instructions: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub tools: Option<Vec<Value>>,
    pub tool_choice: Option<Value>,
    /// `text.format` 结构化输出（原样透传）
    pub text: Option<Value>,
    /// `reasoning` 配置（`{effort}`）
    pub reasoning: Option<Value>,
    pub previous_response_id: Option<String>,
    pub conversation: Option<String>,
    pub store: Option<bool>,
    pub parallel_tool_calls: Option<bool>,
    pub truncation: Option<String>,
    pub metadata: Option<BTreeMap<String, String>>,
    pub background: bool,
    pub user: Option<String>,
    pub config: RequestConfig,
}

impl ResponsesRequest {
    /// 请求体（`stream` 由调用方法决定）。
    fn payload(&self, stream: bool) -> Value {
        let mut body = Map::new();
        body.insert("model".to_string(), json!(self.model));
        body.insert("input".to_string(), self.input.clone());
        if let Some(instructions) = &self.instructions {
            body.insert("instructions".to_string(), json!(instructions));
        }
        if let Some(max) = self.max_output_tokens {
            body.insert("max_output_tokens".to_string(), json!(max));
        }
        if let Some(temperature) = self.temperature {
            body.insert("temperature".to_string(), json!(temperature));
        }
        if let Some(top_p) = self.top_p {
            body.insert("top_p".to_string(), json!(top_p));
        }
        if let Some(tools) = &self.tools {
            body.insert("tools".to_string(), json!(tools));
        }
        if let Some(tool_choice) = &self.tool_choice {
            body.insert("tool_choice".to_string(), tool_choice.clone());
        }
        if let Some(text) = &self.text {
            body.insert("text".to_string(), text.clone());
        }
        if let Some(reasoning) = &self.reasoning {
            body.insert("reasoning".to_string(), reasoning.clone());
        }
        if let Some(previous) = &self.previous_response_id {
            body.insert("previous_response_id".to_string(), json!(previous));
        }
        if let Some(conversation) = &self.conversation {
            body.insert("conversation".to_string(), json!(conversation));
        }
        if let Some(store) = self.store {
            body.insert("store".to_string(), json!(store));
        }
        if let Some(parallel) = self.parallel_tool_calls {
            body.insert("parallel_tool_calls".to_string(), json!(parallel));
        }
        if let Some(truncation) = &self.truncation {
            body.insert("truncation".to_string(), json!(truncation));
        }
        if let Some(metadata) = &self.metadata {
            body.insert("metadata".to_string(), json!(metadata));
        }
        if self.background {
            body.insert("background".to_string(), json!(true));
        }
        if let Some(user) = &self.user {
            body.insert("user".to_string(), json!(user));
        }
        if stream {
            body.insert("stream".to_string(), json!(true));
        }
        Value::Object(body)
    }
}

/// Responses 响应对象。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesResponse {
    pub id: String,
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    pub usage: ResponsesUsage,
    #[serde(default)]
    pub error: Option<ResponsesError>,
    #[serde(default)]
    pub incomplete_details: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 错误对象。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesError {
    #[serde(default)]
    pub code: Option<String>,
    pub message: String,
}

/// output item（message / function_call / 其他）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputItem {
    Message {
        id: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        content: Vec<ResponsesContent>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        #[serde(default)]
        arguments: String,
    },
    #[serde(other)]
    Unknown,
}

/// message content（`output_text`）。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub annotations: Vec<Value>,
}

/// usage。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default)]
    pub input_tokens_details: Option<ResponsesInputTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesInputTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

/// 流式事件（`type` 必填，其余按需）。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub delta: Option<String>,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub output_index: Option<u32>,
    #[serde(default)]
    pub response: Option<ResponsesResponse>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Responses 事件流。
pub struct ResponsesStream {
    inner: BoxStream<'static, Result<ResponsesStreamEvent, FluxgateSdkError>>,
}

impl ResponsesStream {
    fn from_bytes_stream<S>(bytes: S) -> Self
    where
        S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    {
        use eventsource_stream::Eventsource;

        let stream = bytes.eventsource().filter_map(|event| async move {
            match event {
                Ok(event) => {
                    if event.data == "[DONE]" {
                        return None;
                    }
                    match serde_json::from_str::<ResponsesStreamEvent>(&event.data) {
                        Ok(parsed) => Some(Ok(parsed)),
                        Err(error) => Some(Err(FluxgateSdkError::Stream {
                            message: format!("JSON parse error: {error}"),
                        })),
                    }
                }
                Err(error) => Some(Err(FluxgateSdkError::Stream {
                    message: format!("SSE decode error: {error}"),
                })),
            }
        });
        Self {
            inner: stream.boxed(),
        }
    }
}

impl Stream for ResponsesStream {
    type Item = Result<ResponsesStreamEvent, FluxgateSdkError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl FluxgateClient {
    /// 非流式 `POST /v1/responses`。
    pub async fn responses(
        &self,
        req: &ResponsesRequest,
    ) -> Result<ResponsesResponse, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);
        let payload = req.payload(false);
        let url = self
            .base_url
            .join("v1/responses")
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(merged.timeout.unwrap_or(self.effective_timeout));
        if let Some(vendor) = &merged.vendor_hint {
            request = request.header("X-Vendor", vendor);
        }
        if let Some(model) = &merged.model_hint {
            request = request.header("X-Model", model);
        }

        let response = request.send().await?;
        let status = response.status();
        if status.as_u16() == 401 {
            return Err(FluxgateSdkError::Auth {
                message: "unauthorized".into(),
            });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(response.json().await?)
    }

    /// 流式 `POST /v1/responses`（语义化 SSE 事件）。
    pub async fn responses_stream(
        &self,
        req: &ResponsesRequest,
    ) -> Result<ResponsesStream, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);
        let payload = req.payload(true);
        let url = self
            .base_url
            .join("v1/responses")
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload);
        if let Some(vendor) = &merged.vendor_hint {
            request = request.header("X-Vendor", vendor);
        }
        if let Some(model) = &merged.model_hint {
            request = request.header("X-Model", model);
        }

        let response = request.send().await?;
        let status = response.status();
        if status.as_u16() == 401 {
            return Err(FluxgateSdkError::Auth {
                message: "unauthorized".into(),
            });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(ResponsesStream::from_bytes_stream(response.bytes_stream()))
    }

    /// `GET /v1/responses/{id}`。
    pub async fn get_response(&self, id: &str) -> Result<ResponsesResponse, FluxgateSdkError> {
        let url = self
            .base_url
            .join(&format!("v1/responses/{id}"))
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(response.json().await?)
    }

    /// `DELETE /v1/responses/{id}`。
    pub async fn delete_response(&self, id: &str) -> Result<Value, FluxgateSdkError> {
        let url = self
            .base_url
            .join(&format!("v1/responses/{id}"))
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let response = self
            .http
            .delete(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        Ok(response.json().await?)
    }
}
