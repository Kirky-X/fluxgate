// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use sdforge::prelude::*;
use serde_json::json;
use std::sync::Once;

/// forge 端点描述注册进 sdforge 运行时翻译注册表（幂等）——
/// `#[forge(i18n_key)]` 的消费点（OpenAPI/MCP/CLI，按目标 feature 启用）
/// 按活跃 locale 翻译 description。en/zh 值与 locales/{en,zh}/api.ftl 的
/// `api-health-description` 键同源，两处同步维护（sdforge 注册 API 只收
/// 字面量，批量从 FTL 提取不属本桶）。由 [`crate::api::create_router`]
/// 在建站期触发，早于一切协议消费请求。
static REGISTER_FORGE_I18N: Once = Once::new();

pub(crate) fn register_forge_i18n() {
    REGISTER_FORGE_I18N.call_once(|| {
        sdforge::i18n::register_translation(
            "en",
            "api-health-description",
            "Health check endpoint",
        );
        sdforge::i18n::register_translation("zh", "api-health-description", "健康检查端点");
    });
}

/// Health check endpoint
///
/// Returns the current status of the API server.
#[forge(
    name = "health_check",
    version = "v1",
    path = "/health",
    method = "GET",
    description = "Health check endpoint",
    i18n_key = "api-health-description"
)]
pub async fn health_check() -> Result<serde_json::Value, ApiError> {
    // Check DB connection
    // Check other components if needed

    // For now just return OK
    Ok(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}
