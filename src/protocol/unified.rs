// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 统一请求结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
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
    /// 工具定义列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// 工具选择策略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
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
    /// 函数参数 (JSON Schema 格式)
    #[serde(default)]
    pub parameters: ToolParameters,
}

/// 函数参数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameters {
    /// 参数类型，通常为 "object"
    #[serde(rename = "type", default = "default_param_type")]
    pub param_type: String,
    /// 属性定义
    #[serde(default)]
    pub properties: HashMap<String, ToolParameterProperty>,
    /// 必需参数列表
    #[serde(default)]
    pub required: Vec<String>,
}

impl Default for ToolParameters {
    fn default() -> Self {
        Self {
            param_type: "object".to_string(),
            properties: HashMap::new(),
            required: Vec::new(),
        }
    }
}

fn default_param_type() -> String {
    "object".to_string()
}

/// 参数属性定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameterProperty {
    /// 参数类型
    #[serde(rename = "type")]
    pub param_type: String,
    /// 参数描述
    #[serde(default)]
    pub description: String,
    /// 枚举值
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
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

/// 图片来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub type_: String,
    pub media_type: String,
    pub data: String,
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

/// 响应选择
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// Token 使用统计
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

/// 流式响应块
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
    #[serde(rename = "usage")]
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
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
