// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::config::RequestConfig;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::RerankRequest;

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

fn request() -> RerankRequest {
    RerankRequest {
        model: "rerank-v3".into(),
        query: "什么是重排".into(),
        documents: vec!["a".into(), "b".into()],
        top_n: Some(2),
        return_documents: Some(true),
        instruction: None,
        config: RequestConfig::default(),
    }
}

#[tokio::test]
async fn rerank_success_parses_response() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/rerank")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"id":"rerank-1","model":"rerank-v3.5","results":[{"index":1,"relevance_score":0.9,"document":{"text":"b"}},{"index":0,"relevance_score":0.1}],"meta":{"vendor":"cohere-prod","billed_documents":2}}"#,
        )
        .create();

    let client = build_client(&server.url());
    let resp = client.rerank(&request()).await.unwrap();

    assert_eq!(resp.model, "rerank-v3.5", "回传实际上游模型名");
    assert_eq!(resp.results.len(), 2);
    assert_eq!(resp.results[0].index, 1);
    assert!((resp.results[0].relevance_score - 0.9).abs() < f64::EPSILON);
    assert_eq!(resp.results[0].document.as_ref().unwrap().text, "b");
    assert_eq!(resp.results[1].document, None, "可选 document 缺省 None");
    assert_eq!(resp.meta.vendor, "cohere-prod");
    assert_eq!(resp.meta.billed_documents, 2);
}

#[tokio::test]
async fn rerank_500_maps_to_server_error() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/rerank")
        .with_status(500)
        .with_body("upstream boom")
        .create();

    let client = build_client(&server.url());
    let err = client.rerank(&request()).await.unwrap_err();
    match err {
        FluxgateSdkError::Server { status, message } => {
            assert_eq!(status, 500);
            assert!(message.contains("upstream boom"));
        }
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn list_models_appends_task_query_only_when_present() {
    let mut server = mockito::Server::new_async().await;
    let with_task = server
        .mock("GET", "/v1/models?task=rerank")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"object":"list","data":[{"id":"rerank-v3","task":"rerank"}]}"#)
        .create();
    let client = build_client(&server.url());

    let filtered = client.list_models(Some("rerank")).await.unwrap();
    assert_eq!(filtered["data"][0]["task"], "rerank");
    with_task.assert_async().await;

    let no_task = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"object":"list","data":[]}"#)
        .create();
    let all = client.list_models(None).await.unwrap();
    assert_eq!(all["data"].as_array().unwrap().len(), 0);
    no_task.assert_async().await;
}
