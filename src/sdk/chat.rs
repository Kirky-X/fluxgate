//! Chat 请求/响应类型 — wire 契约对齐网关真实形状（D2/D7.11 口径）。
//!
//! SDK 零 crate 内依赖（独立客户端契约面）：类型自有、serde 对齐
//! `llm_gate::protocol` IR 的线上投影——多模态内容件 / 工具调用片段 /
//! 四变体 finish_reason / 含 cache_read 的 usage。`response_format` 等无
//! IR 表达的参数**不造**（对齐 IR 现有表达）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::sdk::config::RequestConfig;
use crate::sdk::error::NexusFlowSdkError;

/// 消息角色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 消息内容 — 纯文本（线上为裸字符串）或多模态内容件数组（线上为
/// 块数组）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatContent {
    /// 纯文本（线上序列化为裸字符串）
    Text(String),
    /// 多模态内容件数组（图片等，OpenAI wire 块形状）
    Parts(Vec<ChatContentPart>),
}

impl ChatContent {
    /// 纯文本便捷构造。
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

impl From<String> for ChatContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for ChatContent {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}

/// 多模态内容件 — OpenAI chat wire 形状（`type` 判别）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatContentPart {
    /// 文本块
    Text { text: String },
    /// 图片块（URL 或 data URL；base64 内联请先编码为
    /// `data:{media_type};base64,{data}` 形态——见 [`Self::image_data_url`]）
    ImageUrl { image_url: ImageUrlBody },
}

/// 图片 URL 体（OpenAI wire `image_url` 嵌套形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrlBody {
    pub url: String,
}

impl ChatContentPart {
    /// 文本块便捷构造。
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// 远程 URL 图片块。
    pub fn image_url(url: impl Into<String>) -> Self {
        Self::ImageUrl {
            image_url: ImageUrlBody { url: url.into() },
        }
    }

    /// base64 内联图片块（编码为 data URL 形态；逐 3 字节标准字母表，
    /// SDK 面零新依赖）。`media_type` **未经消毒**，逐字拼入 data URL
    /// 前缀——调用方须传入受控的 MIME 串（如 "image/png"），勿透传
    /// 不可信输入。
    pub fn image_data_url(media_type: impl std::fmt::Display, data: impl AsRef<[u8]>) -> Self {
        use std::fmt::Write as _;
        let mut url = format!("data:{media_type};base64,");
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let data = data.as_ref();
        for chunk in data.chunks(3) {
            let b = [
                chunk.first().copied().unwrap_or(0),
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let _ = write!(
                url,
                "{}{}{}{}",
                table[(n >> 18) as usize & 63] as char,
                table[(n >> 12) as usize & 63] as char,
                if chunk.len() > 1 {
                    table[(n >> 6) as usize & 63] as char
                } else {
                    '='
                },
                if chunk.len() > 2 {
                    table[n as usize & 63] as char
                } else {
                    '='
                },
            );
        }
        Self::ImageUrl {
            image_url: ImageUrlBody { url },
        }
    }
}

/// assistant 消息携带的工具调用（非流式 wire：无 index）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolCall {
    /// 工具调用 id（上游产出）
    pub id: String,
    /// 函数引用
    pub function: ChatToolFunction,
}

/// 工具调用函数引用（name + JSON 字符串 arguments）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolFunction {
    pub name: String,
    /// JSON 字符串（线上即字符串，不二次结构化）
    pub arguments: String,
}

/// 单条聊天消息 — tool 角色携带 `tool_call_id`；assistant 可携带
/// `tool_calls`（工具调用环）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: ChatContent,
    /// tool 角色消息：本条结果对应的工具调用 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// assistant 消息：发起的工具调用
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

impl ChatMessage {
    /// user 消息。
    pub fn user(content: impl Into<ChatContent>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// system 消息。
    pub fn system(content: impl Into<ChatContent>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// assistant 消息。
    pub fn assistant(content: impl Into<ChatContent>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// assistant 工具调用消息。
    pub fn assistant_with_tool_calls(
        content: impl Into<ChatContent>,
        tool_calls: Vec<ChatToolCall>,
    ) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Some(tool_calls),
        }
    }

    /// tool 结果消息（回填上游工具调用 id）。
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<ChatContent>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }
}

/// 工具定义（function calling）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatTool {
    pub function: ChatToolDefinition,
}

/// 工具函数定义 — `parameters` 为 JSON Schema。
///
/// **无损透传**（A3/ M1 闭环，原「有损透传警告」作废）：网关以
/// `serde_json::Value` 原样直存直出（嵌套 schema/items/anyOf/$ref/缺
/// `type` 属性全量保真）。注意：网关**不做 schema 校验**——schema 合法
/// 性与上游模型兼容性由调用方自担（上游 4xx 原样回传）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// 工具选择 — 策略词（auto/none/required）或具名函数（对齐 IR
/// `ToolChoice` 的线上形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatToolChoice {
    /// 具名函数（`{"type":"function","function":{"name":...}}`）
    Specific {
        #[serde(rename = "type")]
        choice_type: String,
        function: ChatToolChoiceFunction,
    },
    /// 策略词
    Strategy(String),
}

/// 具名工具选择的函数体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatToolChoiceFunction {
    pub name: String,
}

impl ChatToolChoice {
    pub fn auto() -> Self {
        Self::Strategy("auto".into())
    }
    pub fn none() -> Self {
        Self::Strategy("none".into())
    }
    pub fn required() -> Self {
        Self::Strategy("required".into())
    }
    pub fn specific(name: impl Into<String>) -> Self {
        Self::Specific {
            choice_type: "function".into(),
            function: ChatToolChoiceFunction { name: name.into() },
        }
    }
}

/// finish_reason 四变体（对齐 IR FinishReason）+ 上游自定义词兜底。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatFinishReason {
    /// 规范四变体（线上词：stop/length/tool_calls/content_filter）
    Standard(StandardFinishReason),
    /// 上游自定义词透传（网关对未知词双侧透传）
    Other(String),
}

/// 规范 finish_reason 四变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StandardFinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

impl ChatFinishReason {
    /// 从线上词解析。`function_call`（旧版 OpenAI 工具终止词）归并到
    /// `ToolCalls`（审查 LOW：语义同代的变体不落 Other）。
    pub fn from_wire(wire: &str) -> Self {
        match wire {
            "stop" => Self::Standard(StandardFinishReason::Stop),
            "length" => Self::Standard(StandardFinishReason::Length),
            "tool_calls" | "function_call" => Self::Standard(StandardFinishReason::ToolCalls),
            "content_filter" => Self::Standard(StandardFinishReason::ContentFilter),
            other => Self::Other(other.to_string()),
        }
    }

    /// 线上词。
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Standard(StandardFinishReason::Stop) => "stop",
            Self::Standard(StandardFinishReason::Length) => "length",
            Self::Standard(StandardFinishReason::ToolCalls) => "tool_calls",
            Self::Standard(StandardFinishReason::ContentFilter) => "content_filter",
            Self::Other(s) => s,
        }
    }
}

/// usage（token 计数；cache_read 来自线上
/// `prompt_tokens_details.cached_tokens`（嵌套，Deserialize 手写展开）——
/// cache_creation 无 OpenAI 线上槽，网关不产出，SDK 不造）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// prompt cache 命中 tokens（0 = 上游未携带）
    pub cache_read_tokens: u64,
}

impl<'de> Deserialize<'de> for ChatUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            prompt_tokens: u64,
            #[serde(default)]
            completion_tokens: u64,
            #[serde(default)]
            total_tokens: u64,
            #[serde(default)]
            prompt_tokens_details: Option<CachedDetail>,
        }
        #[derive(Deserialize)]
        struct CachedDetail {
            #[serde(default)]
            cached_tokens: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            prompt_tokens: wire.prompt_tokens,
            completion_tokens: wire.completion_tokens,
            total_tokens: wire.total_tokens,
            cache_read_tokens: wire
                .prompt_tokens_details
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
        })
    }
}

/// vendor_extensions 保留键 denylist —— ChatPayload 已序列化的 typed 键
/// 全集（model/messages + OpenAiParams 视图 + 工具面）。与网关侧
/// OPENAI_KNOWN_KEYS 同源同步：任一侧新增 typed 键必须同步本表
///（denylist 漏项 = 扩展键覆写 typed 字段的请求骨架注入面）。
pub const RESERVED_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "tools",
    "tool_choice",
    "max_tokens",
    "temperature",
    "top_p",
    "top_k",
    "frequency_penalty",
    "presence_penalty",
    "stop",
    "user",
    "vendor_extensions",
];

/// Chat 请求 — model + 消息 + 请求级配置 + 工具面 + vendor_extensions
/// 透传（顶层键合并进请求体；网关按协议合法键 denylist 过滤后透传）。
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub config: RequestConfig,
    /// 工具定义（function calling）
    pub tools: Vec<ChatTool>,
    /// 工具选择策略
    pub tool_choice: Option<ChatToolChoice>,
    /// vendor 扩展透传（顶层键；网关 denylist 过滤，同协议保真）
    pub vendor_extensions: BTreeMap<String, serde_json::Value>,
}

impl ChatRequest {
    /// 便捷构造：单条 user 消息 + 默认配置。
    pub fn new(model: &str, content: &str) -> Self {
        Self {
            model: model.to_string(),
            messages: vec![ChatMessage::user(content)],
            config: RequestConfig::default(),
            tools: Vec::new(),
            tool_choice: None,
            vendor_extensions: BTreeMap::new(),
        }
    }

    /// 设置工具面（定义 + 选择策略）。
    pub fn with_tools(mut self, tools: Vec<ChatTool>, choice: Option<ChatToolChoice>) -> Self {
        self.tools = tools;
        self.tool_choice = choice;
        self
    }

    /// 透传 vendor 扩展（顶层键）。
    ///
    /// 安全（审查 HIGH）：`key` 不得与 ChatPayload 已序列化的 typed 键
    /// 冲突——flatten 合并时扩展键会**覆写结构体字段**（如 `model`/
    /// `messages`/`stream`），即客户端可借扩展键篡改请求骨架。
    /// [`RESERVED_KEYS`] denylist 与网关侧 OPENAI_KNOWN_KEYS 同源同步
    /// （两侧任一侧新增 typed 键须同步另一侧）。
    pub fn with_vendor_extension(
        mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<Self, NexusFlowSdkError> {
        let key = key.into();
        if RESERVED_KEYS.contains(&key.as_str()) {
            return Err(NexusFlowSdkError::Protocol {
                message: format!(
                    "vendor extension key `{key}` conflicts with a typed request field"
                ),
            });
        }
        self.vendor_extensions.insert(key, value);
        Ok(self)
    }
}

/// Chat 响应 — finish_reason 结构化 + 含 cache_read 的 usage + 工具调用。
/// 网关非流式响应**不携带 vendor 字段**（核实：`response_to_external`
/// 产物无该键，旧 SDK `vendor` 字段恒空串误导，已移除——BREAKING 记录于
/// docs/CHANGELOG.md）。
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub content: String,
    /// assistant 工具调用（模型发起工具环时非空）
    pub tool_calls: Vec<ChatToolCall>,
    pub finish_reason: ChatFinishReason,
    pub usage: ChatUsage,
}

impl ChatResponse {
    /// 从网关非流式响应 wire JSON 解析（真实形状——choices[0]
    /// message{content,tool_calls?} + finish_reason + usage 可选携带
    /// prompt_tokens_details.cached_tokens）。
    pub(crate) fn from_wire(v: &serde_json::Value) -> Result<Self, NexusFlowSdkError> {
        use crate::sdk::error::NexusFlowSdkError;

        let choices = v.get("choices").and_then(|c| c.as_array()).ok_or_else(|| {
            NexusFlowSdkError::Protocol {
                message: "missing choices array".into(),
            }
        })?;
        let choice = choices.first().ok_or_else(|| NexusFlowSdkError::Protocol {
            message: "empty choices array".into(),
        })?;

        let content = choice["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let tool_calls = choice["message"]["tool_calls"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|tc| {
                        serde_json::from_value::<ChatToolCall>(tc.clone()).map_err(|e| {
                            NexusFlowSdkError::Protocol {
                                message: format!("invalid tool_call: {e}"),
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let finish_reason = choice["finish_reason"]
            .as_str()
            .map(ChatFinishReason::from_wire)
            .unwrap_or(ChatFinishReason::Other(String::new()));

        let usage_json = v.get("usage");
        let cached = usage_json
            .and_then(|u| u.get("prompt_tokens_details"))
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let u64_or = |pointer: &str| -> u64 {
            usage_json
                .and_then(|u| u.pointer(pointer))
                .and_then(|t| t.as_u64())
                .unwrap_or(0)
        };

        Ok(Self {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            model: v["model"].as_str().unwrap_or_default().to_string(),
            content,
            tool_calls,
            finish_reason,
            usage: ChatUsage {
                prompt_tokens: u64_or("/prompt_tokens"),
                completion_tokens: u64_or("/completion_tokens"),
                total_tokens: u64_or("/total_tokens"),
                cache_read_tokens: cached,
            },
        })
    }
}
