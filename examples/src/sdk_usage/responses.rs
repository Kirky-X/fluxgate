// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Responses：非流式两轮（`previous_response_id` 回放）+ 流式事件打印。

use fluxgate::sdk::{FluxgateClientBuilder, FluxgateSdkError, RequestConfig, ResponsesRequest};
use futures::StreamExt as _;

use super::require_env;

pub async fn run() -> Result<(), FluxgateSdkError> {
    let client = FluxgateClientBuilder::new()
        .base_url(require_env("FLUXGATE_BASE_URL"))
        .api_key(require_env("FLUXGATE_API_KEY"))
        .build()?;

    let models = client.list_models(Some("chat")).await?;
    let model = models["data"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["id"].as_str())
        .unwrap_or("gpt-4.1");

    // 第一轮：字符串 input + instructions
    let first = ResponsesRequest {
        model: model.into(),
        input: serde_json::json!("用一句话解释重排与嵌入的区别"),
        instructions: Some("回答不超过 80 字".into()),
        max_output_tokens: Some(128),
        ..Default::default()
    };
    let first = client.responses(&first).await?;
    println!(
        "id={} status={} usage={:?}",
        first.id, first.status, first.usage
    );
    for item in &first.output {
        println!("output: {item:?}");
    }

    // 第二轮：previous_response_id 由网关回放历史（跨厂商可用）
    let second = ResponsesRequest {
        model: model.into(),
        input: serde_json::json!("再给一个具体例子"),
        previous_response_id: Some(first.id.clone()),
        max_output_tokens: Some(128),
        ..Default::default()
    };
    let second = client.responses(&second).await?;
    println!("第二轮 id={} status={}", second.id, second.status);

    // 流式：语义化 SSE 事件
    let stream_request = ResponsesRequest {
        model: model.into(),
        input: serde_json::json!("流式输出三个要点"),
        max_output_tokens: Some(128),
        config: RequestConfig::default(),
        ..Default::default()
    };
    let mut stream = client.responses_stream(&stream_request).await?;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(delta) = event.delta {
            print!("{delta}");
        }
        if matches!(
            event.event_type.as_str(),
            "response.completed" | "response.incomplete" | "response.failed"
        ) {
            println!("\n[{}]", event.event_type);
        }
    }
    Ok(())
}
