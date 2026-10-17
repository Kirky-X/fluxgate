// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 路由提示提取器：从请求头提取 X-Vendor / X-Model / X-Session-Id
//!（会话键缺失时兜底 messages 前缀哈希）

use axum::http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// messages[0] 兜底哈希的输入截断（前 2KB）
const FALLBACK_HASH_INPUT_BYTES: usize = 2048;
/// 兜底会话键的 hex 截断长度（64 bit，碰撞面与键长度平衡）
const FALLBACK_HASH_HEX_LEN: usize = 16;

/// 路由提示结构
#[derive(Debug, Clone, Default)]
pub struct RoutingHints {
    /// 优先使用的厂商 ID
    pub preferred_vendor: Option<String>,
    /// 优先使用的模型 ID
    pub preferred_model: Option<String>,
    /// 会话亲和键（X-Session-Id header）
    pub session_key: Option<String>,
    /// N1 batch 标记（X-Batch: 1）
    pub batch: bool,
    /// 请求级权重（X-Weight header，LiteLLM simple_shuffle
    /// _router_weights 对照）。仅接受有限正数——非数值 / 非有限 / 非正
    /// 一律忽略为 None（非法值忽略语义）。是否生效由
    /// `[routing].allow_request_weight` 门控（Router 侧热读判定）。
    pub preferred_weight: Option<f64>,
}

impl RoutingHints {
    /// 从请求头提取路由提示
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            preferred_vendor: headers
                .get("x-vendor")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
                .map(String::from),
            preferred_model: headers
                .get("x-model")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
                .map(String::from),
            session_key: headers
                .get("x-session-id")
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
                .map(String::from),
            preferred_weight: headers
                .get("x-weight")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|w| w.is_finite() && *w > 0.0),
            batch: headers
                .get("x-batch")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim() == "1")
                .unwrap_or(false),
        }
    }

    /// 检查是否有路由提示
    pub fn has_hints(&self) -> bool {
        self.preferred_vendor.is_some() || self.preferred_model.is_some()
    }

    /// 会话亲和键 — `X-Session-Id` 优先；缺失时兜底
    /// `sha256(messages[0] 序列化前 2KB)` 截断 hex。两者皆不可得
    ///（无 header 且 body 无 messages）返回 None，该请求不参与亲和。
    pub fn effective_session_key(&self, body: &Value) -> Option<String> {
        if let Some(sk) = &self.session_key {
            return Some(sk.clone());
        }
        fallback_session_key(body)
    }
}

/// messages 前缀兜底会话键 — 同前缀的对话粘同一 vendor。
/// 输入为 `serde_json::to_string(messages[0])` 的前 2KB（字符边界安全
/// 截断），SHA256 hex 截前 16 字符。
pub fn fallback_session_key(body: &Value) -> Option<String> {
    let first = body.get("messages")?.get(0)?;
    let serialized = serde_json::to_string(first).ok()?;
    if serialized.is_empty() {
        return None;
    }
    let mut take = serialized.len().min(FALLBACK_HASH_INPUT_BYTES);
    while take > 0 && !serialized.is_char_boundary(take) {
        take -= 1;
    }
    let digest = Sha256::digest(&serialized.as_bytes()[..take]);
    Some(hex::encode(digest)[..FALLBACK_HASH_HEX_LEN].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn session_key_from_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "sess-42".parse().unwrap());
        let hints = RoutingHints::from_headers(&headers);
        assert_eq!(hints.session_key.as_deref(), Some("sess-42"));
        // header 存在时优先于 body 兜底
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(
            hints.effective_session_key(&body).as_deref(),
            Some("sess-42")
        );
    }

    #[test]
    fn no_session_headers_yields_none_until_fallback() {
        let hints = RoutingHints::from_headers(&HeaderMap::new());
        assert!(hints.session_key.is_none());
        assert_eq!(
            hints.effective_session_key(&serde_json::json!({"model": "gpt-4"})),
            None,
            "无 messages 的 body 无兜底键"
        );
    }

    /// 无 header 时同前缀 messages[0] → 同兜底键；内容不同 → 不同键。
    #[test]
    fn fallback_hash_is_prefix_deterministic() {
        let body_a = serde_json::json!({"messages": [{"role": "user", "content": "hello"}]});
        let body_b = serde_json::json!({"messages": [{"role": "user", "content": "hello"}]});
        let body_c = serde_json::json!({"messages": [{"role": "user", "content": "world"}]});
        let ka = fallback_session_key(&body_a).expect("hash key");
        let kb = fallback_session_key(&body_b).expect("hash key");
        let kc = fallback_session_key(&body_c).expect("hash key");
        assert_eq!(ka, kb, "相同 messages[0] 必须同键");
        assert_ne!(ka, kc, "不同 messages[0] 必须异键");
        assert_eq!(ka.len(), FALLBACK_HASH_HEX_LEN);
        // effective_session_key 的兜底路径与直接调用一致
        let hints = RoutingHints::from_headers(&HeaderMap::new());
        assert_eq!(
            hints.effective_session_key(&body_a).as_deref(),
            Some(ka.as_str())
        );
    }

    /// 超长 messages[0]：输入截 2KB 后哈希——前 2KB 相同的巨型消息同键。
    #[test]
    fn fallback_hash_truncates_long_messages() {
        let long = "x".repeat(5000);
        let body_a = serde_json::json!({"messages": [{"role": "user", "content": long}]});
        let mut suffix = "x".repeat(5000);
        suffix.push_str("-tail");
        let body_b = serde_json::json!({"messages": [{"role": "user", "content": suffix}]});
        let ka = fallback_session_key(&body_a).expect("hash key");
        let kb = fallback_session_key(&body_b).expect("hash key");
        assert_eq!(ka, kb, "前 2KB 相同的长消息必须同键（截断语义）");
    }

    // ============ tests：X-Weight 请求级权重解析 ============

    /// X-Weight 头解析：合法有限正数入结构；非法值（非数值 / 负数 /
    /// 非有限 / 缺失）一律忽略为 None。
    #[test]
    fn x_weight_header_parsed_and_invalid_ignored() {
        let mut h = HeaderMap::new();
        h.insert("x-weight", "3.0".parse().unwrap());
        assert_eq!(RoutingHints::from_headers(&h).preferred_weight, Some(3.0));

        let mut h = HeaderMap::new();
        h.insert("x-weight", "0.5".parse().unwrap());
        assert_eq!(RoutingHints::from_headers(&h).preferred_weight, Some(0.5));

        let mut h = HeaderMap::new();
        h.insert("x-weight", "abc".parse().unwrap());
        assert_eq!(
            RoutingHints::from_headers(&h).preferred_weight,
            None,
            "非数值忽略"
        );

        let mut h = HeaderMap::new();
        h.insert("x-weight", "-1".parse().unwrap());
        assert_eq!(
            RoutingHints::from_headers(&h).preferred_weight,
            None,
            "非正数忽略"
        );

        let mut h = HeaderMap::new();
        h.insert("x-weight", "NaN".parse().unwrap());
        assert_eq!(
            RoutingHints::from_headers(&h).preferred_weight,
            None,
            "非有限忽略"
        );

        assert_eq!(
            RoutingHints::from_headers(&HeaderMap::new()).preferred_weight,
            None,
            "缺失忽略"
        );
    }
}
