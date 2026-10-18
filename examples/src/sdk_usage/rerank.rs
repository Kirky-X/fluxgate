// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Rerank：按 task 过滤列出 rerank 模型，再对一批文档重排并打印 top 结果。

use fluxgate::sdk::{FluxgateClientBuilder, FluxgateSdkError, RequestConfig, RerankRequest};

use super::require_env;

pub async fn run() -> Result<(), FluxgateSdkError> {
    let client = FluxgateClientBuilder::new()
        .base_url(require_env("FLUXGATE_BASE_URL"))
        .api_key(require_env("FLUXGATE_API_KEY"))
        .build()?;

    // ?task=rerank：只列出具备重排能力的模型（无此参数时返回全部）
    let models = client.list_models(Some("rerank")).await?;
    println!("rerank models: {}", models["data"]);
    let model = models["data"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["id"].as_str())
        .unwrap_or("rerank-v3");

    let req = RerankRequest {
        model: model.into(),
        query: "Fluxgate 如何做成本分账？".into(),
        documents: vec![
            "按 vendor/model/api_key_hash/小时预聚合四类 token 与分项成本。".into(),
            "重排请求按文档数计价，成本归入 input_cost 分项。".into(),
            "流式响应经 SSE 分帧后逐块送达。".into(),
        ],
        top_n: Some(2),
        return_documents: Some(false),
        instruction: None,
        config: RequestConfig::default(),
    };

    let resp = client.rerank(&req).await?;
    println!(
        "model={} vendor={} docs={}",
        resp.model, resp.meta.vendor, resp.meta.billed_documents
    );
    for item in &resp.results {
        println!("index={} score={:.4}", item.index, item.relevance_score);
    }
    Ok(())
}
