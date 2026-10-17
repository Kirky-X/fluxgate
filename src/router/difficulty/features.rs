// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 难度定档特征提取（纯函数，零 IO）：结构特征 + 可配置文本词法特征。
//!
//! 输入为请求 body（openai/anthropic 两种 messages 形态兼容），输出
//! [`RuleFeatures`]。文本面**有界**：引擎 token 预算超限即拒绝而非截断
//!（vecboost `decision.rs` 契约），故本层取「system + 末条 user 消息」
//! 前缀 ≤2KB，超长请求的剩余信息由结构特征（消息数/长度分档）承接。

use serde_json::Value;
use std::borrow::Cow;

/// 文本摘录上限（字符）：vecboost 决策引擎 token 预算的安全边距。
/// 超预算引擎显性拒绝而非截断，故这是技术契约、不是业务策略，不随配置变化。
pub const MAX_TEXT_CHARS: usize = 2048;

/// 特征词法表借视图：由 orchestrator 从 `[routing.difficulty.lexicon]` 注入，
/// 零拷贝、零分配；生产路径不得使用内置默认。
#[derive(Debug, Clone, Copy)]
pub struct Lexicon<'a> {
    pub hard: &'a [String],
    pub easy: &'a [String],
    pub math_symbols: &'a [String],
    pub question_marks: &'a [String],
    pub code_fence_markers: &'a [String],
}

impl<'a> Lexicon<'a> {
    pub fn new(
        hard: &'a [String],
        easy: &'a [String],
        math_symbols: &'a [String],
        question_marks: &'a [String],
        code_fence_markers: &'a [String],
    ) -> Self {
        Self {
            hard,
            easy,
            math_symbols,
            question_marks,
            code_fence_markers,
        }
    }
}

/// 规则特征快照：结构维（与 LinUCB 5 维同源）+ 文本规则维
#[derive(Debug, Clone, PartialEq)]
pub struct RuleFeatures {
    pub num_messages: usize,
    pub body_chars: usize,
    pub has_tools: bool,
    pub stream: bool,
    /// hard 词表命中次数（同一词多次命中累计）
    pub hard_hits: u32,
    /// easy 词表命中次数
    pub easy_hits: u32,
    /// 问号数（多问拆解信号）
    pub question_marks: u32,
    /// 数学符号出现次数
    pub math_symbols: u32,
    /// 代码围栏（```）出现
    pub code_fence: bool,
    /// 文本摘录（≤[`MAX_TEXT_CHARS`] 字符，喂分类器的有界输入）
    pub text: String,
}

impl RuleFeatures {
    /// 空 body / 解析失败的中性特征（F7 降级输入）
    pub fn neutral() -> Self {
        Self {
            num_messages: 0,
            body_chars: 0,
            has_tools: false,
            stream: false,
            hard_hits: 0,
            easy_hits: 0,
            question_marks: 0,
            math_symbols: 0,
            code_fence: false,
            text: String::new(),
        }
    }
}

/// 从请求 body 提取规则特征。兼容 openai（`messages[].content` 字符串/分块）
/// 与 anthropic（`system` + 分块 `{type:"text",text:..}`）两种形态；
/// 任何畸形输入回落中性特征，不 panic。词法表由配置注入。
pub fn extract_features(body: &Value, lexicon: Lexicon<'_>) -> RuleFeatures {
    let mut f = RuleFeatures::neutral();
    let Some(obj) = body.as_object() else {
        return f;
    };

    f.num_messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    f.body_chars = estimate_body_chars(body);
    f.has_tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    f.stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let text = extract_text(body);
    f.hard_hits = count_hits(&text, lexicon.hard);
    f.easy_hits = count_hits(&text, lexicon.easy);
    f.math_symbols = count_hits(&text, lexicon.math_symbols);
    f.question_marks = count_hits(&text, lexicon.question_marks);
    f.code_fence = lexicon
        .code_fence_markers
        .iter()
        .any(|marker| text.contains(marker.as_str()));
    f.text = text;
    f
}

/// 文本摘录：system（anthropic 顶层字符串 / openai messages[0].role=system）
/// + 末条 user 消息。
///
/// 构建期增量钳 [`MAX_TEXT_CHARS`] 字符（分配 O(2KB)，与消息长度无关）。
/// 控制字符（除 tab/LF/CR）就地替换为空格：vecboost 对 state 字符串叶子
/// 的控制字符检查是显性拒绝，不消毒会让粘贴 ANSI 转义码的正常请求打挂
/// 分类器校验（进而误触熔断）。
fn extract_text(body: &Value) -> String {
    let obj = match body.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    let mut out = String::new();
    let push_part = |out: &mut String, part: &str| {
        if out.chars().count() >= MAX_TEXT_CHARS {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let remaining = MAX_TEXT_CHARS - out.chars().count();
        out.extend(part.chars().map(sanitize_char).take(remaining));
    };

    if let Some(system) = obj.get("system").and_then(Value::as_str) {
        push_part(&mut out, system);
    }

    if let Some(messages) = obj.get("messages").and_then(Value::as_array) {
        // openai 形态的 system 在 messages[0]
        if !out.contains('\n') && out.is_empty() {
            if let Some(first) = messages.first() {
                if first.get("role").and_then(Value::as_str) == Some("system") {
                    if let Some(text) = message_text(first.get("content")) {
                        push_part(&mut out, &text);
                    }
                }
            }
        }
        if let Some(last_user) = messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        {
            if let Some(text) = message_text(last_user.get("content")) {
                push_part(&mut out, &text);
            }
        }
    }
    out
}

/// 控制字符消毒：除 \t \n \r 外的 C0/C1 控制符替换为空格
fn sanitize_char(c: char) -> char {
    if c.is_control() && !matches!(c, '\t' | '\n' | '\r') {
        ' '
    } else {
        c
    }
}

/// body 结构长度估算（免分配、封顶 `CAP` 即早停）：`rule_score` 只消费
/// `min(body_chars, scoring.length_cap)` 的饱和语义，估算值只需单调够用，
/// 不追求与 serde 序列化逐字节相等。`CAP` 高于 `length_cap` 上限 8000，
/// 留出结构/引号余量。
fn estimate_body_chars(body: &Value) -> usize {
    const CAP: usize = 9000;
    // 节点权重只服务单调估算，不追求字节相等
    const NULL_CHARS: usize = 4;
    const BOOL_CHARS: usize = 5;
    const CONTAINER_CHARS: usize = 2;
    fn walk(v: &Value, acc: &mut usize) {
        if *acc >= CAP {
            return;
        }
        match v {
            Value::Null => *acc += NULL_CHARS,
            Value::Bool(_) => *acc += BOOL_CHARS,
            Value::Number(n) => *acc += n.to_string().len(),
            // 单节点即可超界：按剩余额度钳入（否则 1MB 单串一步击穿 CAP）
            Value::String(s) => *acc += (CAP - *acc).min(s.chars().count()),
            Value::Array(items) => {
                *acc += CONTAINER_CHARS;
                for item in items {
                    walk(item, acc);
                    if *acc >= CAP {
                        return;
                    }
                }
            }
            Value::Object(fields) => {
                *acc += CONTAINER_CHARS;
                for (key, value) in fields {
                    *acc += key.len() + CONTAINER_CHARS;
                    walk(value, acc);
                    if *acc >= CAP {
                        return;
                    }
                }
            }
        }
    }
    let mut acc = 0;
    walk(body, &mut acc);
    acc
}

/// message content 统一取文本：字符串借用零拷贝；分块数组拼接（该形态
/// 少见，允许一次分配）。调用方按字符增量消费，不持有 Cow 越界
fn message_text<'a>(content: Option<&'a Value>) -> Option<Cow<'a, str>> {
    match content? {
        Value::String(s) => Some(Cow::Borrowed(s)),
        Value::Array(blocks) => {
            let text: Vec<&str> = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(Cow::Owned(text.join("\n")))
            }
        }
        _ => None,
    }
}

fn count_hits(text: &str, tokens: &[String]) -> u32 {
    tokens
        .iter()
        .map(|token| count_occurrences(text, token.as_str()))
        .sum()
}

fn count_occurrences(haystack: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DifficultyLexiconSection;
    use serde_json::json;

    fn extract(body: &Value) -> RuleFeatures {
        let lexicon = DifficultyLexiconSection::default();
        extract_features(
            body,
            Lexicon::new(
                lexicon.hard_words(),
                lexicon.easy_words(),
                lexicon.math_symbols(),
                lexicon.question_marks(),
                lexicon.code_fence_markers(),
            ),
        )
    }

    #[test]
    fn empty_body_is_neutral() {
        assert_eq!(extract(&Value::Null), RuleFeatures::neutral());
        // 空 object：body_chars = "{}"（2），其余维度中性
        let f = extract(&json!({}));
        assert_eq!(f.num_messages, 0);
        assert_eq!(f.hard_hits, 0);
        assert_eq!(f.easy_hits, 0);
        assert!(f.text.is_empty());
    }

    #[test]
    fn chinese_hard_word_hits() {
        let f = extract(&json!({
            "messages": [{"role": "user", "content": "请证明这个定理，并给出推导过程"}]
        }));
        assert!(f.hard_hits >= 2, "证明+推导至少各一命中: {f:?}");
        assert_eq!(f.easy_hits, 0);
    }

    #[test]
    fn english_hard_word_and_code_fence_hit() {
        let f = extract(&json!({
            "messages": [{"role": "user", "content": "Prove the theorem step by step.\n```rust\nfn main() {}\n```"}]
        }));
        assert!(f.hard_hits >= 2);
        assert!(f.code_fence);
    }

    #[test]
    fn easy_word_hits() {
        let f = extract(&json!({
            "messages": [{"role": "user", "content": "hello, thanks! 请帮我翻译这句话"}]
        }));
        assert!(f.easy_hits >= 3, "hello+thanks+翻译: {f:?}");
    }

    #[test]
    fn structure_flags_and_math_symbols() {
        let f = extract(&json!({
            "stream": true,
            "tools": [{"type": "function"}],
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "a*x^2 + b = 0 有两个解吗？"}
            ]
        }));
        assert!(f.stream);
        assert!(f.has_tools);
        assert_eq!(f.num_messages, 2);
        assert!(f.math_symbols >= 4, "数学符号密度: {f:?}");
        assert!(f.question_marks >= 1, "全角问号计数: {f:?}");
        assert!(f.text.contains("be brief"), "system 摘录进文本");
        assert!(f.text.contains("a*x^2"), "user 消息摘录进文本");
    }

    #[test]
    fn anthropic_block_content_and_truncation() {
        let long = "长".repeat(MAX_TEXT_CHARS + 500);
        let f = extract(&json!({
            "system": "sys",
            "messages": [{"role": "user", "content": [{"type": "text", "text": long}]}]
        }));
        assert_eq!(f.text.chars().count(), MAX_TEXT_CHARS, "文本必须钳到上限");
    }

    #[test]
    fn control_chars_sanitized_before_text_use() {
        // \u001b（ESC/ANSI 色彩码）等控制字符必须消毒——vecboost 对 state
        // 字符串叶子的控制字符检查是显性拒绝
        let f = extract(&json!({
            "messages": [{"role": "user", "content": "a\u{1}b\u{1b}c\td\ne\rf"}]
        }));
        assert!(
            !f.text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')),
            "残留控制字符: {:?}",
            f.text
        );
        assert!(f.text.contains('a'), "正常字符保留");
    }

    #[test]
    fn body_chars_estimate_caps_bounded() {
        // 巨型 body：估算封顶（无全量序列化分配），小 body 有限值
        let huge = json!({"messages": [{"role": "user",
            "content": "x".repeat(1_000_000)}]});
        let f = extract(&huge);
        assert!(f.body_chars <= 9000, "估算必须封顶: {}", f.body_chars);
        let small = extract(&json!({"messages": [{"role": "user", "content": "hi"}]}));
        assert!(small.body_chars > 0 && small.body_chars < 9000);
    }

    #[test]
    fn malformed_content_falls_back_neutral() {
        let f = extract(&json!({
            "messages": [{"role": "user", "content": {"unexpected": 1}}]
        }));
        assert_eq!(f.hard_hits, 0);
        assert!(f.text.is_empty(), "畸形 content 不产生文本");
    }

    #[test]
    fn custom_lexicon_replaces_builtin_signals() {
        let hard = vec!["量子".to_string()];
        let easy: Vec<String> = Vec::new();
        let math = vec!["§".to_string()];
        let marks = vec!["！".to_string()];
        let fences = vec!["~~~".to_string()];
        let f = extract_features(
            &json!({"messages": [{"role": "user", "content": "证明 量子 § ！ ~~~"}]}),
            Lexicon::new(&hard, &easy, &math, &marks, &fences),
        );
        assert_eq!(f.hard_hits, 1, "内置 证明 不再命中，仅自定义 量子 命中");
        assert_eq!(f.easy_hits, 0);
        assert_eq!(f.math_symbols, 1);
        assert_eq!(f.question_marks, 1);
        assert!(f.code_fence);
    }
}
