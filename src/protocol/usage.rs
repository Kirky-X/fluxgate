// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 响应 usage 提数（分账记账与配额回补共用单源）。
//!
//! OpenAI、Anthropic 与 Gemini 的 usage 形态属协议知识，配额侧与分账侧共用同一
//! 实现，禁止第二套提数逻辑。

/// 分账用量提取：OpenAI / Anthropic / Gemini 三形态，返回
/// (prompt, completion, cache_read, cache_creation)，u64 饱和钳 u32。无可用 usage → None（调用方跳过记账，保持预扣）。
pub(crate) fn parse_usage_detailed(response: &serde_json::Value) -> Option<(u32, u32, u32, u32)> {
    let clamp = |v: Option<u64>| v.unwrap_or(0).min(u32::MAX as u64) as u32;
    // Gemini：usageMetadata camelCase；completion = candidates + thoughts（思考
    // token 按输出计费），cache_read = cachedContentTokenCount（prompt 总量子集）
    if let Some(usage) = response.get("usageMetadata") {
        let prompt = usage.get("promptTokenCount").and_then(|v| v.as_u64());
        if let Some(prompt) = prompt {
            let candidates = usage
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let thoughts = usage
                .get("thoughtsTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cache_read = usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            return Some((
                clamp(Some(prompt)),
                clamp(Some(candidates.saturating_add(thoughts))),
                clamp(Some(cache_read)),
                0,
            ));
        }
    }
    let usage = response.get("usage")?;
    if let Some(prompt) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
        let completion = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return Some((
            clamp(Some(prompt)),
            clamp(Some(completion)),
            clamp(Some(cache_read)),
            0,
        ));
    }
    if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        // Anthropic 用顶层 cache_*_input_tokens；Responses 用
        // input_tokens_details.cached_tokens（优先 Anthropic 形态）
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        return Some((
            clamp(Some(input)),
            clamp(Some(output)),
            clamp(Some(cache_read)),
            clamp(Some(cache_creation)),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gemini 形态：completion = candidates + thoughts，cache_read 取 cachedContentTokenCount。
    #[test]
    fn gemini_shape_reads_usage_metadata() {
        let body = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 3,
                "totalTokenCount": 18,
                "cachedContentTokenCount": 4
            }
        });
        assert_eq!(parse_usage_detailed(&body), Some((10, 8, 4, 0)));
        let minimal = serde_json::json!({"usageMetadata": {"promptTokenCount": 9}});
        assert_eq!(parse_usage_detailed(&minimal), Some((9, 0, 0, 0)));
        assert_eq!(
            parse_usage_detailed(&serde_json::json!({"usageMetadata": {}})),
            None
        );
    }

    /// OpenAI 形态（cache_read 来自 prompt_tokens_details.cached_tokens，
    /// cache_creation 恒 0）。
    #[test]
    fn openai_shape() {
        let body = serde_json::json!({
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 20}
            }
        });
        assert_eq!(parse_usage_detailed(&body), Some((50, 7, 20, 0)));
    }

    #[test]
    fn responses_shape_reads_cached_tokens_details() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 20,
                "total_tokens": 120,
                "input_tokens_details": {"cached_tokens": 40}
            }
        });
        assert_eq!(parse_usage_detailed(&body), Some((100, 20, 40, 0)));

        // Anthropic 顶层字段优先（两种形态同时出现时）
        let both = serde_json::json!({
            "usage": {
                "input_tokens": 10,
                "output_tokens": 1,
                "cache_read_input_tokens": 7,
                "input_tokens_details": {"cached_tokens": 3}
            }
        });
        assert_eq!(parse_usage_detailed(&both), Some((10, 1, 7, 0)));
    }

    /// Anthropic 形态（四档齐备）与缺省 0；无 usage → None；超大值钳 u32。
    #[test]
    fn anthropic_shape_and_edges() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 120,
                "output_tokens": 5,
                "cache_read_input_tokens": 60,
                "cache_creation_input_tokens": 30
            }
        });
        assert_eq!(parse_usage_detailed(&body), Some((120, 5, 60, 30)));
        let minimal = serde_json::json!({"usage": {"input_tokens": 9}});
        assert_eq!(parse_usage_detailed(&minimal), Some((9, 0, 0, 0)));
        assert_eq!(parse_usage_detailed(&serde_json::json!({})), None);
        let huge = serde_json::json!({"usage": {"prompt_tokens": u64::MAX}});
        assert_eq!(
            parse_usage_detailed(&huge),
            Some((u32::MAX, 0, 0, 0)),
            "饱和钳 u32"
        );
    }
}
