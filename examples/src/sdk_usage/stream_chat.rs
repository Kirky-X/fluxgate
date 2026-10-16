// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 流式 chat：`chat_stream` 逐 chunk 消费 SSE 流。

use std::time::Duration;

use fluxgate::sdk::{ChatRequest, FluxgateClientBuilder, FluxgateSdkError};
use futures::StreamExt;

use super::require_env;

pub async fn run() -> Result<(), FluxgateSdkError> {
    let client = FluxgateClientBuilder::new()
        .base_url(require_env("FLUXGATE_BASE_URL"))
        .api_key(require_env("FLUXGATE_API_KEY"))
        // 流式不设总超时：总超时按整个 body 计会把 SSE 长流中途截断；
        // read_timeout 只拦截连续无字节的连接停滞，成功读取即重置
        .read_timeout(Duration::from_secs(60))
        .build()?;

    // chat_stream 内部强制 stream = true，config.stream 无需手动置位
    let req = ChatRequest::new("gpt-4", "从 1 数到 10，用逗号分隔。");
    let mut stream = client.chat_stream(&req).await?;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if let Some(content) = chunk.delta.as_ref().and_then(|d| d.content.as_deref()) {
            print!("{content}");
        }
        if let Some(reason) = &chunk.finish_reason {
            println!("\n[流结束: {}]", reason.as_wire());
        }
        if let Some(usage) = &chunk.usage {
            eprintln!(
                "[usage] prompt={} completion={} cache_read={}",
                usage.prompt_tokens, usage.completion_tokens, usage.cache_read_tokens
            );
        }
    }
    Ok(())
}
