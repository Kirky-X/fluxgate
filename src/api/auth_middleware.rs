//! Authentication middleware for NexusFlow
//!
//! Provides API Key and Bearer token authentication using sdforge's security module.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
    Json,
};
use serde_json::json;
use sdforge::security::{ApiKeyAuth, BearerAuth, AuthContext, AuthMetadata};
use std::sync::Arc;

/// Extract API key from Authorization header
fn extract_api_key(auth_header: &str, prefix: &str) -> Option<&str> {
    if auth_header.starts_with(prefix) {
        let key = &auth_header[prefix.len()..].trim();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

/// Create API Key authentication middleware
pub fn create_api_key_auth(api_keys: Vec<String>) -> Arc<ApiKeyAuth> {
    let auth = ApiKeyAuth::new();
    
    for key in api_keys {
        auth.add_key(key, vec!["default".to_string()]);
    }
    
    Arc::new(auth)
}

/// Create Bearer authentication middleware
pub fn create_bearer_auth(secret: String) -> Arc<BearerAuth> {
    Arc::new(BearerAuth::new(secret))
}

/// API Key authentication middleware
pub async fn api_key_auth_middleware(
    req: Request<Body>,
    next: Next,
    auth: Arc<ApiKeyAuth>,
    header_name: &str,
    header_prefix: &str,
) -> Response {
    let client_ip = extract_client_ip(&req);
    
    // Extract authorization header
    let auth_header = req.headers()
        .get(header_name)
        .and_then(|v| v.to_str().ok());
    
    match auth_header {
        Some(header) => {
            if let Some(api_key) = extract_api_key(header, header_prefix) {
                if let Some(permissions) = auth.validate_key(api_key, &client_ip) {
                    let auth_context = AuthContext::new(
                        Some(format!("api-key-{}", permissions.first().unwrap_or(&"unknown".to_string()))),
                        permissions,
                        AuthMetadata::new(
                            Some(client_ip),
                            req.headers().get("user-agent").and_then(|v| v.to_str().ok()).map(|s| s.to_string()),
                        ),
                    );
                    
                    let mut req = req;
                    req.extensions_mut().insert(auth_context);
                    return next.run(req).await;
                }
            }
            
            // Invalid or missing key
            let mut response = Response::new(Json(json!({
                "error": {
                    "message": "Invalid or missing API key",
                    "type": "authentication_error",
                }
            })));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response
        }
        None => {
            // Missing authorization header
            let mut response = Response::new(Json(json!({
                "error": {
                    "message": format!("Missing authorization header. Expected: {} <key>", header_prefix),
                    "type": "authentication_error",
                }
            })));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response
        }
    }
}

/// Bearer token authentication middleware
pub async fn bearer_auth_middleware(
    req: Request<Body>,
    next: Next,
    auth: Arc<BearerAuth>,
) -> Response {
    let client_ip = extract_client_ip(&req);
    
    // Extract authorization header
    let auth_header = req.headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());
    
    match auth_header {
        Some(header) => {
            if let Some(token) = extract_api_key(header, "Bearer ") {
                match auth.validate_token(token) {
                    Ok(auth_context) => {
                        let mut req = req;
                        req.extensions_mut().insert(auth_context);
                        return next.run(req).await;
                    }
                    Err(e) => {
                        let mut response = Response::new(Json(json!({
                            "error": {
                                "message": format!("Invalid token: {}", e),
                                "type": "authentication_error",
                            }
                        })));
                        *response.status_mut() = StatusCode::UNAUTHORIZED;
                        return response;
                    }
                }
            }
            
            let mut response = Response::new(Json(json!({
                "error": {
                    "message": "Invalid authorization header format. Expected: Bearer <token>",
                    "type": "authentication_error",
                }
            })));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response
        }
        None => {
            let mut response = Response::new(Json(json!({
                "error": {
                    "message": "Missing authorization header. Expected: Bearer <token>",
                    "type": "authentication_error",
                }
            })));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response
        }
    }
}

/// Extract client IP from request
fn extract_client_ip(req: &Request<Body>) -> String {
    // Try X-Forwarded-For header first (for proxies)
    if let Some(forwarded) = req.headers().get("X-Forwarded-For") {
        if let Ok(ips) = forwarded.to_str() {
            // Take the first IP (original client)
            if let Some(ip) = ips.split(',').next() {
                return ip.trim().to_string();
            }
        }
    }
    
    // Try X-Real-IP header
    if let Some(real_ip) = req.headers().get("X-Real-IP") {
        if let Ok(ip) = real_ip.to_str() {
            return ip.to_string();
        }
    }
    
    // Fall back to socket address
    req.peer_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
