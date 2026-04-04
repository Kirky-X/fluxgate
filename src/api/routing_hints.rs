//! 路由提示提取器
//!
//! 从 HTTP 请求头提取路由信息：
//! - X-Vendor: 指定具体厂商
//! - X-Model: 使用抽象模型

use axum::http::HeaderMap;

/// 路由提示结构
#[derive(Debug, Clone, Default)]
pub struct RoutingHints {
    /// 优先使用的厂商 ID
    pub preferred_vendor: Option<String>,
    /// 优先使用的模型 ID
    pub preferred_model: Option<String>,
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
        }
    }

    /// 检查是否有路由提示
    pub fn has_hints(&self) -> bool {
        self.preferred_vendor.is_some() || self.preferred_model.is_some()
    }
}
