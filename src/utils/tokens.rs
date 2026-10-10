// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 近似 token 估算（WP3 `POST /v1/messages/count_tokens` 与 WP8 quota
//! 预扣共用，同一口径）。
//!
//! 口径声明（近似，非精确分词）：
//! - 英文文本 ≈ 字符数/4（`CHARS_PER_TOKEN`），向上取整；中文/多模态
//!   偏差大（约 1.5-3×），属保守低估方向——配额预扣侧以回补找平，
//!   此处如实声明不做精度承诺；
//! - 每消息 + `MESSAGE_STRUCT_OVERHEAD`（role 等帧开销近似）；
//! - 内容块数组每块 + `BLOCK_STRUCT_OVERHEAD`；
//! - `tools` 按 schema 序列化字符数/4 计入（JSON 标点一并计入，近似）；
//! - `model` 字段名开销忽略不计。
//!
//! 确定性：同输入同输出（count_tokens 两次调用结果一致）。

use serde_json::Value;

/// 每个 token 的近似字符数（与 quota `[quota].est_chars_per_token` 缺省同值）。
pub const CHARS_PER_TOKEN: u64 = 4;

/// 每消息的结构开销（role/name 等帧开销近似，单位 tokens）。
pub const MESSAGE_STRUCT_OVERHEAD: u64 = 8;

/// 内容块数组中每块的结构开销（单位 tokens）。
pub const BLOCK_STRUCT_OVERHEAD: u64 = 4;

/// 纯文本的近似 token 数（字符数/4 向上取整）。
pub fn estimate_text_tokens(text: &str) -> u64 {
    estimate_text_tokens_with(text, CHARS_PER_TOKEN)
}

/// 纯文本的近似 token 数（字符数/`chars_per_token` 向上取整；`0` 按 1 计，
/// 防除零）。
pub fn estimate_text_tokens_with(text: &str, chars_per_token: u64) -> u64 {
    (text.chars().count() as u64).div_ceil(chars_per_token.max(1))
}

/// 消息 `content` 字段的近似 token 数：string 直接估算；数组逐块拼接
/// （取各块 `text` 字段，无 text 块按序列化字符计入）；其他形态按
/// 序列化字符计入。调用方叠加消息级开销。
pub fn estimate_content_tokens(content: &Value) -> u64 {
    estimate_content_tokens_with(content, CHARS_PER_TOKEN)
}

/// [`estimate_content_tokens`] 的参数化口径（WP8 配额预扣用配置值）。
pub fn estimate_content_tokens_with(content: &Value, chars_per_token: u64) -> u64 {
    match content {
        Value::String(s) => estimate_text_tokens_with(s, chars_per_token),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                match block.get("text").and_then(Value::as_str) {
                    Some(text) => {
                        estimate_text_tokens_with(text, chars_per_token) + BLOCK_STRUCT_OVERHEAD
                    }
                    None => {
                        // 无 text 块（如 image_url）：序列化字符近似
                        estimate_text_tokens_with(&block.to_string(), chars_per_token)
                            + BLOCK_STRUCT_OVERHEAD
                    }
                }
            })
            .sum(),
        // null/缺失 → 0；其他形态序列化近似
        Value::Null => 0,
        other => estimate_text_tokens_with(&other.to_string(), chars_per_token),
    }
}

/// 请求体级输入 token 估算：`messages` 逐条（内容 + 8 开销）＋ `system`
///（string 或内容块数组）＋ `tools`（schema 序列化字符数/4）。
/// 缺失字段按 0 计（字段存在性校验由调用方负责）。
pub fn estimate_messages_tokens(body: &Value) -> u64 {
    estimate_messages_tokens_with(body, CHARS_PER_TOKEN)
}

/// [`estimate_messages_tokens`] 的参数化口径（WP8 配额预扣用配置值）。
pub fn estimate_messages_tokens_with(body: &Value, chars_per_token: u64) -> u64 {
    let mut total = 0u64;

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            let content_tokens = message
                .get("content")
                .map(|c| estimate_content_tokens_with(c, chars_per_token))
                .unwrap_or(0);
            total += content_tokens + MESSAGE_STRUCT_OVERHEAD;
        }
    }

    if let Some(system) = body.get("system") {
        total += estimate_content_tokens_with(system, chars_per_token);
    }

    if let Some(tools) = body.get("tools") {
        if !tools.is_null() {
            total += estimate_text_tokens_with(&tools.to_string(), chars_per_token);
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parameterized_matches_default_at_four() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello world"}],
            "tools": [{"type": "function"}],
        });
        assert_eq!(
            estimate_messages_tokens_with(&body, 4),
            estimate_messages_tokens(&body)
        );
        // cpt=8 减半（向上取整）；cpt=0 不 panic（按 1 计）。
        assert!(estimate_messages_tokens_with(&body, 8) <= estimate_messages_tokens(&body));
        assert!(estimate_text_tokens_with("abcd", 0) > 0);
    }

    #[test]
    fn short_text_rounds_up() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("a"), 1);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        assert_eq!(estimate_text_tokens("abcde"), 2);
    }

    #[test]
    fn message_overhead_applies_per_message() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hi"},
            ],
        });
        // "hi" → 1 + 8 开销
        assert_eq!(estimate_messages_tokens(&body), 9);
    }

    #[test]
    fn deterministic_for_same_input() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello world, this is a test."},
                {"role": "assistant", "content": "Hi there."},
            ],
            "system": "Be helpful.",
            "tools": [{"type": "function", "function": {"name": "get_weather"}}],
        });
        assert_eq!(
            estimate_messages_tokens(&body),
            estimate_messages_tokens(&body)
        );
        assert!(estimate_messages_tokens(&body) > 0);
    }

    #[test]
    fn longer_input_yields_more_tokens() {
        let short = json!({
            "messages": [{"role": "user", "content": "hi"}],
        });
        let long = json!({
            "messages": [{"role": "user", "content": "hi there, please write a long essay about the history of computing for me"}],
        });
        assert!(estimate_messages_tokens(&long) > estimate_messages_tokens(&short));
    }

    #[test]
    fn content_blocks_array_counts_text_blocks() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image", "source": {"data": "aGVsbG8="}},
                ]},
            ],
        });
        // 文本块 ceil(13/4)=4+4开销；image 块序列化近似 +4开销；消息 +8
        let got = estimate_messages_tokens(&body);
        assert!(got >= 8 + 4 + 4, "块开销与消息开销必须计入: {got}");
    }
}
