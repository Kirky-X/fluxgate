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
use crate::sdk::stream::stream_error_message;
use crate::sdk::transport::send_with_retry;

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
    /// `include`（如 `["reasoning.encrypted_content"]`）
    pub include: Option<Vec<String>>,
    /// 单次响应最大工具调用数
    pub max_tool_calls: Option<u32>,
    /// 采样 top 候选 logprob 数（0-20）
    pub top_logprobs: Option<u32>,
    /// prompt 缓存键
    pub prompt_cache_key: Option<String>,
    /// prompt 缓存保留策略（原样透传）
    pub prompt_cache_retention: Option<String>,
    /// 安全标识（滥用追踪）
    pub safety_identifier: Option<String>,
    /// 服务层级（原样透传）
    pub service_tier: Option<String>,
    /// 流式附加配置（原样透传）
    pub stream_options: Option<Value>,
    /// 上下文管理配置（原样透传）
    pub context_management: Option<Value>,
    /// 内容审核配置（原样透传）
    pub moderation: Option<Value>,
    /// 访问程序配置（原样透传）
    pub access_programs: Option<Value>,
    /// 顶层扩展键（未知/实验参数；与 typed 字段冲突时 typed 优先）
    pub extra_body: BTreeMap<String, Value>,
    /// 请求级附加头（覆盖客户端 `default_headers` 同名键，大小写不敏感）
    pub extra_headers: BTreeMap<String, String>,
    pub metadata: Option<BTreeMap<String, String>>,
    pub background: bool,
    pub user: Option<String>,
    pub config: RequestConfig,
}

impl ResponsesRequest {
    /// 请求体（`stream` 由调用方法决定）。
    fn payload(&self, stream: bool) -> Value {
        // 顶层扩展先落：与 typed 字段同名时 typed 覆写（typed 优先）
        let mut body: Map<String, Value> = self
            .extra_body
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
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
        if let Some(include) = &self.include {
            body.insert("include".to_string(), json!(include));
        }
        if let Some(max_tool_calls) = self.max_tool_calls {
            body.insert("max_tool_calls".to_string(), json!(max_tool_calls));
        }
        if let Some(top_logprobs) = self.top_logprobs {
            body.insert("top_logprobs".to_string(), json!(top_logprobs));
        }
        if let Some(prompt_cache_key) = &self.prompt_cache_key {
            body.insert("prompt_cache_key".to_string(), json!(prompt_cache_key));
        }
        if let Some(prompt_cache_retention) = &self.prompt_cache_retention {
            body.insert(
                "prompt_cache_retention".to_string(),
                json!(prompt_cache_retention),
            );
        }
        if let Some(safety_identifier) = &self.safety_identifier {
            body.insert("safety_identifier".to_string(), json!(safety_identifier));
        }
        if let Some(service_tier) = &self.service_tier {
            body.insert("service_tier".to_string(), json!(service_tier));
        }
        if let Some(stream_options) = &self.stream_options {
            body.insert("stream_options".to_string(), stream_options.clone());
        }
        if let Some(context_management) = &self.context_management {
            body.insert("context_management".to_string(), context_management.clone());
        }
        if let Some(moderation) = &self.moderation {
            body.insert("moderation".to_string(), moderation.clone());
        }
        if let Some(access_programs) = &self.access_programs {
            body.insert("access_programs".to_string(), access_programs.clone());
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

/// `POST /v1/responses/input_tokens` 请求：输入 token 估算。网关要求
/// `model` 与 `input` 非空（SDK 侧先行校验，空值不发请求）。
#[derive(Debug, Clone, Default)]
pub struct ResponsesInputTokensRequest {
    pub model: String,
    /// 与 [`ResponsesRequest::input`] 同形状（字符串或 item 数组）
    pub input: Value,
}

impl ResponsesInputTokensRequest {
    /// 便捷构造。
    pub fn new(model: impl Into<String>, input: Value) -> Self {
        Self {
            model: model.into(),
            input,
        }
    }
}

/// 输入 token 估算响应（`{"object":"response.input_tokens","input_tokens":N}`）。
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesInputTokensResponse {
    #[serde(default)]
    pub object: String,
    pub input_tokens: u64,
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
                    // `event: error` 帧：不按事件解析，直接上抛（对齐官方
                    // SDK `error` 事件语义）
                    if event.event == "error" {
                        return Some(Err(FluxgateSdkError::Stream {
                            message: stream_error_message(&event.data),
                        }));
                    }
                    match serde_json::from_str::<ResponsesStreamEvent>(&event.data) {
                        Ok(parsed) => {
                            // `type:"error"` 事件：流内事件无 HTTP status，
                            // 消息取 `message`；不得当作正常事件静默下发
                            if parsed.event_type == "error" {
                                return Some(Err(FluxgateSdkError::Stream {
                                    message: stream_error_message(&event.data),
                                }));
                            }
                            Some(Ok(parsed))
                        }
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
        let mut merged = self.default_config.merge(&req.config);
        // 请求级 extra_headers 覆盖客户端/配置默认（小写归一键）
        merged.extra_headers.extend(
            req.extra_headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.clone())),
        );
        let payload = req.payload(false);
        let url = self
            .base_url
            .join("v1/responses")
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(merged.timeout.unwrap_or(self.effective_timeout));
        let request = self.apply_request_headers(request, &merged);
        let policy = self.retry_policy_for(&merged);

        let response = send_with_retry(&request, &policy).await?;
        Ok(response.json().await?)
    }

    /// 流式 `POST /v1/responses`（语义化 SSE 事件）。
    pub async fn responses_stream(
        &self,
        req: &ResponsesRequest,
    ) -> Result<ResponsesStream, FluxgateSdkError> {
        let mut merged = self.default_config.merge(&req.config);
        merged.extra_headers.extend(
            req.extra_headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.clone())),
        );
        let payload = req.payload(true);
        let url = self
            .base_url
            .join("v1/responses")
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload);
        let request = self.apply_request_headers(request, &merged);
        let policy = self.retry_policy_for(&merged);

        // 仅响应头阶段可重试：bytes_stream 建立后不再重试
        let response = send_with_retry(&request, &policy).await?;
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
        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let response = send_with_retry(&request, &self.retry_policy).await?;
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
        let request = self
            .http
            .delete(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let response = send_with_retry(&request, &self.retry_policy).await?;
        Ok(response.json().await?)
    }

    /// `POST /v1/responses/{id}/cancel`：仅后台任务可取消（网关对非
    /// background 响应返回 400），返回 cancelled 响应对象。
    pub async fn cancel_response(&self, id: &str) -> Result<ResponsesResponse, FluxgateSdkError> {
        let url = self
            .base_url
            .join(&format!("v1/responses/{id}/cancel"))
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let response = send_with_retry(&request, &self.retry_policy).await?;
        Ok(response.json().await?)
    }

    /// `POST /v1/responses/input_tokens`：输入 token 估算（网关近似
    /// 估算）。`model` 与 `input` 为空时返回 Protocol 错误，不发请求。
    pub async fn input_tokens(
        &self,
        req: &ResponsesInputTokensRequest,
    ) -> Result<ResponsesInputTokensResponse, FluxgateSdkError> {
        if req.model.trim().is_empty() {
            return Err(FluxgateSdkError::Protocol {
                message: "input_tokens requires non-empty `model`".to_string(),
            });
        }
        let input_empty = match &req.input {
            Value::String(text) => text.trim().is_empty(),
            Value::Array(items) => items.is_empty(),
            Value::Null => true,
            _ => false,
        };
        if input_empty {
            return Err(FluxgateSdkError::Protocol {
                message: "input_tokens requires non-empty `input`".to_string(),
            });
        }

        let url = self
            .base_url
            .join("v1/responses/input_tokens")
            .map_err(|error| FluxgateSdkError::Config {
                message: format!("invalid URL: {error}"),
            })?;
        let payload = json!({"model": req.model, "input": req.input});
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let response = send_with_retry(&request, &self.retry_policy).await?;
        Ok(response.json().await?)
    }
}
