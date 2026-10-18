// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Embedding：一次请求两条文本，打印每条向量维度与 token 用量
//!（`EmbedInput` 多形态与 `EmbeddingValue` 双编码展示）。

use fluxgate::sdk::{
    EmbedInput, EmbedRequest, EmbeddingValue, FluxgateClientBuilder, FluxgateSdkError,
    RequestConfig,
};

use super::require_env;

pub async fn run() -> Result<(), FluxgateSdkError> {
    let client = FluxgateClientBuilder::new()
        .base_url(require_env("FLUXGATE_BASE_URL"))
        .api_key(require_env("FLUXGATE_API_KEY"))
        .build()?;

    let req = EmbedRequest {
        // 须为网关侧配置了 embedding 能力的模型（样例值取自 SDK 集成测试）
        model: "text-embedding-3-small".into(),
        // 输入四形态可选：Text / Strings / Tokens / NestedTokens
        input: EmbedInput::Strings(vec![
            "Fluxgate 是智能 LLM 路由网关。".into(),
            "流式响应经 SSE 分帧后逐块送达。".into(),
        ]),
        // 可选：dimensions 降维（模型支持时生效）、encoding_format 切 base64
        dimensions: None,
        encoding_format: None,
        config: RequestConfig::default(),
    };

    let resp = client.embed(&req).await?;
    for data in &resp.data {
        // 响应向量双形态：float 数组（缺省）或 base64 原串（不代解码）
        match &data.embedding {
            EmbeddingValue::Floats(values) => {
                let dim = values.len();
                println!(
                    "input[{}]: dims={} head={:?}",
                    data.index,
                    dim,
                    &values[..dim.min(3)]
                );
            }
            EmbeddingValue::Base64(raw) => {
                println!(
                    "input[{}]: base64 编码（{} 字符，SDK 不代解码）",
                    data.index,
                    raw.len()
                );
            }
        }
    }
    println!(
        "model: {} | tokens: prompt={} total={}",
        resp.model, resp.usage.prompt_tokens, resp.usage.total_tokens
    );
    Ok(())
}
