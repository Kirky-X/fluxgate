// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 运维端点：health / vendors / models。

use fluxgate::sdk::{FluxgateClientBuilder, FluxgateSdkError};

use super::require_env;

pub async fn run() -> Result<(), FluxgateSdkError> {
    let client = FluxgateClientBuilder::new()
        .base_url(require_env("FLUXGATE_BASE_URL"))
        .api_key(require_env("FLUXGATE_API_KEY"))
        .build()?;

    let health = client.health().await?;
    println!(
        "health: {} | version: {} | timestamp: {}",
        health.status, health.version, health.timestamp
    );

    println!("--- vendors（运行面状态）---");
    for vendor in client.vendors().await? {
        println!(
            "{} | {} | queue={} | key_pool={}",
            vendor.id,
            vendor.status,
            vendor.queue_length,
            vendor
                .key_pool
                .as_ref()
                .map(|p| format!("{}/{}", p.current_index + 1, p.size))
                .unwrap_or_else(|| "n/a".into())
        );
    }

    println!("--- models（能力目录）---");
    for model in client.models().await? {
        println!(
            "{} | ctx={} | in={} out={} {}/1M | tools={}",
            model.model_id,
            model.context_window,
            model.input_price,
            model.output_price,
            model.currency,
            model.supports_tools
        );
    }
    Ok(())
}
