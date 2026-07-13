//! Chat 请求/响应类型。

use serde::{Deserialize, Serialize};

use crate::config::RequestConfig;

/// 消息角色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 单条聊天消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// Chat 请求。
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub config: RequestConfig,
}

impl ChatRequest {
    /// 便捷构造：单条 user 消息 + 默认配置。
    pub fn new(model: &str, content: &str) -> Self {
        Self {
            model: model.to_string(),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: content.to_string(),
            }],
            config: RequestConfig::default(),
        }
    }
}

/// Chat 响应（从 OpenAI 格式解析后的扁平结构）。
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub content: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub vendor: String,
    pub finish_reason: String,
}
