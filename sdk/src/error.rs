//! SDK 错误类型 — Phase 3 用 thiserror 派生完整枚举。
#![allow(dead_code)]

/// SDK 统一错误类型。骨架，Phase 3 替换为 thiserror 枚举。
pub type NexusFlowSdkError = Box<dyn std::error::Error + Send + Sync>;
