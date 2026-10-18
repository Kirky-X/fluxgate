// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Gemini wire 形状（仅声明本网关读写的字段；未知字段由 `Value`/`serde`
//! 忽略，保持上游扩展不被序列化污染）。

use serde::Deserialize;
use serde_json::Value;

/// Gemini 请求顶层已显式映射的键（absorb 反用 / 出站 merge denylist）。
///
/// `safetySettings`/`cachedContent` 触发原生直通，不经 IR；仍列入本表，
/// 防止手构 IR 把二者当 vendor_extensions 注入其它协议目标。
pub const GEMINI_KNOWN_KEYS: &[&str] = &[
    "contents",
    "systemInstruction",
    "system_instruction",
    "tools",
    "toolConfig",
    "tool_config",
    "generationConfig",
    "generation_config",
    "safetySettings",
    "safety_settings",
    "cachedContent",
    "cached_content",
    "labels",
    "model",
];

/// `:action` 后缀。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiAction {
    GenerateContent,
    StreamGenerateContent,
    CountTokens,
    EmbedContent,
    BatchEmbedContents,
}

impl GeminiAction {
    /// wire 词（大小写敏感，与 Google SDK 一致）。
    pub fn parse(action: &str) -> Option<Self> {
        match action {
            "generateContent" => Some(Self::GenerateContent),
            "streamGenerateContent" => Some(Self::StreamGenerateContent),
            "countTokens" => Some(Self::CountTokens),
            "embedContent" => Some(Self::EmbedContent),
            "batchEmbedContents" => Some(Self::BatchEmbedContents),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GenerateContent => "generateContent",
            Self::StreamGenerateContent => "streamGenerateContent",
            Self::CountTokens => "countTokens",
            Self::EmbedContent => "embedContent",
            Self::BatchEmbedContents => "batchEmbedContents",
        }
    }

    pub fn is_stream(self) -> bool {
        matches!(self, Self::StreamGenerateContent)
    }

    pub fn is_embedding(self) -> bool {
        matches!(self, Self::EmbedContent | Self::BatchEmbedContents)
    }
}

/// model 归一化：剥离一次 `models/` 前缀（Gemini 允许两种写法混用）。
pub fn normalize_model(model: &str) -> &str {
    model.trim().strip_prefix("models/").unwrap_or(model.trim())
}

/// `usageMetadata`（camelCase wire）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub thoughts_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
    #[serde(default)]
    pub total_token_count: u64,
    #[serde(default)]
    pub tool_use_prompt_token_count: u64,
}

/// 响应候选。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
}

/// 候选内容。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Content {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// 内容分片：字段全可选，按出现键分派。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default)]
    pub text: Option<String>,
    /// thought summary 标记（`true` 时文本为思考摘要，不作为正文）。
    #[serde(default)]
    pub thought: bool,
    #[serde(default)]
    pub inline_data: Option<Blob>,
    #[serde(default)]
    pub file_data: Option<FileData>,
    #[serde(default)]
    pub function_call: Option<FunctionCall>,
    #[serde(default)]
    pub function_response: Option<FunctionResponse>,
}

/// 内联二进制（base64）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub data: String,
}

/// Files API / gs:// 引用。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_uri: String,
}

/// 函数调用（Gemini 侧完整 JSON，非增量字符串）。
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub args: Option<Value>,
}

/// 函数结果。
#[derive(Debug, Clone, Deserialize)]
pub struct FunctionResponse {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub response: Option<Value>,
}

/// `GenerateContentResponse`（非流式与流式 chunk 同形）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    pub prompt_feedback: Option<Value>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub response_id: Option<String>,
}

/// 解析响应体；非对象或结构不合法 → None（调用方映射 502）。
pub fn parse_response(value: &Value) -> Option<GenerateContentResponse> {
    serde_json::from_value(value.clone()).ok()
}

impl GenerateContentResponse {
    /// 首候选 finishReason（便捷访问）。
    pub fn finish_reason(&self) -> Option<&str> {
        self.candidates
            .first()
            .and_then(|c| c.finish_reason.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_parse_and_wire() {
        for action in [
            GeminiAction::GenerateContent,
            GeminiAction::StreamGenerateContent,
            GeminiAction::CountTokens,
            GeminiAction::EmbedContent,
            GeminiAction::BatchEmbedContents,
        ] {
            assert_eq!(GeminiAction::parse(action.as_str()), Some(action));
        }
        assert_eq!(GeminiAction::parse("predict"), None);
        assert!(GeminiAction::StreamGenerateContent.is_stream());
        assert!(!GeminiAction::GenerateContent.is_stream());
        assert!(GeminiAction::EmbedContent.is_embedding());
        assert!(!GeminiAction::CountTokens.is_embedding());
    }

    #[test]
    fn model_normalization_strips_single_prefix() {
        assert_eq!(normalize_model("models/gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(normalize_model("gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(normalize_model(" models/gemini-2.5-pro "), "gemini-2.5-pro");
        // 只剥一次：双前缀保留一层（上游会显式拒绝，避免静默改写）
        assert_eq!(normalize_model("models/models/x"), "models/x");
    }

    #[test]
    fn response_parses_parts_and_usage() {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "hi"},
                    {"functionCall": {"name": "f", "args": {"a": 1}}},
                    {"inlineData": {"mimeType": "image/png", "data": "AAA"}},
                    {"fileData": {"mimeType": "image/jpeg", "fileUri": "gs://b/x"}}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 4,
                "thoughtsTokenCount": 2,
                "cachedContentTokenCount": 1,
                "totalTokenCount": 9
            },
            "modelVersion": "gemini-2.5-pro",
            "responseId": "r-1"
        });
        let parsed = parse_response(&body).expect("parse");
        let parts = &parsed.candidates[0].content.as_ref().unwrap().parts;
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].text.as_deref(), Some("hi"));
        assert_eq!(parts[1].function_call.as_ref().unwrap().name, "f");
        assert!(parts[2].inline_data.is_some());
        assert!(parts[3].file_data.is_some());
        let usage = parsed.usage_metadata.clone().unwrap();
        assert_eq!(usage.prompt_token_count, 3);
        assert_eq!(usage.candidates_token_count, 4);
        assert_eq!(usage.thoughts_token_count, 2);
        assert_eq!(usage.cached_content_token_count, 1);
        assert_eq!(usage.total_token_count, 9);
        assert_eq!(parsed.finish_reason(), Some("STOP"));
    }

    #[test]
    fn invalid_shape_returns_none() {
        assert!(parse_response(&json!({"candidates": "nope"})).is_none());
        assert!(parse_response(&json!({"candidates": []})).is_some());
    }
}
