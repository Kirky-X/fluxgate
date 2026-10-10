// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 租户身份辅助（头部键提取与摘要）的单一实现点。
//!
//! 下沉到 utils（原散在 middleware::auth 与 api::response_cache）：认证、
//! 响应缓存键、会话亲和与配额租户键共用同一提取/摘要逻辑——middleware
//! 不再反向依赖 crate::api，且「缓存键的租户维度 == 认证验证的 key 来源」
//! 由单实现保证。

use axum::http::HeaderMap;
use sha2::{Digest, Sha256};

/// 从请求头提取 API key：`Authorization: Bearer` 优先，`x-api-key` 兜底。
pub fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // 尝试从 Authorization header 提取
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            if let Some(stripped) = value.strip_prefix("Bearer ") {
                return Some(stripped.to_string());
            }
        }
    }

    // 尝试从 x-api-key header 提取
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(value) = key.to_str() {
            return Some(value.to_string());
        }
    }

    None
}

/// 调用方 API key 的 SHA256 摘要（响应缓存键/亲和表/配额账本的租户维度）。
/// 认证关闭/无 key 的请求统一归入 `anonymous` 租户。
pub fn api_key_digest(headers: &HeaderMap) -> String {
    match extract_api_key(headers) {
        Some(key) => hex::encode(Sha256::digest(key.as_bytes())),
        None => hex::encode(Sha256::digest(b"anonymous")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    /// Bearer 优先于 x-api-key；缺省归 anonymous；摘要与 sha256sum 同值
    ///（64 位小写 hex）。
    #[test]
    fn extraction_precedence_and_anonymous_fallback() {
        assert_eq!(
            extract_api_key(&headers(&[
                ("authorization", "Bearer abc"),
                ("x-api-key", "xyz"),
            ]))
            .as_deref(),
            Some("abc"),
            "Bearer 优先"
        );
        assert_eq!(
            extract_api_key(&headers(&[("x-api-key", "xyz")])).as_deref(),
            Some("xyz")
        );
        assert_eq!(extract_api_key(&headers(&[])), None);
        assert_eq!(
            api_key_digest(&headers(&[])),
            hex::encode(Sha256::digest(b"anonymous"))
        );
        assert_eq!(
            api_key_digest(&headers(&[("authorization", "Bearer abc")])),
            hex::encode(Sha256::digest(b"abc"))
        );
    }
}
