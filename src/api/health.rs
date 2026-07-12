// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use sdforge::prelude::*;
use serde_json::json;

/// Health check endpoint
///
/// Returns the current status of the API server.
#[service_api(
    name = "health_check",
    version = "v1",
    path = "/health",
    method = "GET",
    description = "Health check endpoint"
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
