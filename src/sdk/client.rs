//! Fluxgate SDK 客户端 — HTTP 调用 /v1/chat/completions。

use std::time::Duration;

use url::Url;

use crate::sdk::chat::{ChatRequest, ChatResponse};
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::stream::ChatStream;

/// 非流式请求（chat / embed）的默认总超时。reqwest 默认无总超时，
/// 挂起的网关会让调用方任务无限悬挂。不作用于 [`FluxgateClient::chat_stream`]：
/// 流式不设总超时（reqwest 总超时按整个 body 计且不按 chunk 重置，
/// SSE 长流会被中途切断），由 [`DEFAULT_READ_TIMEOUT`] 看护。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 流读取超时默认值：作用于所有请求的每次读取，成功读取后重置——
/// 只拦截「连接停滞」（连续 30s 无字节），不限制长流的总时长。
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// 计算生效超时：显式值优先，否则落到 [`DEFAULT_REQUEST_TIMEOUT`]。
fn effective_timeout(explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or(DEFAULT_REQUEST_TIMEOUT)
}

/// 计算生效读超时：显式值优先，否则落到 [`DEFAULT_READ_TIMEOUT`]。
fn effective_read_timeout(explicit: Option<Duration>) -> Duration {
    explicit.unwrap_or(DEFAULT_READ_TIMEOUT)
}

/// Fluxgate 网关客户端。
pub struct FluxgateClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Url,
    pub(crate) api_key: String,
    pub(crate) default_config: RequestConfig,
    /// 非流式请求的默认总超时（chat_stream 不适用；实际执行逐请求设置）
    pub(crate) effective_timeout: Duration,
    /// 流读取超时（reqwest client 级 read_timeout）
    pub(crate) read_timeout: Duration,
}

// api_key 是 Bearer 凭证：手写 Debug 脱敏，不用 derive（会打印明文）
impl std::fmt::Debug for FluxgateClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FluxgateClient")
            .field("http", &self.http)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("effective_timeout", &self.effective_timeout)
            .field("read_timeout", &self.read_timeout)
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
        assert_eq!(default_client.read_timeout, DEFAULT_READ_TIMEOUT);

        let tuned = FluxgateClientBuilder::new()
            .base_url("http://127.0.0.1:8080")
            .api_key("k")
            .read_timeout(Duration::from_millis(400))
            .build()
            .unwrap();
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
    default_config: RequestConfig,
    timeout: Option<Duration>,
    read_timeout: Option<Duration>,
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

    pub fn default_config(mut self, config: RequestConfig) -> Self {
        self.default_config = config;
        self
    }

    /// 非流式请求（chat / embed）的默认总超时；不作用于流式请求。
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 流读取超时（按读重置，拦截连接停滞）。
    pub fn read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = Some(read_timeout);
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
        // client 级只设 read_timeout（按读重置，拦截连接停滞）：
        // 总超时若设在 client 级会按整个 body 计且不按 chunk 重置，
        // 把 SSE 长流在默认 30s 处切断。非流式请求的总超时逐请求设置。
        let read = effective_read_timeout(self.read_timeout);
        builder = builder.read_timeout(read);
        let http = builder.build()?;
        let effective = effective_timeout(self.timeout);

        Ok(FluxgateClient {
            http,
            base_url,
            api_key,
            default_config: self.default_config,
            effective_timeout: effective,
            read_timeout: read,
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

impl FluxgateClient {
    /// 发送 chat 请求。合并 `default_config` 与 `req.config`，POST /v1/chat/completions。
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, FluxgateSdkError> {
        let merged = self.default_config.merge(&req.config);

        let payload = ChatPayload {
            model: &req.model,
            messages: &req.messages,
            params: merged.openai_params(),
            extras: WireExtras {
                tools: &req.tools,
                tool_choice: &req.tool_choice,
                vendor_extensions: &req.vendor_extensions,
            },
        };

        let url =
            self.base_url
                .join("v1/chat/completions")
                .map_err(|e| FluxgateSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            // 非流式总超时逐请求设置：请求级 config.timeout 覆盖客户端默认
            .timeout(merged.timeout.unwrap_or(self.effective_timeout));

        if let Some(v) = &merged.vendor_hint {
            request = request.header("X-Vendor", v);
        }
        if let Some(m) = &merged.model_hint {
            request = request.header("X-Model", m);
        }

        let resp = request.send().await?;
        let status = resp.status();

        if status.as_u16() == 401 {
            return Err(FluxgateSdkError::Auth {
                message: "unauthorized".into(),
            });
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        let v: serde_json::Value = resp.json().await?;
        ChatResponse::from_wire(&v)
    }

    /// 发送流式 chat 请求。强制 `stream = true`，POST /v1/chat/completions，返回 ChatStream。
    pub async fn chat_stream(&self, req: &ChatRequest) -> Result<ChatStream, FluxgateSdkError> {
        let mut merged = self.default_config.merge(&req.config);
        merged.stream = true;

        let payload = ChatPayload {
            model: &req.model,
            messages: &req.messages,
            params: merged.openai_params(),
            extras: WireExtras {
                tools: &req.tools,
                tool_choice: &req.tool_choice,
                vendor_extensions: &req.vendor_extensions,
            },
        };

        let url =
            self.base_url
                .join("v1/chat/completions")
                .map_err(|e| FluxgateSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let mut request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload);
        // 流式不设总超时：reqwest 总超时按整个 body 计且不按 chunk 重置，
        // SSE 长流会被中途切断；连接停滞由 client 级 read_timeout 看护
        // （RequestConfig.timeout 对流式不生效）。

        if let Some(v) = &merged.vendor_hint {
            request = request.header("X-Vendor", v);
        }
        if let Some(m) = &merged.model_hint {
            request = request.header("X-Model", m);
        }

        let resp = request.send().await?;
        let status = resp.status();

        if status.as_u16() == 401 {
            return Err(FluxgateSdkError::Auth {
                message: "unauthorized".into(),
            });
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(FluxgateSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(ChatStream::from_bytes_stream(resp.bytes_stream()))
    }
}
