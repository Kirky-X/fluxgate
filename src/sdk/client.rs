// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Fluxgate SDK 客户端 — HTTP 调用 /v1/chat/completions。

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use url::Url;

use crate::sdk::chat::{ChatRequest, ChatResponse, RESERVED_KEYS};
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::retry::RetryPolicy;
use crate::sdk::stream::ChatStream;
use crate::sdk::transport::send_with_retry;

/// 非流式请求（chat / embed）的默认总超时（对齐官方 SDK 600s）。reqwest
/// 默认无总超时，挂起的网关会让调用方任务无限悬挂。不作用于
/// [`FluxgateClient::chat_stream`]：流式不设总超时（reqwest 总超时按整个
/// body 计且不按 chunk 重置，SSE 长流会被中途切断），由
/// [`DEFAULT_READ_TIMEOUT`] 看护。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// 连接建立默认超时（对齐官方 SDK connect 5s）：DNS/TCP/TLS 阶段卡死
/// 时不拖到总超时。
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 流读取超时默认值（对齐官方 SDK 600s）：作用于所有请求的每次读取，
/// 成功读取后重置——只拦截「连接停滞」（连续 600s 无字节），不限制长流
/// 的总时长。
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// 计算生效超时：显式值优先，否则落到 [`DEFAULT_REQUEST_TIMEOUT`]。
fn effective_timeout(explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or(DEFAULT_REQUEST_TIMEOUT)
}

/// 计算生效连接超时：显式值优先，否则落到 [`DEFAULT_CONNECT_TIMEOUT`]。
fn effective_connect_timeout(explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or(DEFAULT_CONNECT_TIMEOUT)
}

/// 计算生效读超时：显式值优先，否则落到 [`DEFAULT_READ_TIMEOUT`]。
fn effective_read_timeout(explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or(DEFAULT_READ_TIMEOUT)
}

/// 扩展头不得覆写的受限头名（比较忽略大小写）：凭证由 `bearer_auth`
/// 持有，`host`/`content-length` 由传输层决定——配置面覆写即请求伪造面；
/// 5 个路由头由 typed 字段作唯一来源（`header()` 为追加语义，扩展头注入
/// 会以先插入者压过 typed 值）。
const RESTRICTED_HEADERS: &[&str] = &[
    "authorization",
    "host",
    "content-length",
    "x-vendor",
    "x-model",
    "x-session-id",
    "x-batch",
    "x-weight",
];

/// `max_retries` 允许的上限：重试是调用方自伤面，超限值静默夹紧并记
/// debug（`u32::MAX` 配合 120s `Retry-After` 等于无限挂起）。
const MAX_MAX_RETRIES: u32 = 16;

/// Fluxgate 网关客户端。
pub struct FluxgateClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Url,
    pub(crate) api_key: String,
    /// 客户端级默认头（所有请求路径；请求级 `extra_headers` 覆盖同名键）
    pub(crate) default_headers: BTreeMap<String, String>,
    pub(crate) default_config: RequestConfig,
    /// 非流式请求的默认总超时（chat_stream 不适用；实际执行逐请求设置）
    pub(crate) effective_timeout: Duration,
    /// 连接建立超时（reqwest client 级 connect_timeout）
    pub(crate) connect_timeout: Duration,
    /// 流读取超时（reqwest client 级 read_timeout）
    pub(crate) read_timeout: Duration,
    /// 重试策略（429/5xx 与传输错误；builder `max_retries` 调整）
    pub(crate) retry_policy: RetryPolicy,
}

// api_key 是 Bearer 凭证：手写 Debug 脱敏，不用 derive（会打印明文）
impl std::fmt::Debug for FluxgateClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FluxgateClient")
            .field("http", &self.http)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            // 头值可能含凭证（如 x-api-key）：Debug 只列键名
            .field(
                "default_headers",
                &self.default_headers.keys().collect::<Vec<_>>(),
            )
            .field("effective_timeout", &self.effective_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("max_retries", &self.retry_policy.max_retries)
            .field("default_config", &self.default_config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未显式设置 timeout 时必须有默认超时：reqwest 默认无总超时，
    /// 挂起的网关会让调用方任务无限悬挂。
    #[test]
    fn default_timeout_applied_when_unset() {
        assert_eq!(
            effective_timeout(None),
            DEFAULT_REQUEST_TIMEOUT,
            "未设置 timeout 必须落到默认值"
        );
    }

    #[test]
    fn explicit_timeout_wins_over_default() {
        let explicit = Duration::from_millis(250);
        assert_eq!(effective_timeout(Some(explicit)), explicit);
    }

    /// 官方 SDK 对齐钉：总 600s / connect 5s / read 600s。
    #[test]
    fn official_sdk_timeout_defaults() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(600));
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(5));
        assert_eq!(DEFAULT_READ_TIMEOUT, Duration::from_secs(600));
    }

    /// 超时必须真的进 reqwest Client：默认构造 → DEFAULT_REQUEST_TIMEOUT，
    /// 显式 timeout() → 该值（客户端存储的生效超时可审计）。
    #[test]
    fn builder_applies_effective_timeout_to_client() {
        let default_client = FluxgateClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .api_key("k")
            .build()
            .unwrap();
        assert_eq!(default_client.effective_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(default_client.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(default_client.read_timeout, DEFAULT_READ_TIMEOUT);

        let tuned = FluxgateClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .api_key("k")
            .connect_timeout(Duration::from_millis(300))
            .read_timeout(Duration::from_millis(400))
            .build()
            .unwrap();
        assert_eq!(tuned.connect_timeout, Duration::from_millis(300));
        assert_eq!(tuned.read_timeout, Duration::from_millis(400));

        let explicit = Duration::from_millis(250);
        let tuned_client = FluxgateClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .api_key("k")
            .timeout(explicit)
            .build()
            .unwrap();
        assert_eq!(tuned_client.effective_timeout, explicit);
    }

    /// chat wire 载荷：类型化单次序列化，model/messages 直入顶层，
    /// 未设配置字段省略（无 null、无 Value 中间物二次序列化）。
    #[test]
    fn chat_payload_serializes_without_null() {
        use crate::sdk::chat::{ChatMessage, ChatRole};
        let payload = ChatPayload {
            model: "gpt-4",
            messages: &[ChatMessage {
                role: ChatRole::User,
                content: "hi".into(),
                tool_call_id: None,
                tool_calls: None,
            }],
            params: RequestConfig::default().openai_params(),
            extras: WireExtras {
                tools: &Vec::new(),
                tool_choice: &None,
                vendor_extensions: &std::collections::BTreeMap::new(),
            },
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["model"], "gpt-4");
        assert_eq!(v["messages"][0]["content"], "hi");
        assert_eq!(v["stream"], false);
        assert!(v.get("temperature").is_none());
        assert!(v.get("tools").is_none(), "空工具面省略");
        let rendered = v.to_string();
        assert!(
            !rendered.contains("null"),
            "wire body 不得含 null: {rendered}"
        );
    }

    /// api_key 是 Bearer 凭证：Debug 输出必须脱敏，{:?} 打印客户端
    /// （日志/调试）不得泄露明文 key。
    #[test]
    fn debug_output_redacts_api_key() {
        let client = FluxgateClientBuilder::new()
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
pub struct FluxgateClientBuilder {
    base_url: Option<String>,
    api_key: Option<String>,
    default_headers: BTreeMap<String, String>,
    default_config: RequestConfig,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    max_retries: Option<u32>,
}

impl FluxgateClientBuilder {
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

    /// 客户端级默认头（所有请求路径发送；请求级 `extra_headers` 覆盖
    /// 同名键，大小写不敏感；`authorization`/`host`/`content-length`
    /// 忽略并 debug）。
    pub fn default_headers(mut self, default_headers: BTreeMap<String, String>) -> Self {
        self.default_headers = default_headers;
        self
    }

    pub fn default_config(mut self, config: RequestConfig) -> Self {
        self.default_config = config;
        self
    }

    /// 非流式请求（chat / embed）的默认总超时；不作用于流式请求。
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 连接建立超时（默认 5s，官方 SDK 对齐）。
    pub fn connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = Some(connect_timeout);
        self
    }

    /// 流读取超时（按读重置，拦截连接停滞；默认 600s）。
    pub fn read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = Some(read_timeout);
        self
    }

    /// 重试次数上限（默认 2；`0` 关闭重试）。只对 429/5xx 与传输错误
    /// 生效，`x-should-retry`/`Retry-After` 提示优先。
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = Some(max_retries);
        self
    }

    pub fn build(self) -> Result<FluxgateClient, FluxgateSdkError> {
        let url_str = self.base_url.ok_or_else(|| FluxgateSdkError::Config {
            message: "base_url is required".into(),
        })?;
        let base_url = Url::parse(&url_str).map_err(|e| FluxgateSdkError::Config {
            message: format!("invalid base_url: {e}"),
        })?;
        let api_key = self.api_key.ok_or_else(|| FluxgateSdkError::Config {
            message: "api_key is required".into(),
        })?;

        let mut builder = reqwest::Client::builder();
        // client 级设 connect_timeout 与 read_timeout（按读重置，拦截连接
        // 停滞）：总超时若设在 client 级会按整个 body 计且不按 chunk 重置，
        // 把 SSE 长流在默认 600s 处切断。非流式请求的总超时逐请求设置。
        let connect = effective_connect_timeout(self.connect_timeout);
        let read = effective_read_timeout(self.read_timeout);
        builder = builder.connect_timeout(connect).read_timeout(read);
        let http = builder.build()?;
        let effective = effective_timeout(self.timeout);
        let mut retry_policy = RetryPolicy::default();
        if let Some(max_retries) = self.max_retries {
            retry_policy.max_retries = max_retries.min(MAX_MAX_RETRIES);
        }

        Ok(FluxgateClient {
            http,
            base_url,
            api_key,
            default_headers: self.default_headers,
            default_config: self.default_config,
            effective_timeout: effective,
            connect_timeout: connect,
            read_timeout: read,
            retry_policy,
        })
    }
}

/// chat 请求 wire 载荷：类型化单次序列化（不经 serde_json::Value 中间物，
/// 未设配置字段由 [`OpenAiParams`] 省略；工具面/vendor_extensions 由
/// [`WireExtras`] flatten 合并）。
#[derive(serde::Serialize)]
struct ChatPayload<'a> {
    model: &'a str,
    messages: &'a [crate::sdk::chat::ChatMessage],
    #[serde(flatten)]
    params: crate::sdk::config::OpenAiParams,
    #[serde(flatten)]
    extras: WireExtras<'a>,
}

/// 工具面 + vendor_extensions 的 wire 视图（tools 空省略；extensions
/// 顶层键经 flatten 合并进请求体）。
#[derive(serde::Serialize)]
struct WireExtras<'a> {
    #[serde(skip_serializing_if = "std::vec::Vec::is_empty")]
    tools: &'a Vec<crate::sdk::chat::ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: &'a Option<crate::sdk::chat::ChatToolChoice>,
    #[serde(flatten)]
    vendor_extensions: &'a std::collections::BTreeMap<String, serde_json::Value>,
}

/// 合并请求级 `extra_body` 与 `vendor_extensions`（后者优先）并做
/// [`RESERVED_KEYS`] 拒绝：扩展键不得覆写 typed 请求骨架（flatten 覆写面）。
fn merge_request_extensions(
    merged: &RequestConfig,
    request: &ChatRequest,
) -> Result<BTreeMap<String, serde_json::Value>, FluxgateSdkError> {
    for (source, extensions) in [
        ("extra_body", &merged.extra_body),
        ("vendor extension", &request.vendor_extensions),
    ] {
        if let Some(key) = extensions
            .keys()
            .find(|key| RESERVED_KEYS.contains(&key.as_str()))
        {
            return Err(FluxgateSdkError::Protocol {
                message: format!("{source} key `{key}` conflicts with a typed request field"),
            });
        }
    }
    let mut extensions = merged.extra_body.clone();
    extensions.extend(request.vendor_extensions.clone());
    Ok(extensions)
}

impl FluxgateClient {
    /// 应用客户端级默认头（所有请求路径共用；受限头忽略并 debug）。
    pub(crate) fn apply_default_headers(
        &self,
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        let headers: BTreeMap<String, String> = self
            .default_headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect();
        self.apply_header_map(request, &headers)
    }

    /// 统一请求头注入：客户端 `default_headers` → 请求级 `extra_headers`
    ///（覆盖同名，大小写不敏感）→ typed 路由头（X-Vendor / X-Model /
    /// X-Session-Id / X-Batch / X-Weight）。受限头（`authorization` /
    /// `host` / `content-length`）忽略并 debug。
    pub(crate) fn apply_request_headers(
        &self,
        request: reqwest::RequestBuilder,
        merged: &RequestConfig,
    ) -> reqwest::RequestBuilder {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (name, value) in &self.default_headers {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
        for (name, value) in &merged.extra_headers {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
        let mut request = self.apply_header_map(request, &headers);

        if let Some(vendor) = &merged.vendor_hint {
            request = request.header("X-Vendor", vendor);
        }
        if let Some(model) = &merged.model_hint {
            request = request.header("X-Model", model);
        }
        if let Some(session) = &merged.session_id {
            request = request.header("X-Session-Id", session);
        }
        if let Some(true) = merged.batch {
            // 网关口径：`X-Batch: 1` 为真（其余值/缺失为假）
            request = request.header("X-Batch", "1");
        }
        if let Some(weight) = merged.weight {
            request = request.header("X-Weight", weight.to_string());
        }
        request
    }

    /// 逐请求生效的重试策略：请求级 `max_retries` 覆盖客户端默认（夹紧到
    /// [`MAX_MAX_RETRIES`]），其余字段（退避曲线）沿用客户端策略。
    pub(crate) fn retry_policy_for(&self, merged: &RequestConfig) -> RetryPolicy {
        let mut policy = self.retry_policy.clone();
        if let Some(max_retries) = merged.max_retries {
            policy.max_retries = max_retries.min(MAX_MAX_RETRIES);
        }
        policy
    }

    /// 应用单一头映射（键需已小写）：受限头忽略并 debug，非法头名/值
    /// 忽略并 debug。
    fn apply_header_map(
        &self,
        mut request: reqwest::RequestBuilder,
        headers: &BTreeMap<String, String>,
    ) -> reqwest::RequestBuilder {
        for (name, value) in headers {
            if RESTRICTED_HEADERS
                .iter()
                .any(|r| name.eq_ignore_ascii_case(r))
            {
                tracing::debug!(target: "fluxgate::sdk", header = %name, "restricted header ignored");
                continue;
            }
            match (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                (Ok(parsed_name), Ok(parsed_value)) => {
                    request = request.header(parsed_name, parsed_value);
                }
                _ => {
                    tracing::debug!(target: "fluxgate::sdk", header = %name, "invalid header ignored");
                }
            }
        }
        request
    }

    /// 发送 chat 请求。合并 `default_config` 与 `req.config`，POST /v1/chat/completions。
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);
        // extra_body 先、vendor_extensions 后（后者优先）；RESERVED_KEYS 拒绝
        let extensions = merge_request_extensions(&merged, req)?;

        let payload = ChatPayload {
            model: &req.model,
            messages: &req.messages,
            params: merged.openai_params(),
            extras: WireExtras {
                tools: &req.tools,
                tool_choice: &req.tool_choice,
                vendor_extensions: &extensions,
            },
        };

        let url =
            self.base_url
                .join("v1/chat/completions")
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

        let v: serde_json::Value = resp.json().await?;
        ChatResponse::from_wire(&v)
    }

    /// 发送流式 chat 请求。强制 `stream = true`，POST /v1/chat/completions，返回 ChatStream。
    pub async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, FluxgateSdkError> {
        let mut merged = self.default_config.merge(&req.config);
        merged.stream = true;
        let extensions = merge_request_extensions(&merged, req)?;

        let payload = ChatPayload {
            model: &req.model,
            messages: &req.messages,
            params: merged.openai_params(),
            extras: WireExtras {
                tools: &req.tools,
                tool_choice: &req.tool_choice,
                vendor_extensions: &extensions,
            },
        };

        let url =
            self.base_url
                .join("v1/chat/completions")
                .map_err(|e| FluxgateSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload);
        // 流式不设总超时：reqwest 总超时按整个 body 计且不按 chunk 重置，
        // SSE 长流会被中途切断；连接停滞由 client 级 read_timeout 看护
        // （RequestConfig.timeout 对流式不生效）。
        let request = self.apply_request_headers(request, &merged);
        let policy = self.retry_policy_for(&merged);

        // 仅响应头阶段可重试：bytes_stream 建立后不再重试（对齐
        // openai-node `!hasStreamingBody`）
        let resp = send_with_retry(&request, &policy).await?;

        Ok(ChatStream::from_bytes_stream(resp.bytes_stream()))
    }
}
