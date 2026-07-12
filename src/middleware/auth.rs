// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use serde_json::json;
use std::sync::Arc;

/// API Key 认证状态
#[derive(Clone)]
pub struct ApiAuthState {
    /// 有效的 API keys
    valid_keys: Arc<std::collections::HashSet<String>>,
    /// 是否启用认证
    enabled: bool,
}

impl ApiAuthState {
    /// 创建新的认证状态
    pub fn new(enabled: bool) -> Self {
        Self {
            valid_keys: Arc::new(std::collections::HashSet::new()),
            enabled,
        }
    }

    /// 添加有效的 API key
    pub fn add_api_key(&mut self, key: String) {
        Arc::make_mut(&mut self.valid_keys).insert(key);
    }

    /// 验证 API key
    pub fn validate_key(&self, key: &str) -> bool {
        if !self.enabled {
            return true;
        }
        self.valid_keys.contains(key)
    }
}

/// 从请求头提取 API key
fn extract_api_key(headers: &axum::http::HeaderMap) -> Option<String> {
    // 尝试从 Authorization header 提取
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            if value.starts_with("Bearer ") {
                return Some(value[7..].to_string());
            }
            return Some(value.to_string());
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

/// API Key 认证中间件
pub async fn api_auth_middleware(
    request: Request<Body>,
    next: Next,
    auth_state: Arc<ApiAuthState>,
) -> Result<Response, StatusCode> {
    if !auth_state.enabled {
        return Ok(next.run(request).await);
    }

    // 排除健康检查端点
    let path = request.uri().path();
    if path == "/health" || path == "/" {
        return Ok(next.run(request).await);
    }

    // 排除 Admin 端点 (内部使用)
    if path.starts_with("/admin/") {
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
                            "message": "Missing API key",
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
                        "message": "Invalid API key",
                        "type": "authentication_error"
                    }
                })
                .to_string(),
            ))
            .unwrap());
    }

    Ok(next.run(request).await)
}
