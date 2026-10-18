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

/// 全量 typed 参数配置（T044 面向官方 SDK 参数面）。
fn full_config() -> RequestConfig {
    use crate::sdk::config::{ReasoningEffort, StreamOptions, Verbosity};
    use std::collections::BTreeMap;

    RequestConfig {
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        max_tokens: Some(1000),
        max_completion_tokens: Some(512),
        frequency_penalty: Some(0.1),
        presence_penalty: Some(0.2),
        n: Some(2),
        logprobs: Some(true),
        top_logprobs: Some(3),
        seed: Some(42),
        response_format: Some(json!({"type": "json_object"})),
        logit_bias: Some(BTreeMap::from([("50256".to_string(), -100)])),
        parallel_tool_calls: Some(false),
        reasoning_effort: Some(ReasoningEffort::Xhigh),
        verbosity: Some(Verbosity::Low),
        service_tier: Some("flex".into()),
        store: Some(true),
        metadata: Some(BTreeMap::from([("trace".to_string(), "t-1".to_string())])),
        safety_identifier: Some("user-hash".into()),
        prompt_cache_key: Some("cache-1".into()),
        prompt_cache_retention: Some("24h".into()),
        stream_options: Some(StreamOptions {
            include_usage: Some(true),
            include_obfuscation: None,
        }),
        modalities: Some(vec!["text".into(), "audio".into()]),
        audio: Some(json!({"voice": "alloy", "format": "wav"})),
        prediction: Some(json!({"type": "content", "content": "hi"})),
        web_search_options: Some(json!({"search_context_size": "low"})),
        stream: true,
        stop: Some(vec!["\n".into()]),
        user: Some("user-123".into()),
        ..Default::default()
    }
}

#[test]
fn openai_params_serializes_full_typed_surface() {
    let v = serde_json::to_value(full_config().openai_params()).unwrap();

    assert_eq!(v["max_completion_tokens"], json!(512));
    assert_eq!(v["n"], json!(2));
    assert_eq!(v["logprobs"], json!(true));
    assert_eq!(v["top_logprobs"], json!(3));
    assert_eq!(v["seed"], json!(42));
    assert_eq!(v["response_format"]["type"], json!("json_object"));
    assert_eq!(v["logit_bias"]["50256"], json!(-100));
    assert_eq!(v["parallel_tool_calls"], json!(false));
    assert_eq!(v["reasoning_effort"], json!("xhigh"), "七值词表 wire 词");
    assert_eq!(v["verbosity"], json!("low"));
    assert_eq!(v["service_tier"], json!("flex"));
    assert_eq!(v["store"], json!(true));
    assert_eq!(v["metadata"]["trace"], json!("t-1"));
    assert_eq!(v["safety_identifier"], json!("user-hash"));
    assert_eq!(v["prompt_cache_key"], json!("cache-1"));
    assert_eq!(v["prompt_cache_retention"], json!("24h"));
    assert_eq!(v["stream_options"]["include_usage"], json!(true));
    assert!(
        v["stream_options"].get("include_obfuscation").is_none(),
        "stream_options 未设子键省略"
    );
    assert_eq!(v["modalities"], json!(["text", "audio"]));
    assert_eq!(v["audio"]["voice"], json!("alloy"));
    assert_eq!(v["prediction"]["type"], json!("content"));
    assert_eq!(v["web_search_options"]["search_context_size"], json!("low"));

    let rendered = v.to_string();
    assert!(
        !rendered.contains("null"),
        "wire body 不得含 null: {rendered}"
    );
}

#[test]
fn reasoning_effort_wire_words() {
    use crate::sdk::config::ReasoningEffort;
    for (variant, word) in [
        (ReasoningEffort::None, "none"),
        (ReasoningEffort::Minimal, "minimal"),
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Xhigh, "xhigh"),
        (ReasoningEffort::Max, "max"),
    ] {
        assert_eq!(serde_json::to_value(variant).unwrap(), json!(word));
    }
}

#[test]
fn merge_new_typed_fields_take_other_priority() {
    use crate::sdk::config::ReasoningEffort;
    let base = RequestConfig {
        max_completion_tokens: Some(100),
        reasoning_effort: Some(ReasoningEffort::Low),
        ..Default::default()
    };
    let override_cfg = RequestConfig {
        max_completion_tokens: Some(200),
        ..Default::default()
    };
    let merged = base.merge(&override_cfg);
    assert_eq!(merged.max_completion_tokens, Some(200), "other 优先");
    assert_eq!(
        merged.reasoning_effort,
        Some(ReasoningEffort::Low),
        "other 未设 → 保留 self"
    );
}

/// denylist 覆盖不变量：OpenAiParams 全量序列化产出的每个键必须 ∈
/// RESERVED_KEYS（漏项 = vendor 扩展键可覆写 typed 请求骨架）。
#[test]
fn reserved_keys_cover_typed_wire_surface() {
    use crate::sdk::chat::RESERVED_KEYS;

    let v = serde_json::to_value(full_config().openai_params()).unwrap();
    for key in v.as_object().unwrap().keys() {
        assert!(
            RESERVED_KEYS.contains(&key.as_str()),
            "typed 键 `{key}` 未入 RESERVED_KEYS"
        );
    }
    // ChatPayload 顶层承载键 + 扩展载体键
    for key in [
        "model",
        "messages",
        "tools",
        "tool_choice",
        "vendor_extensions",
    ] {
        assert!(RESERVED_KEYS.contains(&key), "`{key}` 未入 RESERVED_KEYS");
    }
}
