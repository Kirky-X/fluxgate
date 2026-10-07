// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Embedding：一次请求两条文本，打印每条向量维度与 token 用量。

use nexusflow::sdk::{EmbedRequest, NexusFlowClientBuilder, NexusFlowSdkError, RequestConfig};

use super::require_env;

pub async fn run() -> Result<(), NexusFlowSdkError> {
    let client = NexusFlowClientBuilder::new()
        .base_url(require_env("NEXUSFLOW_BASE_URL"))
        .api_key(require_env("NEXUSFLOW_API_KEY"))
        .build()?;

    let req = EmbedRequest {
        // 须为网关侧配置了 embedding 能力的模型（样例值取自 SDK 集成测试）
        model: "text-embedding-3-small".into(),
        input: vec![
            "NexusFlow 是智能 LLM 路由网关。".into(),
            "流式响应经 SSE 分帧后逐块送达。".into(),
        ],
        config: RequestConfig::default(),
    };

    let resp = client.embed(&req).await?;
    for data in &resp.data {
        let dim = data.embedding.len();
        println!(
            "input[{}]: dims={} head={:?}",
            data.index,
            dim,
            &data.embedding[..dim.min(3)]
        );
    }
    println!(
        "model: {} | tokens: prompt={} total={}",
        resp.model, resp.usage.prompt_tokens, resp.usage.total_tokens
    );
    Ok(())
}
