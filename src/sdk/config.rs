//! 请求级配置（temperature / top_p / top_k / max_tokens / stream / timeout / hints）。

use std::time::Duration;

use serde_json::{json, Value};

/// 请求配置。字段覆盖 OpenAI Chat Completions API 常用参数，
/// 外加 `vendor_hint` / `model_hint` 用于路由 header（不进入请求 body）。
#[derive(Debug, Clone, Default)]
pub struct RequestConfig {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
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

    /// 转换为 OpenAI Chat Completions API 的 JSON body 片段。
    /// `vendor_hint` / `model_hint` 不出现在 body 中（它们是 header 级参数）。
    pub fn to_openai_json(&self) -> Value {
        json!({
            "temperature": self.temperature,
            "top_p": self.top_p,
            "top_k": self.top_k,
            "max_tokens": self.max_tokens,
            "stream": self.stream,
            "stop": self.stop,
            "user": self.user,
        })
    }
}
