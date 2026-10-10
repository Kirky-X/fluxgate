// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

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

/// 常量时间字节比较 — 消除摘要比较的时序侧信道。
/// 长度不等不做提前返回（O(1) 短路会泄漏长度信息）：两侧 pad 到最大
/// 长度后经 [`ConstantTimeEq`] 全程扫描，执行时间只随最大长度变化。
/// pad 补零使含 NUL 的前后缀对逐字节相等，长度判等必须折进结果防止
/// 误判。调用方传入的都是 SHA256 hex（恒 64 字符）：≤64 走栈缓冲零
/// 分配（本函数在 validate_key/tier_for_key 的每请求热路径上），更长
/// 输入走堆缓冲防御兜底。
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

/// API Key 认证状态
///
/// 内部只存 SHA256 hex 摘要，不存明文：`add_api_key` 现场摘要后存储，
/// `from_hashes`/`from_section` 直接接受配置中的摘要
/// （`echo -n "key" | sha256sum`）。校验时对请求 key 现场算摘要后做
/// 常量时间比较。
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

    /// 从 SHA256 hex 摘要列表构造（[auth].api_keys 的存储形态）
    pub fn from_hashes(hashes: Vec<String>) -> Self {
        let mut state = Self::new(true);
        for h in hashes {
            state.add_hash(h);
        }
        state
    }

    /// 从配置 [auth] 段构造
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

    /// key 档位 — 现场摘要后与 [auth].keys 表做常量时间比较
    ///（复用 [`constant_time_eq`]，与 [`Self::validate_key`] 同一摘要
    /// 语义）；未声明 → 缺省档 elastic。认证关闭时表为空 → 恒 elastic。
    pub fn tier_for_key(&self, key: &str) -> AuthTier {
        let candidate = sha256_hex(key);
        self.tiers
            .iter()
            .find(|(stored, _)| constant_time_eq(stored, &candidate))
            .map(|(_, tier)| *tier)
            .unwrap_or(AuthTier::Elastic)
    }
}

/// 从请求头提取 API key — 实现已下沉 [`crate::utils::tenant::extract_api_key`]
///（认证/响应缓存/亲和/配额同源单实现），此处 re-export 维持 middleware
/// 内部的既有引用路径。
pub(crate) use crate::utils::tenant::extract_api_key;

/// API Key 认证中间件（from_fn_with_state 挂载形态）
///
/// State 是 `Arc<ConfigManager>`：每请求从热配置现读 [auth] 段构造
/// ApiAuthState，key 吊销/轮换与 enabled 开关热更新立即生效，无"返回
/// success 但继续用旧 key 集直到重启"的静默失效。开销为每请求一次
/// HashSet 分配 + 摘要串 clone（条数通常个位数），对秒级 LLM 请求可忽略。
///
/// `api_auth_middleware` 的第三参数是裸 `Arc<ApiAuthState>`（便于直调/测试）；
/// axum 的 `from_fn_with_state` 要求第三参数可由 state 提取，此包装补齐该形态。
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
    request: Request<Body>,
    next: Next,
    auth_state: Arc<ApiAuthState>,
) -> Result<Response, StatusCode> {
    if !auth_state.enabled {
        return Ok(next.run(request).await);
    }

    // 排除健康检查端点。
    // /api/v1/health 是实际注册路径：sdforge #[forge(version = "v1")] 宏会把
    // path 加 /api/v1 版本前缀（sdforge-macros lib.rs）。
    // 入站限流侧有另一份独立的健康检查白名单（含 /healthz、/ready、
    // /readyz、/metrics），维护于 src/api/router_builder.rs 的
    // is_quota_exempt_path，两处各自独立、语义不共享。
    // /readyz（就绪探针）与 /metrics（指标抓取）同样豁免——基础设施
    // 端点无调用方 key，认证无从谈起。
    // 文档面（/api-docs/*、/swagger-ui/*）同样豁免——开关本身即
    // 访问门（默认关闭），打开后浏览器可用性要求无 key 可达。
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

    // 提取并验证 API key
    let api_key = match extract_api_key(request.headers()) {
        Some(key) => key,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({
                        "error": {
                            "message": t("middleware-missing-api-key"),
                            "type": "authentication_error"
                        }
                    })
                    .to_string(),
                ))
                .unwrap());
        }
    };

    if !auth_state.validate_key(&api_key) {
        return Ok(Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("Content-Type", "application/json")
            .body(Body::from(
                json!({
                    "error": {
                        "message": t("middleware-invalid-api-key"),
                        "type": "authentication_error"
                    }
                })
                .to_string(),
            ))
            .unwrap());
    }

    Ok(next.run(request).await)
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
        // 长度不等恒 false，且不因 pad 补零产生误判：pad 使含 NUL 的
        // 前后缀对逐字节相等，长度判等必须折进结果（防止只 pad 不查
        // 长度的实现把 "ab\0"/"ab" 判成相等）
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
}
