//! 请求级配置（temperature / top_p / top_k / max_tokens / stream / timeout / hints）。

use std::time::Duration;

use serde::Serialize;

/// OpenAI wire 参数视图：未设置的 Option 字段直接省略（不写显式 null，
/// 严格模式上游会拒绝 null，且省略免去空字段序列化分配）。
/// `vendor_hint` / `model_hint` 是 header 级参数，不出现在该视图。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OpenAiParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) presence_penalty: Option<f32>,
    pub(crate) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user: Option<String>,
}

/// 请求配置。字段覆盖 OpenAI Chat Completions API 常用参数，
/// 外加 `vendor_hint` / `model_hint` 用于路由 header（不进入请求 body）。
#[derive(Debug, Clone, Default)]
pub struct RequestConfig {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    /// 频率惩罚（IR SamplingParams 同名语义；-2.0 ~ 2.0）
    pub frequency_penalty: Option<f32>,
    /// 存在惩罚（IR SamplingParams 同名语义）
    pub presence_penalty: Option<f32>,
    pub stream: bool,
    /// 非流式请求（chat / embed）的总超时逐请求覆盖：Some(d) 取代客户端
    /// 默认（`FluxgateClientBuilder::timeout`，缺省 30s）；对
    /// `chat_stream` 不生效（流式不设总超时，由 read_timeout 看护停滞）。
    pub timeout: Option<Duration>,
    pub vendor_hint: Option<String>,
    pub model_hint: Option<String>,
    pub stop: Option<Vec<String>>,
    pub user: Option<String>,
}

impl RequestConfig {
    /// 合并配置：`other` 的非 None 字段优先覆盖 `self`。
    /// `stream` 总是取 `other` 的值（显式覆盖）。
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            temperature: other.temperature.or(self.temperature),
            top_p: other.top_p.or(self.top_p),
            top_k: other.top_k.or(self.top_k),
            max_tokens: other.max_tokens.or(self.max_tokens),
            frequency_penalty: other.frequency_penalty.or(self.frequency_penalty),
            presence_penalty: other.presence_penalty.or(self.presence_penalty),
            stream: other.stream,
            timeout: other.timeout.or(self.timeout),
            vendor_hint: other
                .vendor_hint
                .clone()
                .or_else(|| self.vendor_hint.clone()),
            model_hint: other.model_hint.clone().or_else(|| self.model_hint.clone()),
            stop: other.stop.clone().or_else(|| self.stop.clone()),
            user: other.user.clone().or_else(|| self.user.clone()),
        }
    }

    /// OpenAI wire 参数视图（未设字段省略；`timeout`/hints 不进 body）。
    pub(crate) fn openai_params(&self) -> OpenAiParams {
        OpenAiParams {
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_tokens: self.max_tokens,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            stream: self.stream,
            stop: self.stop.clone(),
            user: self.user.clone(),
        }
    }
}
