// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::error::GateError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 统一请求结构（对齐 core `CompletionRequest`）：flat 采样槽与
/// [`SamplingParams`] 并存，出站时 flat 优先，`sampling` 只补未设置的槽
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnifiedRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// 规范系统提示槽：单一事实源，出站优先与 Role::System 消息合并去重；
    /// OpenAI 侧无本槽
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
    /// 停止序列。字段名沿用 wire 惯例 `stop`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub logit_bias: HashMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// core 对齐采样参数槽：槽 `Some` = 显式携带；内部字段 `None` = 沿用
    /// provider 缺省。出站 flat 优先，本槽仅补未设置的槽
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingParams>,
    /// 思考深度配置（对齐 core `ThinkingConfig` types.rs）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    /// 是否回传思考摘要（Gemini `thinkingConfig.includeThoughts`）。
    /// 其它协议无对应槽，出站忽略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_include_thoughts: Option<bool>,
    /// 任务努力级别（对齐 core `EffortLevel` types.rs）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<EffortLevel>,
    /// 当前轮次索引（core runner 逐轮注入，从 0 计；无 wire 表示——
    /// 两侧协议均不映射，仅 IR/serde 承载）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<u64>,
    /// 工具定义列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// 工具选择策略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// 并行工具调用开关（OpenAI chat / Responses 有；Anthropic 无对应槽，
    /// 出站丢弃）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// 结构化输出配置（OpenAI `response_format` / Responses `text.format`）。
    /// `Value` 原样保真；Anthropic 无对应槽，出站丢弃
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    /// 厂商扩展透传表：入站吸收未识别顶层字段（防丢），出站按原键名
    /// 透传回顶层；目标协议 known_keys（denylist）永不由扩展表补入/覆写。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vendor_extensions: HashMap<String, serde_json::Value>,
}

/// LLM 采样参数（对齐 core `SamplingParams`）。全部可选：`None` 表示
/// 沿用 provider 默认值。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
}

/// 思考深度配置（对齐 core `ThinkingConfig`，`crates/core/src/traits/types.rs`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    /// 模型自适应决定思考预算
    Adaptive,
    /// 启用思考，指定 token 预算（u64 对齐 core 宽度）
    Enabled {
        #[serde(default)]
        budget_tokens: u64,
    },
    /// 禁用思考（默认）
    #[default]
    Disabled,
}

/// 任务努力级别（对齐 core `EffortLevel`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    #[default]
    Medium,
    High,
}

impl EffortLevel {
    /// wire 词汇（OpenAI `reasoning_effort` 同形）→ IR 枚举；未知词返回 `None`。
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// IR 枚举 → wire 词汇（小写，OpenAI `reasoning_effort` 同形）。
    pub fn to_wire(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// LLM 补全完成原因（对齐 core `FinishReason`）。
///
/// 与 [`Choice::finish_reason`] 的原始透传槽构成双轨：结构化槽存已知词
/// 的映射结果，未知词只进原始槽（不猜变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

impl FinishReason {
    /// wire 词汇（OpenAI finish_reason / Anthropic stop_reason 映射后的
    /// IR 统一词汇）→ 结构化变体；未知词返回 `None`。
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "stop" => Some(Self::Stop),
            "length" => Some(Self::Length),
            "tool_calls" | "function_call" => Some(Self::ToolCalls),
            "content_filter" => Some(Self::ContentFilter),
            _ => None,
        }
    }

    /// 结构化变体 → IR 统一 wire 词汇（OpenAI finish_reason 词汇）。
    pub fn to_wire(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
        }
    }
}

/// 工具选择策略
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// 使用特定工具
    Specific {
        #[serde(rename = "type")]
        choice_type: String,
        function: ToolChoiceFunction,
    },
    /// 策略类型: "auto", "none", "required"
    Strategy(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

/// 消息角色
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "tool")]
    Tool,
}

/// 工具定义 - 统一的工具结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// 工具类型，目前支持 "function"
    #[serde(rename = "type", default)]
    pub tool_type: String,
    /// 函数工具定义
    #[serde(default)]
    pub function: ToolFunction,
}

impl Default for Tool {
    fn default() -> Self {
        Self {
            tool_type: "function".to_string(),
            function: ToolFunction::default(),
        }
    }
}

/// 函数工具定义
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolFunction {
    /// 函数名称
    pub name: String,
    /// 函数描述
    pub description: String,
    /// 函数参数 —— JSON Schema 原样保真：`Value` 直存直出，不做结构化
    /// 升降级。`Null` = 未携带（出站按协议缺省形状补齐）。
    #[serde(default)]
    pub parameters: Value,
    /// 严格 schema 模式（OpenAI chat `function.strict` / Responses 顶层
    /// `strict`）；Anthropic 无对应槽，出站丢弃
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// 单个工具 `parameters` schema 的序列化字节上限（128 KiB），
/// 超限入站 400 拒绝（防任意大 schema 消耗上游带宽与 token 预算）
pub const MAX_TOOL_SCHEMA_BYTES: usize = 128 * 1024;

/// 单个工具 `parameters` schema 的递归深度上限（32 层），超限入站 400 拒绝
pub const MAX_TOOL_SCHEMA_DEPTH: usize = 32;

/// 计算 [`serde_json::Value`] 的递归深度：叶子（标量/Null）为 0，
/// 容器为 1 + 子节点最大深度（空容器为 1）。
pub(crate) fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// 工具 schema 体积/深度上限校验（入站解析处）：超限返回 Validation
///（请求级 400），field 携带工具下标 + 实测值 + 上限值
pub(crate) fn validate_tool_schemas(tools: &[Tool]) -> Result<(), GateError> {
    for (index, tool) in tools.iter().enumerate() {
        let parameters = &tool.function.parameters;
        let size = serde_json::to_vec(parameters)
            .map_err(|e| GateError::Validation {
                field: format!("tools[{index}].parameters: schema serialization failed: {e}"),
            })?
            .len();
        if size > MAX_TOOL_SCHEMA_BYTES {
            return Err(GateError::Validation {
                field: format!(
                    "tools[{index}].parameters: serialized size {size} bytes exceeds limit {MAX_TOOL_SCHEMA_BYTES}"
                ),
            });
        }
        let depth = json_depth(parameters);
        if depth > MAX_TOOL_SCHEMA_DEPTH {
            return Err(GateError::Validation {
                field: format!(
                    "tools[{index}].parameters: nesting depth {depth} exceeds limit {MAX_TOOL_SCHEMA_DEPTH}"
                ),
            });
        }
    }
    Ok(())
}

/// 工具调用 - 助手消息中的工具调用请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 ID
    pub id: String,
    /// 调用类型，通常为 "function"
    #[serde(rename = "type", default = "default_tool_call_type")]
    pub call_type: String,
    /// 函数信息
    pub function: ToolCallFunction,
}

fn default_tool_call_type() -> String {
    "function".to_string()
}

/// 工具调用函数信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    /// 函数名称
    pub name: String,
    /// 函数参数 (JSON 字符串)
    pub arguments: String,
}

/// 工具结果 - 工具执行后的返回结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// 关联的工具调用 ID
    pub tool_call_id: String,
    /// 工具函数名称
    #[serde(default)]
    pub function_name: String,
    /// 工具执行结果内容
    pub content: String,
    /// 是否为错误
    #[serde(default)]
    pub is_error: bool,
}

/// 消息内容
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    MultiModal(Vec<ContentBlock>),
    /// 工具调用内容（助手消息中的工具调用）
    ToolCalls(Vec<ToolCall>),
    /// 工具结果内容（工具角色消息的结果）
    ToolResult(ToolResult),
    /// 混合内容：文本 + 工具调用
    #[serde(rename_all = "camelCase")]
    TextWithToolCalls {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
}

impl Default for MessageContent {
    fn default() -> Self {
        MessageContent::Text(String::new())
    }
}

/// 多模态内容块
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    #[serde(rename = "type")]
    Type(String),
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        detail: Option<String>,
    },
}

/// 图片来源。
///
/// 枚举化区分 base64 内联与远程 URL 两种形态（修复旧结构体把 URL 塞进
/// `data` 并硬编码 jpeg 的往返失真）：
/// - `Base64` 序列化为 `{"type":"base64","media_type":...,"data":...}`
///   （与 Anthropic `source` 块同形，旧 IR 结构体形状向后兼容）；
/// - `Url` 序列化为 `{"type":"url","url":...}`（OpenAI `image_url.url`
///   直链与 Anthropic URL source 同形）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// 消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

/// 统一响应结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedResponse {
    pub id: String,
    #[serde(default)]
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: TokenUsage,
}

/// 响应选择。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// 原始透传槽：上游 finish_reason/stop_reason 词汇原样保留（含未知词）。
    /// 与 [`Self::finish`] 结构化槽双轨并存：解析时已知词双写两槽，
    /// 出站时结构化槽优先（`finish` 有值且 `finish_reason` 为空时回填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// 结构化完成原因（对齐 core `FinishReason`；`None` = 上游词未知或未携带）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Token 使用统计（对齐 core `Usage`）。cache 宽度 u64 对齐 core。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    /// 缓存读取 token 数（prompt cache hit；u64 对齐 core Usage 宽度）
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// 缓存创建 token 数（prompt cache miss / new cache entries；u64 对齐 core）
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// USD 计价成本（协议 wire 无成本字段，仅 IR/serde 承载）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// CNY 计价成本（协议 wire 无成本字段，仅 IR/serde 承载）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_cny: Option<f64>,
}

/// 流式响应块（映射 core `StreamChunk`）。
///
/// 终止语义由 `Content{finish_reason: Some(_)}` 承担，不设独立 Done 变体；
/// [`StreamChunk::ToolCall`] 为流式工具片段流（并行工具 = 多 id 交错流）。
/// `Content` 携带 `MessageContent::ToolCalls` 仅为旧表示回退——openai
/// 单帧多片段 / 片段与 finish_reason 同帧的非规范形状（trait 单 chunk
/// 交付约束下的无损回退）；`Heartbeat` 为网关保心跳，无 core 对应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamChunk {
    #[serde(rename = "content")]
    Content {
        index: u32,
        delta: MessageContent,
        #[serde(default)]
        finish_reason: Option<String>,
    },
    /// 流式工具调用片段（对齐 core `ToolCallDelta` types.rs）。
    /// `id`/`name` 仅首片段携带（续片为空/None）；`arguments` 为增量分片。
    #[serde(rename = "tool_call")]
    ToolCall {
        index: u32,
        #[serde(default)]
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<String>,
    },
    /// 流式用量结算（取最后一次出现的值结算）。
    /// `finish_reason` 承载与 usage 同帧到达的终止原因（Anthropic
    /// `message_delta` 尾帧的 `delta.stop_reason` 映射结果；OpenAI 侧
    /// 终止原因先行于 usage 尾帧，恒 `None`）。
    #[serde(rename = "usage")]
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        #[serde(default)]
        cache_read_tokens: u32,
        #[serde(default)]
        cache_creation_tokens: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat,
}

/// 统一 Embedding 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedEmbeddingResponse {
    pub id: String,
    #[serde(default)]
    pub object: String,
    pub created: u64,
    pub model: String,
    pub data: Vec<EmbeddingData>,
    pub usage: EmbeddingUsage,
}

/// Embedding 数据项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    pub index: u32,
    #[serde(rename = "object")]
    pub object_: String,
    pub embedding: Vec<f32>,
}

/// Embedding 使用统计
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    #[serde(default, rename = "prompt_tokens")]
    pub prompt_tokens: u32,
    #[serde(default, rename = "total_tokens")]
    pub total_tokens: u32,
}

/// 入站吸收未识别顶层字段 → [`UnifiedRequest::vendor_extensions`]（透传防丢）。
///
/// `known_keys` 为该协议已显式映射进 IR 的键；其余顶层字段原样收进扩展表，
/// 出站时按原键名透传。
pub(crate) fn absorb_vendor_extensions(
    body: &serde_json::Value,
    known_keys: &[&str],
) -> HashMap<String, serde_json::Value> {
    body.as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(key, _)| !known_keys.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// 出站透传 [`UnifiedRequest::vendor_extensions`] → 顶层键。
///
/// `deny_keys` 为目标协议的合法键集（调用侧传入本协议 known_keys 反用）：
/// 协议键永不由扩展表补入/覆写；非协议键原样透传。
pub(crate) fn merge_vendor_extensions(
    body: &mut serde_json::Value,
    extensions: &HashMap<String, serde_json::Value>,
    deny_keys: &[&str],
) {
    if extensions.is_empty() {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        for (key, value) in extensions {
            if deny_keys.contains(&key.as_str()) {
                continue;
            }
            obj.entry(key.clone()).or_insert(value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vendor_extensions_absorb_and_merge_helpers() {
        let body = json!({
            "model": "m",
            "messages": [],
            "seed": 42,
            "top_k": 40
        });
        let extensions = absorb_vendor_extensions(&body, &["model", "messages"]);
        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions.get("seed"), Some(&json!(42)));

        let mut out = json!({"model": "m"});
        merge_vendor_extensions(&mut out, &extensions, &["model", "messages"]);
        assert_eq!(out["seed"], 42);
        assert_eq!(out["top_k"], 40);
        assert_eq!(out["model"], "m");

        // denylist（known_keys 反用）：协议键不得补入/覆写
        let mut out = json!({"model": "real"});
        let mut hostile = std::collections::HashMap::new();
        hostile.insert("model".to_string(), json!("hijacked"));
        hostile.insert("stream".to_string(), json!(true));
        merge_vendor_extensions(&mut out, &hostile, &["model", "stream"]);
        assert_eq!(out["model"], "real");
        assert!(out.get("stream").is_none());

        // 非对象 body 与空扩展表为 no-op
        let mut scalar = json!(1);
        merge_vendor_extensions(&mut scalar, &std::collections::HashMap::new(), &[]);
        assert_eq!(scalar, json!(1));
    }

    #[test]
    fn finish_reason_wire_mapping_table() {
        // 四变体全映射（OpenAI 词汇）+ OpenAI 旧词汇 function_call
        assert_eq!(FinishReason::from_wire("stop"), Some(FinishReason::Stop));
        assert_eq!(
            FinishReason::from_wire("length"),
            Some(FinishReason::Length)
        );
        assert_eq!(
            FinishReason::from_wire("tool_calls"),
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(
            FinishReason::from_wire("function_call"),
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(
            FinishReason::from_wire("content_filter"),
            Some(FinishReason::ContentFilter)
        );
        // 未知词不猜变体
        assert_eq!(FinishReason::from_wire("pause_turn"), None);
        assert_eq!(FinishReason::from_wire(""), None);
        // 反向一致
        for wire in ["stop", "length", "tool_calls", "content_filter"] {
            let finish = FinishReason::from_wire(wire).unwrap();
            assert_eq!(finish.to_wire(), wire);
        }
    }

    #[test]
    fn finish_reason_serde_roundtrip() {
        for finish in [
            FinishReason::Stop,
            FinishReason::Length,
            FinishReason::ToolCalls,
            FinishReason::ContentFilter,
        ] {
            let json = serde_json::to_value(finish).unwrap();
            let back: FinishReason = serde_json::from_value(json).unwrap();
            assert_eq!(back, finish);
        }
        assert_eq!(
            serde_json::to_value(FinishReason::ToolCalls).unwrap(),
            json!("tool_calls")
        );
    }

    #[test]
    fn effort_level_wire_and_serde() {
        assert_eq!(EffortLevel::from_wire("low"), Some(EffortLevel::Low));
        assert_eq!(EffortLevel::from_wire("medium"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_wire("high"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_wire("minimal"), None);
        assert_eq!(EffortLevel::High.to_wire(), "high");
        assert_eq!(
            serde_json::to_value(EffortLevel::Medium).unwrap(),
            json!("medium")
        );
        let back: EffortLevel = serde_json::from_value(json!("high")).unwrap();
        assert_eq!(back, EffortLevel::High);
    }

    #[test]
    fn thinking_config_serde_roundtrip() {
        let enabled = ThinkingConfig::Enabled {
            budget_tokens: 2048,
        };
        let json = serde_json::to_value(&enabled).unwrap();
        assert_eq!(json, json!({"type": "enabled", "budget_tokens": 2048}));
        let back: ThinkingConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, enabled);

        assert_eq!(
            serde_json::to_value(&ThinkingConfig::Adaptive).unwrap(),
            json!({"type": "adaptive"})
        );
        let back: ThinkingConfig = serde_json::from_value(json!({"type": "adaptive"})).unwrap();
        assert_eq!(back, ThinkingConfig::Adaptive);

        let back: ThinkingConfig = serde_json::from_value(json!({"type": "disabled"})).unwrap();
        assert_eq!(back, ThinkingConfig::Disabled);
        assert_eq!(ThinkingConfig::default(), ThinkingConfig::Disabled);
    }

    #[test]
    fn sampling_params_serde_roundtrip() {
        let full = SamplingParams {
            temperature: Some(0.7),
            top_k: Some(40),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(-0.2),
        };
        let json = serde_json::to_value(&full).unwrap();
        let back: SamplingParams = serde_json::from_value(json).unwrap();
        assert_eq!(back, full);

        // 空参数全部跳过序列化
        let empty_json = serde_json::to_value(SamplingParams::default()).unwrap();
        assert_eq!(empty_json, json!({}));
        let back: SamplingParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(back, SamplingParams::default());
    }

    #[test]
    fn token_usage_cache_and_cost_fields() {
        // 旧形状（仅三 token 字段）解析：cache 字段 serde default，成本 None
        let legacy: TokenUsage = serde_json::from_value(
            json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}),
        )
        .unwrap();
        assert_eq!(legacy.cache_read_tokens, 0);
        assert_eq!(legacy.cache_creation_tokens, 0);
        assert!(legacy.cost_usd.is_none());
        assert!(legacy.cost_cny.is_none());

        let full = TokenUsage {
            cache_read_tokens: 8,
            cache_creation_tokens: 3,
            cost_usd: Some(0.25),
            cost_cny: Some(1.8),
            ..Default::default()
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["cache_read_tokens"], 8);
        assert_eq!(json["cache_creation_tokens"], 3);
        let back: TokenUsage = serde_json::from_value(json).unwrap();
        assert_eq!(back.cache_read_tokens, 8);
        assert_eq!(back.cache_creation_tokens, 3);
        assert_eq!(back.cost_usd, Some(0.25));
        assert_eq!(back.cost_cny, Some(1.8));

        // None 成本字段跳过序列化
        let json = serde_json::to_value(TokenUsage::default()).unwrap();
        assert!(json.get("cost_usd").is_none());
        assert!(json.get("cost_cny").is_none());
    }

    #[test]
    fn unified_request_new_fields_serde() {
        // 旧形状解析：新字段全部 serde default
        let legacy: UnifiedRequest = serde_json::from_value(json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap();
        assert!(legacy.system_prompt.is_none());
        assert!(legacy.sampling.is_none());
        assert!(legacy.thinking_config.is_none());
        assert!(legacy.effort.is_none());
        assert!(legacy.turn_index.is_none());
        assert!(legacy.vendor_extensions.is_empty());

        // 全量新字段往返
        let req = UnifiedRequest {
            system_prompt: Some("be terse".into()),
            sampling: Some(SamplingParams {
                top_k: Some(40),
                ..Default::default()
            }),
            thinking_config: Some(ThinkingConfig::Enabled {
                budget_tokens: 1024,
            }),
            effort: Some(EffortLevel::High),
            turn_index: Some(3),
            vendor_extensions: [("x_custom".to_string(), json!({"flag": true}))]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["system_prompt"], "be terse");
        assert_eq!(json["sampling"]["top_k"], 40);
        assert_eq!(json["thinking_config"]["type"], "enabled");
        assert_eq!(json["effort"], "high");
        assert_eq!(json["turn_index"], 3);
        assert_eq!(json["vendor_extensions"]["x_custom"]["flag"], true);
        let back: UnifiedRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.system_prompt.as_deref(), Some("be terse"));
        assert_eq!(back.sampling.unwrap().top_k, Some(40));
        assert_eq!(
            back.thinking_config,
            Some(ThinkingConfig::Enabled {
                budget_tokens: 1024
            })
        );
        assert_eq!(back.effort, Some(EffortLevel::High));
        assert_eq!(back.turn_index, Some(3));
        assert!(back.vendor_extensions.contains_key("x_custom"));

        // 新字段缺省时全部跳过序列化
        let minimal = UnifiedRequest::default();
        let json = serde_json::to_value(&minimal).unwrap();
        for key in [
            "system_prompt",
            "sampling",
            "thinking_config",
            "effort",
            "turn_index",
            "vendor_extensions",
        ] {
            assert!(json.get(key).is_none(), "{key} 应被跳过");
        }
    }

    #[test]
    fn image_source_enum_serde() {
        let base64 = ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "aGk=".into(),
        };
        let json = serde_json::to_value(&base64).unwrap();
        // 与 Anthropic source 块同形
        assert_eq!(
            json,
            json!({"type": "base64", "media_type": "image/png", "data": "aGk="})
        );
        let back: ImageSource = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ImageSource::Base64 { .. }));

        let url = ImageSource::Url {
            url: "https://example.com/cat.png".into(),
        };
        let json = serde_json::to_value(&url).unwrap();
        assert_eq!(
            json,
            json!({"type": "url", "url": "https://example.com/cat.png"})
        );
        let back: ImageSource = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ImageSource::Url { .. }));
    }

    #[test]
    fn choice_finish_slot_serde() {
        // 结构化槽与原始槽双轨：序列化同现、缺省跳过
        let choice = Choice {
            finish_reason: Some("tool_calls".into()),
            finish: Some(FinishReason::ToolCalls),
            ..Default::default()
        };
        let json = serde_json::to_value(&choice).unwrap();
        assert_eq!(json["finish_reason"], "tool_calls");
        assert_eq!(json["finish"], "tool_calls");
        let back: Choice = serde_json::from_value(json).unwrap();
        assert_eq!(back.finish, Some(FinishReason::ToolCalls));

        let json = serde_json::to_value(Choice::default()).unwrap();
        assert!(json.get("finish").is_none());
        assert!(json.get("finish_reason").is_none());
    }

    #[test]
    fn stream_chunk_tool_call_serde() {
        let chunk = StreamChunk::ToolCall {
            index: 0,
            id: "call_1".into(),
            name: Some("get_weather".into()),
            arguments: Some("{\"city\":".into()),
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["type"], "tool_call");
        assert_eq!(json["id"], "call_1");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["arguments"], "{\"city\":");
        let back: StreamChunk = serde_json::from_value(json).unwrap();
        assert!(matches!(back, StreamChunk::ToolCall { .. }));

        // 续片段：id/name 缺省
        let cont: StreamChunk = serde_json::from_value(json!({
            "type": "tool_call",
            "index": 0,
            "arguments": ":5}"
        }))
        .unwrap();
        match cont {
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, 0);
                assert_eq!(id, "");
                assert_eq!(name, None);
                assert_eq!(arguments.as_deref(), Some(":5}"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn stream_chunk_usage_cache_and_finish_fields() {
        // 旧形状（三 token 字段）解析：cache 字段 default，finish_reason None
        let legacy: StreamChunk = serde_json::from_value(json!({
            "type": "usage",
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }))
        .unwrap();
        match legacy {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
            } => {
                assert_eq!(
                    (prompt_tokens, completion_tokens, total_tokens),
                    (10, 5, 15)
                );
                assert_eq!((cache_read_tokens, cache_creation_tokens), (0, 0));
                assert_eq!(finish_reason, None);
            }
            other => panic!("expected Usage, got {other:?}"),
        }

        // 新字段往返（Anthropic message_delta 尾帧形状）
        let chunk = StreamChunk::Usage {
            prompt_tokens: 25,
            completion_tokens: 9,
            total_tokens: 34,
            cache_read_tokens: 12,
            cache_creation_tokens: 4,
            finish_reason: Some("tool_calls".into()),
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["cache_read_tokens"], 12);
        assert_eq!(json["cache_creation_tokens"], 4);
        assert_eq!(json["finish_reason"], "tool_calls");
        let back: StreamChunk = serde_json::from_value(json).unwrap();
        assert!(matches!(
            back,
            StreamChunk::Usage {
                cache_read_tokens: 12,
                finish_reason: Some(_),
                ..
            }
        ));
    }

    // ======== 工具 schema 体积/深度上限 ========

    use crate::error::GateError;

    fn tool_with_params(name: &str, parameters: Value) -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: name.to_string(),
                description: String::new(),
                parameters,
                strict: None,
            },
        }
    }

    /// 序列化后恰为 `target` 字节的 Value（`{"a":"xxx…"}` = 8 + pad 字节）。
    fn value_of_serialized_size(target: usize) -> Value {
        json!({ "a": "x".repeat(target - 8) })
    }

    /// 递归深度恰为 `depth` 的 Value（叶子上套 depth 层数组）。
    fn value_of_depth(depth: usize) -> Value {
        let mut v = json!(0);
        for _ in 0..depth {
            v = json!([v]);
        }
        v
    }

    #[test]
    fn json_depth_counts_container_nesting() {
        assert_eq!(json_depth(&json!(null)), 0, "叶子（Null/标量）深度为 0");
        assert_eq!(json_depth(&json!(42)), 0);
        assert_eq!(json_depth(&json!({"type": "object"})), 1, "空容器深度为 1");
        assert_eq!(
            json_depth(&json!({"a": [1, {"b": 2}]})),
            3,
            "容器 = 1 + 子节点最大深度"
        );
    }

    #[test]
    fn validate_tool_schemas_accepts_null_and_exact_boundaries() {
        let tools = vec![
            tool_with_params("no_params", Value::Null),
            tool_with_params(
                "fat_boundary",
                value_of_serialized_size(MAX_TOOL_SCHEMA_BYTES),
            ),
            tool_with_params("deep_boundary", value_of_depth(MAX_TOOL_SCHEMA_DEPTH)),
        ];
        assert!(
            validate_tool_schemas(&tools).is_ok(),
            "未携带 schema 与恰在上限（128KB / 32 层）的 schema 必须放行"
        );
    }

    #[test]
    fn validate_tool_schemas_rejects_oversized_schema_with_locator_and_limits() {
        let tools = vec![tool_with_params(
            "fat",
            value_of_serialized_size(MAX_TOOL_SCHEMA_BYTES + 1),
        )];
        let err = validate_tool_schemas(&tools).unwrap_err();
        assert_eq!(err.status_code(), 400, "超限入站拒绝映射请求级 400");
        assert!(matches!(err, GateError::Validation { .. }));
        let GateError::Validation { field } = &err else {
            panic!("expected Validation, got {err:?}")
        };
        assert!(
            field.contains("tools[0].parameters"),
            "定位到出错工具: {field}"
        );
        assert!(
            field.contains(&MAX_TOOL_SCHEMA_BYTES.to_string()),
            "显性化上限值: {field}"
        );
    }

    #[test]
    fn validate_tool_schemas_rejects_overdeep_schema() {
        let tools = vec![tool_with_params(
            "deep",
            value_of_depth(MAX_TOOL_SCHEMA_DEPTH + 1),
        )];
        let err = validate_tool_schemas(&tools).unwrap_err();
        let GateError::Validation { field } = &err else {
            panic!("expected Validation, got {err:?}")
        };
        assert!(field.contains("depth"), "超深拒绝须显性化深度语义: {field}");
        assert!(
            field.contains(&MAX_TOOL_SCHEMA_DEPTH.to_string()),
            "显性化上限值: {field}"
        );
    }

    #[test]
    fn validate_tool_schemas_reports_offending_tool_index() {
        let tools = vec![
            tool_with_params("ok", json!({"type": "object"})),
            tool_with_params("fat", value_of_serialized_size(MAX_TOOL_SCHEMA_BYTES + 1)),
        ];
        let err = validate_tool_schemas(&tools).unwrap_err();
        let GateError::Validation { field } = &err else {
            panic!("expected Validation, got {err:?}")
        };
        assert!(
            field.contains("tools[1].parameters"),
            "第二个工具超限，定位下标须为 1: {field}"
        );
    }
}
