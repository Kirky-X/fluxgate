// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::config::{AuthSection, AuthTier, ConfigManager};
use crate::i18n::t;

/// 计算 SHA256 hex 摘要（64 个小写 hex 字符）
fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(digest)
}

/// 常量时间字节比较 — 消除摘要比较的时序侧信道：不做长度提前返回（O(1) 短路
/// 会泄漏长度），两侧 pad 到最大长度后全程扫描，长度判等折进结果防误判。
/// 调用方传入恒 64 字符的 SHA256 hex：≤64 走栈缓冲，更长走堆缓冲兜底。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let max = a.len().max(b.len());
    if max <= 64 {
        let mut padded_a = [0u8; 64];
        let mut padded_b = [0u8; 64];
        padded_a[..a.len()].copy_from_slice(a);
        padded_b[..b.len()].copy_from_slice(b);
        bool::from(padded_a[..max].ct_eq(&padded_b[..max])) & (a.len() == b.len())
    } else {
        let mut padded_a = vec![0u8; max];
        padded_a[..a.len()].copy_from_slice(a);
        let mut padded_b = vec![0u8; max];
        padded_b[..b.len()].copy_from_slice(b);
        bool::from(padded_a.ct_eq(&padded_b)) & (a.len() == b.len())
    }
}

/// API Key 认证状态 — 内部只存 SHA256 hex 摘要不存明文；校验时对请求 key
/// 现场摘要后做常量时间比较。
#[derive(Clone)]
pub struct ApiAuthState {
    /// 有效的 API key 摘要（SHA256 hex）
    valid_keys: Arc<std::collections::HashSet<String>>,
    /// key 摘要 → 档位（[auth].keys 子表声明；未声明的 key
    /// 缺省 elastic）。准入层经 [`Self::tier_for_key`] 查询。
    tiers: Arc<std::collections::HashMap<String, AuthTier>>,
    /// 是否启用认证
    enabled: bool,
}

impl ApiAuthState {
    /// 创建新的认证状态
    pub fn new(enabled: bool) -> Self {
        Self {
            valid_keys: Arc::new(std::collections::HashSet::new()),
            tiers: Arc::new(std::collections::HashMap::new()),
            enabled,
        }
    }

    /// 从 SHA256 hex 摘要列表构造（`[auth]`.api_keys 的存储形态）
    pub fn from_hashes(hashes: Vec<String>) -> Self {
        let mut state = Self::new(true);
        for h in hashes {
            state.add_hash(h);
        }
        state
    }

    /// 从配置 `[auth]` 段构造
    pub fn from_section(section: &AuthSection) -> Self {
        let mut state = Self::new(section.enabled);
        for h in &section.api_keys {
            state.add_hash(h.clone());
        }
        // 档位表（[[auth.keys]]；缺省空表 → 全部 elastic）
        let mut tiers = std::collections::HashMap::new();
        for entry in &section.keys {
            tiers.insert(entry.hash.clone(), entry.tier);
        }
        state.tiers = Arc::new(tiers);
        state
    }

    /// 添加摘要形式的 key（原样入库，不做二次摘要）
    fn add_hash(&mut self, hash: String) {
        Arc::make_mut(&mut self.valid_keys).insert(hash);
    }

    /// 添加明文 API key — 现场算 SHA256 摘要后存储，明文即刻丢弃
    pub fn add_api_key(&mut self, key: String) {
        self.add_hash(sha256_hex(&key));
    }

    /// 验证 API key：现场摘要后与存储的摘要做常量时间比较
    pub fn validate_key(&self, key: &str) -> bool {
        if !self.enabled {
            return true;
        }
        let candidate = sha256_hex(key);
        self.valid_keys
            .iter()
            .any(|stored| constant_time_eq(stored, &candidate))
    }

    /// key 档位 — 现场摘要后与 `[auth]`.keys 表做常量时间比较；未声明 → 缺省档
    /// elastic，认证关闭时表为空 → 恒 elastic。
    pub fn tier_for_key(&self, key: &str) -> AuthTier {
        let candidate = sha256_hex(key);
        self.tiers
            .iter()
            .find(|(stored, _)| constant_time_eq(stored, &candidate))
            .map(|(_, tier)| *tier)
            .unwrap_or(AuthTier::Elastic)
    }
}

/// 从请求头提取 API key — 实现下沉 [`crate::utils::tenant::extract_api_key`]，
/// 此处 re-export 维持既有引用路径。
pub(crate) use crate::utils::tenant::extract_api_key;

/// API Key 认证中间件（from_fn_with_state 挂载形态）— State 是 `Arc<ConfigManager>`：
/// 每请求从热配置现读 `[auth]` 段构造 ApiAuthState，key 吊销/轮换与 enabled 开关热生效。
/// 第三参数是裸 `Arc<ApiAuthState>`（便于直调/测试），此包装补齐该形态。
pub async fn auth_middleware(
    axum::extract::State(config_manager): axum::extract::State<Arc<ConfigManager>>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_state = match config_manager.read().auth.as_ref() {
        Some(section) => ApiAuthState::from_section(section),
        None => ApiAuthState::new(false),
    };
    api_auth_middleware(request, next, Arc::new(auth_state)).await
}

/// API Key 认证中间件
pub async fn api_auth_middleware(
    mut request: Request<Body>,
    next: Next,
    auth_state: Arc<ApiAuthState>,
) -> Result<Response, StatusCode> {
    // Gemini SDK 可用 `?key=` 传凭证：认证/租户摘要前注入到标准头，
    // 仅限 `/v1beta/` 路径（其它路径不接受 query key，避免进入代理日志）
    inject_gemini_query_key(&mut request);
    if !auth_state.enabled {
        return Ok(next.run(request).await);
    }

    // 豁免端点：健康检查/就绪探针/指标抓取无调用方 key，认证无从谈起；
    // 文档面（/api-docs/*、/swagger-ui/*）开关本身即访问门（默认关闭）。
    let path = request.uri().path();
    if path == "/health"
        || path == "/api/v1/health"
        || path == "/"
        || path == "/readyz"
        || path == "/metrics"
        || path.starts_with("/api-docs/")
        || path == "/api-docs"
        || path.starts_with("/swagger-ui/")
        || path == "/swagger-ui"
    {
        return Ok(next.run(request).await);
    }

    let api_key = match extract_api_key(request.headers()) {
        Some(key) => key,
        None => {
            return Ok(unauthorized_response(path, t("middleware-missing-api-key")));
        }
    };

    if !auth_state.validate_key(&api_key) {
        return Ok(unauthorized_response(path, t("middleware-invalid-api-key")));
    }

    Ok(next.run(request).await)
}

/// Gemini 路径的 query key 注入：仅在无任何凭证头时生效，`key` 经
/// 最小 percent-decode 后写入 `x-goog-api-key`。
fn inject_gemini_query_key(request: &mut Request<Body>) {
    if !request.uri().path().starts_with("/v1beta/") {
        return;
    }
    let headers = request.headers();
    if headers.contains_key("authorization")
        || headers.contains_key("x-api-key")
        || headers.contains_key("x-goog-api-key")
    {
        return;
    }
    let Some(key) = request
        .uri()
        .query()
        .and_then(|query| query_param(query, "key"))
    else {
        return;
    };
    if key.is_empty() {
        return;
    }
    if let Ok(value) = axum::http::HeaderValue::from_str(&key) {
        request
            .headers_mut()
            .insert(axum::http::HeaderName::from_static("x-goog-api-key"), value);
    }
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))
    })
}

/// 最小 percent-decode（仅 `%XX`；`+` 保持字面值，API key 以 `%2B` 传递）。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 认证失败：Gemini 路径返回其原生错误体，其它路径保持历史形状。
fn unauthorized_response(path: &str, message: String) -> Response {
    let body = if path.starts_with("/v1beta/") {
        json!({
            "error": {
                "code": 401,
                "message": t("middleware-gemini-unauthorized"),
                "status": "UNAUTHENTICATED",
            }
        })
    } else {
        json!({
            "error": {
                "message": message,
                "type": "authentication_error",
            }
        })
    };
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知明文/摘要对（`printf %%s "test-key-123" | sha256sum` 实测），
    /// 硬编码锁定摘要算法与编码格式
    const KNOWN_KEY: &str = "test-key-123";
    const KNOWN_HASH: &str = "625faa3fbbc3d2bd9d6ee7678d04cc5339cb33dc68d9b58451853d60046e226a";

    #[test]
    fn test_sha256_hex_matches_known_vector() {
        assert_eq!(sha256_hex(KNOWN_KEY), KNOWN_HASH);
    }

    #[test]
    fn test_add_api_key_stores_digest_not_plaintext() {
        let mut state = ApiAuthState::new(true);
        state.add_api_key(KNOWN_KEY.to_string());
        // 摘要入库：校验通过
        assert!(state.validate_key(KNOWN_KEY));
        // 内部集合不含明文
        assert!(
            !state.valid_keys.contains(KNOWN_KEY),
            "plaintext must never be stored"
        );
        assert!(state.valid_keys.contains(KNOWN_HASH));
    }

    #[test]
    fn test_from_hashes_accepts_config_digests() {
        let state = ApiAuthState::from_hashes(vec![KNOWN_HASH.to_string()]);
        assert!(state.validate_key(KNOWN_KEY), "plaintext of stored digest");
        assert!(!state.validate_key("wrong"));
    }

    #[test]
    fn test_validate_key_rejects_when_hash_mismatch() {
        let state = ApiAuthState::from_hashes(vec![KNOWN_HASH.to_string()]);
        // 摘要不同 → 拒绝
        assert!(!state.validate_key("another-key"));
        // 空串 → 摘要非空，拒绝
        assert!(!state.validate_key(""));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn test_constant_time_eq_length_independent() {
        // 长度不等恒 false，且不因 pad 补零产生误判（"ab\0" vs "ab"）。
        assert!(!constant_time_eq("ab\0", "ab"));
        assert!(!constant_time_eq("ab", "ab\0"));
        assert!(!constant_time_eq("ab\0c", "ab"));
        // 摘要场景：64 hex 与其截断形态（前后缀关系）
        assert!(!constant_time_eq(&KNOWN_HASH[..63], KNOWN_HASH));
        assert!(!constant_time_eq(KNOWN_HASH, &KNOWN_HASH[..63]));
        assert!(constant_time_eq(KNOWN_HASH, KNOWN_HASH));
    }

    #[test]
    fn test_from_section_maps_enabled_and_keys() {
        let section = AuthSection {
            enabled: true,
            api_keys: vec![KNOWN_HASH.to_string()],
            keys: vec![],
            header: Default::default(),
        };
        let state = ApiAuthState::from_section(&section);
        assert!(state.enabled);
        assert!(state.validate_key(KNOWN_KEY));

        let disabled = AuthSection {
            enabled: false,
            api_keys: vec![],
            keys: vec![],
            header: Default::default(),
        };
        let state = ApiAuthState::from_section(&disabled);
        assert!(!state.enabled);
        assert!(state.validate_key("anything"), "disabled → 放行");
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    #[test]
    fn gemini_query_key_is_injected_only_on_v1beta_paths() {
        let mut gemini =
            request("/v1beta/models/gemini-2.5-pro:generateContent?alt=sse&key=abc-123");
        inject_gemini_query_key(&mut gemini);
        assert_eq!(gemini.headers()["x-goog-api-key"], "abc-123");

        let mut other = request("/v1/chat/completions?key=abc-123");
        inject_gemini_query_key(&mut other);
        assert!(
            other.headers().get("x-goog-api-key").is_none(),
            "非 Gemini 路径不得接受 query key"
        );

        let mut header_wins = request("/v1beta/models/x:generateContent?key=query-key");
        header_wins.headers_mut().insert(
            "x-goog-api-key",
            axum::http::HeaderValue::from_static("header-key"),
        );
        inject_gemini_query_key(&mut header_wins);
        assert_eq!(header_wins.headers()["x-goog-api-key"], "header-key");
    }

    #[test]
    fn query_param_percent_decodes() {
        assert_eq!(
            query_param("a=1&key=sk%2Babc%3D", "key").as_deref(),
            Some("sk+abc=")
        );
        assert_eq!(query_param("alt=sse", "key"), None);
        assert_eq!(query_param("key=", "key").as_deref(), Some(""));
    }
}
