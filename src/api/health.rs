// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use sdforge::prelude::*;
use serde_json::json;
use std::sync::Once;

/// forge 端点描述注册进 sdforge 运行时翻译注册表（幂等，建站期触发）；
/// en/zh 字面量与 locales/{en,zh}/api.ftl 同源，两处同步维护
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
