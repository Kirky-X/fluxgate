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

/// `X-Session-Id` 头部值长度上限。跨后端契约：MySQL 列为 `VARCHAR(255)`，
/// PG b-tree 索引条目上限约 2704 字节；超长值会让整批日志写入失败并被
/// 丢弃（`async_logger` 的批回滚语义）。超限值**截断**而非丢弃——会话
/// 归属仍可近似成立，且不破坏既有请求。
const SESSION_KEY_MAX_BYTES: usize = 255;
/// `anthropic-beta` 头值长度上限（字节）
const ANTHROPIC_BETA_MAX_LEN: usize = 4096;

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
    /// 入站 `anthropic-beta` 头值（已过白名单校验）；Anthropic 型厂商
    /// 且 `forward_beta_headers` 开启时注入出站
    pub anthropic_beta: Option<String>,
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
                .map(truncate_session_key),
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
            anthropic_beta: headers
                .get("anthropic-beta")
                .and_then(|v| v.to_str().ok())
                .and_then(validate_anthropic_beta),
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

/// `anthropic-beta` 值白名单：非空、ASCII 可见字符（0x21..=0x7E）且
/// 长度 ≤ [`ANTHROPIC_BETA_MAX_LEN`]；非法值 warn 后丢弃（None）。
fn validate_anthropic_beta(value: &str) -> Option<String> {
    let valid = !value.is_empty()
        && value.len() <= ANTHROPIC_BETA_MAX_LEN
        && value.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if !valid {
        tracing::warn!(
            header = "anthropic-beta",
            "invalid anthropic-beta header value dropped"
        );
        return None;
    }
    Some(value.to_string())
}

/// messages 前缀兜底会话键 — 同前缀的对话粘同一 vendor。
/// 输入为 `serde_json::to_string(messages[0])` 的前 2KB（字符边界安全
/// 截断），SHA256 hex 截前 16 字符。
/// 截断会话键到 [`SESSION_KEY_MAX_BYTES`]（按 UTF-8 字符边界安全截断）。
fn truncate_session_key(raw: &str) -> String {
    if raw.len() <= SESSION_KEY_MAX_BYTES {
        return raw.to_string();
    }
    let mut take = SESSION_KEY_MAX_BYTES;
    while take > 0 && !raw.is_char_boundary(take) {
        take -= 1;
    }
    tracing::warn!("session key header exceeds {SESSION_KEY_MAX_BYTES} bytes; truncated to {take}");
    raw[..take].to_string()
}

/// 亲和决策解释块（`POST /admin/route/explain` 的 `affinity` 字段；
/// R-affinity-004）。纯只读：`peek` 不续期、stick 记账只读，不改任何状态。
///
/// `decision` 四态机器码（由调用方区分「没接线」与「这次请求没带会话」）：
/// - `hit`：绑定存在（带 `vendor`）
/// - `miss`：表已接线、给了会话键，但没有绑定
/// - `no_session`：表已接线、模式未关，但请求没带 `session_id`
/// - `disabled`：亲和未接线——`[routing.affinity] mode = "off"` 或亲和表未装配
///
/// `worth_tokens` / `ttl_secs`：最后一次 settle 的粘性记账（且其 vendor 与
/// 当前绑定一致）；无记账（legacy 模式 / 未结算 / 记账行已换 vendor）为
/// `null`。`ttl_secs` 是**绑定键**的剩余存活秒数（与 `peek` 同源），不是记账
/// 快照——记账行的 `expires_at` 不会随读路径变化。
///
/// `tenant`：`affinity_tenant_scoped` 开启时由调用方传 API key 摘要（与路由
/// 同源）；关闭时一律 legacy 键空间。
pub async fn affinity_decision_block(
    state: &crate::api::state::ApiState,
    session_id: Option<&str>,
    tenant: Option<&str>,
) -> Value {
    use crate::config::AffinityMode;
    let mode = AffinityMode::from_section(&state.config.read().routing.affinity);
    let (decision, worth, ttl) = match (state.affinity.as_ref(), session_id) {
        (Some(table), Some(session)) if mode != AffinityMode::Off => {
            match table.peek(tenant, session).await {
                Some(vendor) => {
                    // TTL 取亲和表侧的真实剩余（不是记账行快照）
                    let ttl = table
                        .remaining_ttl(tenant, session)
                        .await
                        .map(|d| d.as_secs());
                    // 记账只在 vendor 与当前绑定一致时回填——记账表按（租户,
                    // 会话）唯一，绑定可能在两次记账之间换过 vendor
                    let worth = state
                        .affinity_writer
                        .as_ref()
                        .and_then(|w| w.last_stick_for(tenant, session, &vendor));
                    (
                        serde_json::json!({"decision": "hit", "vendor": vendor}),
                        worth,
                        ttl,
                    )
                }
                None => (serde_json::json!({"decision": "miss"}), None, None),
            }
        }
        (Some(_), None) if mode != AffinityMode::Off => {
            (serde_json::json!({"decision": "no_session"}), None, None)
        }
        _ => (serde_json::json!({"decision": "disabled"}), None, None),
    };
    let mut block = decision;
    block["worth_tokens"] = serde_json::json!(worth);
    block["ttl_secs"] = serde_json::json!(ttl);
    block
}

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

    /// 超长会话键截到 255 字节（直接测截断函数——HeaderMap 自身会拒收
    /// 超长头值，无法经头部路径构造该用例）。
    #[test]
    fn truncates_overlong_session_key() {
        let long = "a".repeat(400);
        let key = super::truncate_session_key(&long);
        assert_eq!(key.len(), SESSION_KEY_MAX_BYTES, "必须截到上限");

        // 多字节边界：200 个汉字 = 600 字节，截断点必须落在字符边界
        let cjk = "字".repeat(200);
        let key = super::truncate_session_key(&cjk);
        assert!(key.len() <= SESSION_KEY_MAX_BYTES);
        assert!(key.chars().all(|c| c == '字'), "不得产生半个字符");

        // 未超限原样返回
        assert_eq!(super::truncate_session_key("short"), "short");
    }

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

    // tests：X-Weight 请求级权重解析

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

    // tests：anthropic-beta 白名单

    /// 合法 beta 值原样提取（含逗号多值）；缺失 → None。
    #[test]
    fn anthropic_beta_parsed_when_visible_ascii() {
        let mut h = HeaderMap::new();
        h.insert(
            "anthropic-beta",
            "extended-thinking-2025-01-01,prompt-caching-2024-07-31"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            RoutingHints::from_headers(&h).anthropic_beta.as_deref(),
            Some("extended-thinking-2025-01-01,prompt-caching-2024-07-31")
        );
        assert_eq!(
            RoutingHints::from_headers(&HeaderMap::new()).anthropic_beta,
            None
        );
    }

    /// 非法值（含空格 / 非 ASCII / 超长）丢弃为 None；4096 边界合法。
    #[test]
    fn anthropic_beta_rejects_invalid_values() {
        let mut h = HeaderMap::new();
        h.insert("anthropic-beta", "bad value".parse().unwrap()); // 空格非可见字符
        assert_eq!(RoutingHints::from_headers(&h).anthropic_beta, None);

        // 非 ASCII 与空串：validator 直测（HeaderValue 无法承载该形态）
        assert_eq!(validate_anthropic_beta("calf\u{00e9}"), None);
        assert_eq!(validate_anthropic_beta(""), None);

        let mut h = HeaderMap::new();
        let long = "a".repeat(ANTHROPIC_BETA_MAX_LEN + 1);
        h.insert("anthropic-beta", long.parse().unwrap());
        assert_eq!(RoutingHints::from_headers(&h).anthropic_beta, None);

        let mut h = HeaderMap::new();
        let edge = "a".repeat(ANTHROPIC_BETA_MAX_LEN);
        h.insert("anthropic-beta", edge.parse().unwrap());
        assert_eq!(
            RoutingHints::from_headers(&h)
                .anthropic_beta
                .map(|s| s.len()),
            Some(ANTHROPIC_BETA_MAX_LEN)
        );
    }
}
