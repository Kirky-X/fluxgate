// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OIDC 换短时网关键（`oidc` feature 门控，auth-and-masking R-auth-002）。
//!
//! Bearer JWT 命中 `[auth.oidc_rules]` issuer 规则时：JWKS 按 kid 验签
//! （RS256/ES256；缓存 TTL 10 分钟，失败沿用旧缓存至 24h 上限）→
//! audience/claims **精确匹配** → 母键仍在位校验 → sub+issuer 派生会话
//! 身份、签发进程内 TTL 短时凭据（[`crate::utils::oidc_keys`] 内存表，
//! 不落盘、重启失效），继承母键 tier 与全部限制。短时凭据不出现在任何
//! 日志（本模块零 tracing 输出凭据值）。
//!
//! 无规则时认证中间件不进入本路径（行为与今日逐字节一致，R-auth-001）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::{AuthSection, OidcRuleConfig};

/// JWKS 缓存刷新间隔：条目老化超此值即尝试重拉（成功则续期）。
const JWKS_REFRESH_SECS: u64 = 600;
/// JWKS 拉取失败后旧缓存的可用上限（厂商抖动容忍窗）。
const JWKS_STALE_MAX_SECS: u64 = 86_400;
/// 单次 JWKS 拉取超时。
const JWKS_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// 允许的验签算法闭集（R-auth-002：RS256/ES256；其余一律拒绝）。
const ALLOWED_ALGS: &[jsonwebtoken::Algorithm] = &[
    jsonwebtoken::Algorithm::RS256,
    jsonwebtoken::Algorithm::ES256,
];

/// 验签拒绝原因（认证中间件统一按既有 401 形状回复，原因仅观测用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OidcRejection {
    /// 非 JWT 形态（无三段结构）
    NotJwt,
    /// 无命中 issuer 规则
    NoRule,
    /// JWKS 拉取失败且无可用缓存
    JwksUnavailable,
    /// 验签/aud/claims/exp 校验失败
    VerifyFailed,
    /// 母键已删除/禁用（规则立即失效）
    MotherKeyMissing,
}

/// 验签成功产物：短时凭据（已登记 [`crate::utils::oidc_keys`] 别名）与
/// 母键摘要（观测面；凭据值禁止入日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcIdentity {
    pub credential: String,
    pub mother_hash: String,
    /// 短时键过期时刻（Unix 秒）
    pub expires_unix: i64,
}

/// 是否为 JWT 形态（三段 base64 分隔；预筛防普通 key 误入验签路径）。
pub fn is_jwt_shape(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty())
}

/// issuer 规则匹配单点：未验证 claims 的 iss 与规则 issuer 精确相等
///（insecure_decode 只做路由预筛，真正的信任边界在验签）。
fn rule_for<'a>(
    token: &str,
    rules: &'a [OidcRuleConfig],
) -> Result<&'a OidcRuleConfig, OidcRejection> {
    let peek = jsonwebtoken::dangerous::insecure_decode_claims::<Value>(token)
        .map_err(|_| OidcRejection::NotJwt)?;
    let issuer = peek
        .get("iss")
        .and_then(Value::as_str)
        .ok_or(OidcRejection::VerifyFailed)?;
    rules
        .iter()
        .find(|rule| rule.issuer.trim() == issuer)
        .ok_or(OidcRejection::NoRule)
}

/// 母键在位校验（R-auth-002「母键删除/禁用即失效」每请求现读热配置）。
fn mother_key_present(auth: &AuthSection, hash: &str) -> bool {
    auth.enabled
        && (auth.api_keys.iter().any(|stored| stored.trim() == hash)
            || auth.keys.iter().any(|entry| entry.hash.trim() == hash))
}

/// claims 精确匹配（R-auth-001 红线运行时半边）：规则声明逐条要求 token
/// claims 存在同名**字符串**且字面相等；非字符串 claim 一律失配。
fn claims_exact_match(rule: &OidcRuleConfig, token_claims: &Value) -> bool {
    rule.claims.iter().all(|(key, expected)| {
        token_claims
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|actual| actual == expected)
    })
}

/// OIDC 验签与短时键签发（auth 中间件入口）：返回已登记别名的短时凭据。
pub async fn authenticate(token: &str, auth: &AuthSection) -> Result<OidcIdentity, OidcRejection> {
    let rules = auth
        .oidc_rules
        .as_ref()
        .map(|section| section.rules.as_slice())
        .unwrap_or(&[]);
    if rules.is_empty() {
        return Err(OidcRejection::NoRule);
    }
    let rule = rule_for(token, rules)?;

    let header = jsonwebtoken::decode_header(token).map_err(|_| OidcRejection::VerifyFailed)?;
    if !ALLOWED_ALGS.contains(&header.alg) {
        return Err(OidcRejection::VerifyFailed);
    }
    let kid = header.kid.clone().ok_or(OidcRejection::VerifyFailed)?;
    let jwks = jwks_for_issuer(rule).await?;
    let jwk = jwks.find(&kid).ok_or(OidcRejection::VerifyFailed)?;
    let decoding_key =
        jsonwebtoken::DecodingKey::from_jwk(jwk).map_err(|_| OidcRejection::VerifyFailed)?;
    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_issuer(&[rule.issuer.trim()]);
    validation.set_audience(&[rule.audience.trim()]);
    let data = jsonwebtoken::decode::<Value>(token, &decoding_key, &validation)
        .map_err(|_| OidcRejection::VerifyFailed)?;
    if !claims_exact_match(rule, &data.claims) {
        return Err(OidcRejection::VerifyFailed);
    }
    let sub = data
        .claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|sub| !sub.is_empty())
        .ok_or(OidcRejection::VerifyFailed)?;

    let mother = rule.bind_key_hash.trim();
    if !mother_key_present(auth, mother) {
        return Err(OidcRejection::MotherKeyMissing);
    }

    // sub+issuer 派生会话身份（不可逆摘要做表键，token claims 不落表）
    let session_id = {
        use sha2::{Digest, Sha256};
        let digest =
            Sha256::digest(format!("fluxweir-oidc\0{}\0{}", rule.issuer.trim(), sub).as_bytes());
        hex::encode(digest)
    };
    let ttl_secs = rule.ttl_secs.min(86_400) as i64;
    let credential = crate::utils::oidc_keys::register(
        &session_id,
        || {
            // 双 uuid v4（122-bit 随机 ×2）：短时凭据不可由 issuer+sub 推算
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        },
        mother,
        ttl_secs,
    )
    .ok_or(OidcRejection::JwksUnavailable)?;
    Ok(OidcIdentity {
        credential,
        mother_hash: mother.to_string(),
        expires_unix: chrono::Utc::now().timestamp() + ttl_secs,
    })
}

/// issuer → JWKS 缓存条目：内容 + 拉取/成功时刻（Instant 单调钟，重启即空）。
#[derive(Clone)]
struct JwksEntry {
    jwks: std::sync::Arc<jsonwebtoken::jwk::JwkSet>,
    fetched_at: Instant,
    last_success: Instant,
}

static JWKS_CACHE: Mutex<Option<HashMap<String, JwksEntry>>> = Mutex::new(None);

fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(JWKS_FETCH_TIMEOUT)
                .build()
                .expect("JWKS client 构造失败")
        })
        .clone()
}

async fn fetch_jwks(issuer: &str) -> Result<jsonwebtoken::jwk::JwkSet, OidcRejection> {
    let url = format!("{}/.well-known/jwks.json", issuer.trim_end_matches('/'));
    let response = http_client()
        .get(&url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|_| OidcRejection::JwksUnavailable)?;
    response
        .json::<jsonwebtoken::jwk::JwkSet>()
        .await
        .map_err(|_| OidcRejection::JwksUnavailable)
}

/// JWKS 按 issuer 取用（缓存 TTL 10 分钟；失败沿用旧缓存至 24h）：
/// 缓存未老化直接用；老化后重拉，成功换新，失败在 24h 窗内沿用旧缓存。
async fn jwks_for_issuer(
    rule: &OidcRuleConfig,
) -> Result<std::sync::Arc<jsonwebtoken::jwk::JwkSet>, OidcRejection> {
    let issuer = rule.issuer.trim().to_string();
    {
        let guard = JWKS_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = guard.as_ref().and_then(|map| map.get(&issuer)) {
            if entry.fetched_at.elapsed() < Duration::from_secs(JWKS_REFRESH_SECS) {
                return Ok(entry.jwks.clone());
            }
        }
    }
    // 拉取在锁外（网络 IO 不持锁；并发双拉以最后完成者为准——幂等）
    match fetch_jwks(&issuer).await {
        Ok(jwks) => {
            let jwks = std::sync::Arc::new(jwks);
            let entry = JwksEntry {
                jwks: jwks.clone(),
                fetched_at: Instant::now(),
                last_success: Instant::now(),
            };
            let mut guard = JWKS_CACHE.lock().unwrap_or_else(|p| p.into_inner());
            guard.get_or_insert_with(HashMap::new).insert(issuer, entry);
            Ok(jwks)
        }
        Err(rejection) => {
            let guard = JWKS_CACHE.lock().unwrap_or_else(|p| p.into_inner());
            let stale = guard
                .as_ref()
                .and_then(|map| map.get(&issuer))
                .filter(|entry| {
                    entry.last_success.elapsed() < Duration::from_secs(JWKS_STALE_MAX_SECS)
                });
            match stale {
                Some(entry) => Ok(entry.jwks.clone()),
                None => Err(rejection),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OidcRuleConfig, OidcRulesSection};
    use std::collections::HashMap;

    // OIDC 测试密钥材料（P-256 固定测试对，仅测试面使用）。私钥按 RFC 5959
    // 最小 PKCS#8 编码（named curve + SEC1 内层含公点），测试夹具无真实效力；
    // 公点进 JWKS 由 mockito 本地服务返回。
    const TEST_EC_PRIVATE_PKCS8_DER: &[u8] = &[
        0x30, 0x81, 0x87, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d,
        0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x04, 0x6d, 0x30,
        0x6b, 0x02, 0x01, 0x01, 0x04, 0x20, 0x58, 0x0e, 0x90, 0xe8, 0x85, 0x73, 0xf9, 0xf4, 0x6c,
        0x8d, 0x0d, 0xce, 0xd9, 0x20, 0x4a, 0x6e, 0x5d, 0x09, 0x55, 0xa1, 0xa7, 0x87, 0x06, 0xf0,
        0xe1, 0x28, 0x54, 0x1b, 0xdc, 0x68, 0xb1, 0x7e, 0xa1, 0x44, 0x03, 0x42, 0x00, 0x04, 0xa8,
        0x72, 0xa5, 0xdc, 0x11, 0xc5, 0x4b, 0x7c, 0x67, 0xae, 0xba, 0x59, 0x7f, 0xc6, 0xd5, 0x6b,
        0x67, 0x56, 0x55, 0x66, 0x38, 0xce, 0x42, 0x13, 0xd1, 0x8d, 0x06, 0x3d, 0xb1, 0x95, 0x5c,
        0xb7, 0xb9, 0xb2, 0x63, 0x07, 0x8c, 0x55, 0x97, 0x4d, 0xc7, 0x4a, 0x02, 0xec, 0x1d, 0x64,
        0x66, 0xf2, 0x7a, 0x8b, 0xa4, 0x07, 0xe5, 0xdd, 0x4a, 0xc2, 0x65, 0x61, 0x1c, 0x1d, 0x18,
        0x86, 0x03, 0x6a,
    ];

    /// 测试 JWKS 文档（kid=test-key-1，公点 x/y base64url）——mockito 返回体。
    const TEST_JWKS_JSON: &str = r#"{
  "keys": [
    {
      "kty": "EC",
      "crv": "P-256",
      "kid": "test-key-1",
      "x": "qHKl3BHFS3xnrrpZf8bVa2dWVWY4zkIT0Y0GPbGVXLc",
      "y": "ubJjB4xVl03HSgLsHWRm8nqLpAfl3UrCZWEcHRiGA2o"
    }
  ]
}"#;

    /// sha256("test-good-key")。
    const MOTHER_HASH: &str = "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";

    fn rule(issuer: &str) -> OidcRuleConfig {
        OidcRuleConfig {
            issuer: issuer.to_string(),
            audience: "gateway-client".to_string(),
            claims: HashMap::new(),
            bind_key_hash: MOTHER_HASH.to_string(),
            ttl_secs: 3600,
        }
    }

    fn auth_section(rules: Vec<OidcRuleConfig>) -> AuthSection {
        AuthSection {
            enabled: true,
            api_keys: vec![MOTHER_HASH.to_string()],
            header: Default::default(),
            keys: vec![],
            oidc_rules: Some(OidcRulesSection { rules }),
        }
    }

    /// ES256 测试签名（kid=test-key-1，与 TEST_JWKS_JSON 对应）。
    fn sign(claims: Value) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("test-key-1".to_string());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_ec_der(TEST_EC_PRIVATE_PKCS8_DER),
        )
        .expect("测试签名失败")
    }

    fn claims(issuer: &str, exp_offset: i64, extra: Value) -> Value {
        let mut claims = serde_json::json!({
            "iss": issuer,
            "aud": "gateway-client",
            "sub": "alice",
            "exp": chrono::Utc::now().timestamp() + exp_offset,
        });
        if let (Some(base), Some(extra)) = (claims.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        claims
    }

    /// 起本地 JWKS 服务（mockito）：规则 issuer = 服务地址，公钥为测试对。
    async fn jwks_server() -> mockito::ServerGuard {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/.well-known/jwks.json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;
        server
    }

    #[test]
    fn jwt_shape_predates_verification() {
        assert!(is_jwt_shape("eyJh.eyJi.c2ln"));
        assert!(!is_jwt_shape("plain-static-key"));
        assert!(!is_jwt_shape("a..c"), "空段非 JWT 形态");
        assert!(!is_jwt_shape("a.b"), "两段非 JWT 形态");
        // 短时凭据（64 hex 无点）不入 JWT 形态——重放凭据走不到验签路径
        assert!(!is_jwt_shape(
            "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303"
        ));
    }

    /// 验签通过 → 签发短时凭据：别名解析到母键摘要，api_key_digest 与
    /// 母键同视角（R-auth-002 继承语义的单测面）。
    #[tokio::test]
    async fn authenticated_jwt_issues_short_key_bound_to_mother() {
        let server = jwks_server().await;
        let issuer = server.url();
        let auth = auth_section(vec![rule(&issuer)]);
        let token = sign(claims(&issuer, 600, serde_json::json!({})));

        let identity = authenticate(&token, &auth)
            .await
            .expect("合法 JWT 必须签发成功");
        assert_eq!(identity.mother_hash, MOTHER_HASH);
        assert!(
            crate::utils::oidc_keys::resolve(&identity.credential).as_deref() == Some(MOTHER_HASH),
            "短时凭据别名解析到母键摘要"
        );
        assert!(identity.expires_unix > chrono::Utc::now().timestamp());
        // 下游租户视角：Bearer 短时凭据的摘要 = 母键摘要
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {}", identity.credential)).unwrap(),
        );
        assert_eq!(
            crate::utils::tenant::api_key_digest(&headers),
            MOTHER_HASH,
            "短时凭据的租户摘要 = 母键摘要（配额/预算/亲和继承）"
        );
        // 重放防护：凭据直接呈报走静态校验必拒（别名不参与 validate_key）
        assert!(
            !crate::middleware::auth::ApiAuthState::from_hashes(vec![MOTHER_HASH.to_string()])
                .validate_key(&identity.credential),
            "短时凭据重放不得通过静态校验"
        );
    }

    /// aud 不匹配与额外 claims 不匹配（精确匹配红线运行时半边）拒绝。
    #[tokio::test]
    async fn audience_and_claims_mismatch_rejected() {
        let server = jwks_server().await;
        let issuer = server.url();

        let mut rule_with_claims = rule(&issuer);
        rule_with_claims
            .claims
            .insert("repository".to_string(), "org/repo".to_string());
        let auth = auth_section(vec![rule_with_claims]);

        // aud 不匹配
        let mut bad_aud = claims(&issuer, 600, serde_json::json!({"repository": "org/repo"}));
        bad_aud["aud"] = serde_json::json!("other-client");
        assert_eq!(
            authenticate(&sign(bad_aud), &auth).await,
            Err(OidcRejection::VerifyFailed),
            "aud 不匹配必须拒绝"
        );

        // claims 值不匹配（前缀型冒充 org/repo-x 不放行）
        let prefix_bomb = claims(
            &issuer,
            600,
            serde_json::json!({"repository": "org/repo-x", "sub": "alice"}),
        );
        assert_eq!(
            authenticate(&sign(prefix_bomb), &auth).await,
            Err(OidcRejection::VerifyFailed),
            "claims 前缀冒充必须拒绝（精确匹配红线）"
        );

        // 全匹配放行
        let good = claims(&issuer, 600, serde_json::json!({"repository": "org/repo"}));
        assert!(
            authenticate(&sign(good), &auth).await.is_ok(),
            "aud+claims 全匹配放行"
        );
    }

    /// 过期 JWT 拒绝（exp 校验）。
    #[tokio::test]
    async fn expired_jwt_rejected() {
        let server = jwks_server().await;
        let issuer = server.url();
        let auth = auth_section(vec![rule(&issuer)]);
        // exp 远超默认 leeway（60s），确保真正过期
        let expired = sign(claims(&issuer, -3600, serde_json::json!({})));
        assert_eq!(
            authenticate(&expired, &auth).await,
            Err(OidcRejection::VerifyFailed),
            "过期 JWT 必须拒绝"
        );
    }

    /// 母键删除/禁用 → 规则立即失效（MotherKeyMissing）。
    #[tokio::test]
    async fn mother_key_removal_invalidates_rule() {
        let server = jwks_server().await;
        let issuer = server.url();
        let token = sign(claims(&issuer, 600, serde_json::json!({})));

        let mut auth = auth_section(vec![rule(&issuer)]);
        auth.api_keys.clear();
        auth.keys.clear();
        assert_eq!(
            authenticate(&token, &auth).await,
            Err(OidcRejection::MotherKeyMissing),
            "母键删除后立即失效"
        );

        // 认证关闭同语义（disabled = 母键面不在位）
        let mut disabled = auth_section(vec![rule(&issuer)]);
        disabled.enabled = false;
        assert_eq!(
            authenticate(&token, &disabled).await,
            Err(OidcRejection::MotherKeyMissing)
        );
    }

    /// 未知 kid / 无规则 issuer → 拒绝（JWKS 缓存按 issuer 隔离）。
    #[tokio::test]
    async fn unknown_kid_and_issuer_rejected() {
        let server = jwks_server().await;
        let issuer = server.url();
        let auth = auth_section(vec![rule(&issuer)]);

        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("ghost-kid".to_string());
        let ghost = jsonwebtoken::encode(
            &header,
            &claims(&issuer, 600, serde_json::json!({})),
            &jsonwebtoken::EncodingKey::from_ec_der(TEST_EC_PRIVATE_PKCS8_DER),
        )
        .unwrap();
        assert_eq!(
            authenticate(&ghost, &auth).await,
            Err(OidcRejection::VerifyFailed),
            "未知 kid 必须拒绝"
        );

        // issuer 无规则命中
        let stranger = sign(claims("https://evil.example", 600, serde_json::json!({})));
        assert_eq!(
            authenticate(&stranger, &auth).await,
            Err(OidcRejection::NoRule),
            "未登记 issuer 必须拒绝"
        );
    }

    /// 非允许算法（HS256）拒绝（闭集 RS256/ES256）。
    #[tokio::test]
    async fn disallowed_algorithm_rejected() {
        let server = jwks_server().await;
        let issuer = server.url();
        let auth = auth_section(vec![rule(&issuer)]);
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = Some("test-key-1".to_string());
        let hs = jsonwebtoken::encode(
            &header,
            &claims(&issuer, 600, serde_json::json!({})),
            &jsonwebtoken::EncodingKey::from_secret(b"symmetric-secret"),
        )
        .unwrap();
        assert_eq!(
            authenticate(&hs, &auth).await,
            Err(OidcRejection::VerifyFailed),
            "HS256 不在允许算法闭集"
        );
    }
}
