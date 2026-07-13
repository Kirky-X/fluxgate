//! Chat 请求/响应类型 — Phase 4 完整实现。
#![allow(dead_code)]

use std::fmt;

/// 消息角色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl fmt::Display for ChatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => write!(f, "system"),
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
            Self::Tool => write!(f, "tool"),
        }
    }
}

/// 单条聊天消息。骨架，Phase 4 补充 serde 与方法。
pub struct ChatMessage;

/// Chat 请求。骨架，Phase 4 完整实现。
pub struct ChatRequest;

/// Chat 响应。骨架，Phase 4 完整实现。
pub struct ChatResponse;
