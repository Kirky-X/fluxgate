// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Embeddings 形状测试（R-sdk-client-006）：`EmbedInput` 四形状 wire 与
//! untagged 判别、`dimensions/encoding_format/user` 序列化、
//! `EmbeddingValue` 双形态解析（f32 数组 / base64 字符串保真）。

use serde_json::json;

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::config::RequestConfig;
use crate::sdk::embed::{EmbedEncodingFormat, EmbedInput, EmbeddingValue};
use crate::sdk::EmbedRequest;

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

#[test]
fn embed_input_wire_shapes() {
    assert_eq!(
        serde_json::to_value(EmbedInput::Text("hello".into())).unwrap(),
        json!("hello")
    );
    assert_eq!(
        serde_json::to_value(EmbedInput::Strings(vec!["a".into(), "b".into()])).unwrap(),
        json!(["a", "b"])
    );
    assert_eq!(
        serde_json::to_value(EmbedInput::Tokens(vec![1, 2])).unwrap(),
        json!([1, 2])
    );
    assert_eq!(
        serde_json::to_value(EmbedInput::NestedTokens(vec![vec![1, 2], vec![3]])).unwrap(),
        json!([[1, 2], [3]])
    );
}

#[test]
fn embed_input_untagged_classification() {
    let cases = [
        (json!("s"), EmbedInput::Text("s".into())),
        (
            json!(["a", "b"]),
            EmbedInput::Strings(vec!["a".into(), "b".into()]),
        ),
        (json!([1, 2]), EmbedInput::Tokens(vec![1, 2])),
        (
            json!([[1], [2, 3]]),
            EmbedInput::NestedTokens(vec![vec![1], vec![2, 3]]),
        ),
    ];
    for (wire, expected) in cases {
        let parsed: EmbedInput = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(parsed, expected, "wire={wire}");
    }
    // From 便捷转换（&str / Vec<String> / Vec<u32> / Vec<Vec<u32>>）
    assert_eq!(EmbedInput::from("x"), EmbedInput::Text("x".into()));
    assert_eq!(
        EmbedInput::from(vec!["x".to_string()]),
        EmbedInput::Strings(vec!["x".into()])
    );
    assert_eq!(EmbedInput::from(vec![7u32]), EmbedInput::Tokens(vec![7]));
    assert_eq!(
        EmbedInput::from(vec![vec![1u32]]),
        EmbedInput::NestedTokens(vec![vec![1]])
    );
}

#[test]
fn embedding_value_untagged_matrix() {
    let floats: EmbeddingValue = serde_json::from_value(json!([0.5, 1.5])).unwrap();
    assert_eq!(floats, EmbeddingValue::Floats(vec![0.5, 1.5]));
    let base64: EmbeddingValue = serde_json::from_value(json!("AQIDBA==")).unwrap();
    assert_eq!(base64, EmbeddingValue::Base64("AQIDBA==".into()));
}

/// dimensions/encoding_format（EmbedRequest）+ user（RequestConfig）进
/// wire；base64 响应 embedding 原串保真。
#[tokio::test]
async fn embed_dimensions_encoding_and_base64_response() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/embeddings")
        .match_body(mockito::Matcher::PartialJson(json!({
            "model": "text-embedding-3-small",
            "input": ["hello"],
            "dimensions": 256,
            "encoding_format": "base64",
            "user": "u-1"
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"model":"text-embedding-3-small","data":[{"index":0,"embedding":"AQIDBA=="}],"usage":{"prompt_tokens":3,"total_tokens":3}}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let mut req = EmbedRequest::new("text-embedding-3-small", vec!["hello".to_string()]);
    req.dimensions = Some(256);
    req.encoding_format = Some(EmbedEncodingFormat::Base64);
    req.config = RequestConfig {
        user: Some("u-1".into()),
        ..Default::default()
    };

    let resp = build_client(&server.url())
        .embed(&req)
        .await
        .expect("embed");
    assert_eq!(
        resp.data[0].embedding,
        EmbeddingValue::Base64("AQIDBA==".into()),
        "base64 原串保真（不代解码）"
    );
    mock.assert_async().await;
}

/// 单字符串/token 数组输入进 wire（非数组批量路径）。
#[tokio::test]
async fn embed_token_input_serializes_as_integer_array() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/embeddings")
        .match_body(mockito::Matcher::PartialJson(json!({
            "input": [11, 22, 33]
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"model":"m","data":[{"index":0,"embedding":[1.0]}],"usage":{"prompt_tokens":1,"total_tokens":1}}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let req = EmbedRequest::new("m", vec![11u32, 22, 33]);
    build_client(&server.url())
        .embed(&req)
        .await
        .expect("embed");
    mock.assert_async().await;
}
