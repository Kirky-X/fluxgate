// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::sdk::config::RequestConfig;
use serde_json::json;

#[test]
fn default_stream_is_false() {
    assert!(!RequestConfig::default().stream);
}

#[test]
fn merge_takes_other_priority() {
    let a = RequestConfig::default();
    let b = RequestConfig {
        temperature: Some(0.5),
        ..Default::default()
    };
    let merged = a.merge(&b);
    assert_eq!(merged.temperature, Some(0.5));
}

/// 未设置的可选字段必须省略而非写 null：显式 null 在严格模式的上游
/// 会被拒绝，且每次请求都为空字段付出序列化分配
#[test]
fn openai_params_omit_unset_fields() {
    let config = RequestConfig::default();
    let v = serde_json::to_value(config.openai_params()).unwrap();
    assert!(v.get("temperature").is_none(), "unset 温度必须省略: {v}");
    assert!(v.get("top_p").is_none());
    assert!(v.get("top_k").is_none());
    assert!(v.get("max_tokens").is_none());
    assert!(v.get("stop").is_none());
    assert!(v.get("user").is_none());
    // stream 是显式布尔，恒存在
    assert_eq!(v["stream"], json!(false));
    let rendered = v.to_string();
    assert!(
        !rendered.contains("null"),
        "wire body 不得含 null: {rendered}"
    );
}

#[test]
fn openai_params_excludes_hints() {
    let config = RequestConfig {
        vendor_hint: Some("openai".into()),
        model_hint: Some("gpt-4".into()),
        ..Default::default()
    };
    let v = serde_json::to_value(config.openai_params()).unwrap();
    assert!(v.get("vendor_hint").is_none());
    assert!(v.get("model_hint").is_none());
}

#[test]
fn openai_params_includes_all_fields_when_set() {
    let config = RequestConfig {
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        max_tokens: Some(1000),
        stream: true,
        stop: Some(vec!["\n".into()]),
        user: Some("user-123".into()),
        ..Default::default()
    };
    let v = serde_json::to_value(config.openai_params()).unwrap();
    // f32 → f64 有精度损失，用近似比较
    assert!((v["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
    assert!((v["top_p"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(v["top_k"], json!(40));
    assert_eq!(v["max_tokens"], json!(1000));
    assert_eq!(v["stream"], json!(true));
    assert_eq!(v["stop"], json!(["\n"]));
    assert_eq!(v["user"], json!("user-123"));
}
