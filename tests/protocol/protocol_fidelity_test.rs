// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 协议转换语义保真参数化回归（表驱动：每语义点至少一条用例）。

use fluxgate::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
use serde_json::{json, Value};

fn openai() -> OpenAIConverter {
    OpenAIConverter::new()
}

fn anthropic() -> AnthropicConverter {
    AnthropicConverter::new()
}

fn base_openai_request() -> Value {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "hi"}],
    })
}

fn base_anthropic_request() -> Value {
    json!({
        "model": "claude-x",
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}],
    })
}

/// tool_choice {"type":"any"} / {"type":"tool",name} 不得降级为 "auto"
///（跨协议客户端常带 Anthropic 形态；降级会静默改变工具调度语义）。
#[test]
fn tool_choice_any_and_tool_never_downgrade_to_auto() {
    for (name, body) in [
        ("any", json!({"type": "any"})),
        ("tool-named", json!({"type": "tool", "name": "get_weather"})),
    ] {
        let mut request = base_openai_request();
        request["tool_choice"] = body;
        let unified = openai().to_unified(request).expect("to unified");
        let choice = unified.tool_choice.expect("tool_choice 必须保留");
        let rendered = serde_json::to_value(&choice).unwrap().to_string();
        assert!(
            !rendered.contains("\"auto\""),
            "{name} 不得降级为 auto: {rendered}"
        );

        // 出站还原：any → required（OpenAI wire 词），tool → 具名 function。
        let mut unified2 = openai()
            .to_unified({
                let mut request = base_openai_request();
                request["tool_choice"] = if name == "any" {
                    json!({"type": "any"})
                } else {
                    json!({"type": "tool", "name": "get_weather"})
                };
                request
            })
            .expect("to unified");
        unified2.model = "gpt-4".into();
        let external = openai().request_to_external(unified2).expect("to external");
        let wire = external["tool_choice"].to_string();
        assert!(!wire.contains("\"auto\""), "{name} 出站不得为 auto: {wire}");
    }

    // Anthropic 入站同语义：any → required；tool(name) → 具名保留。
    let mut request = base_anthropic_request();
    request["tool_choice"] = json!({"type": "any"});
    let unified = anthropic().to_unified(request).expect("to unified");
    let rendered = serde_json::to_value(unified.tool_choice.unwrap())
        .unwrap()
        .to_string();
    assert!(
        rendered.contains("required") && !rendered.contains("\"auto\""),
        "anthropic any → required: {rendered}"
    );
    let mut request = base_anthropic_request();
    request["tool_choice"] = json!({"type": "tool", "name": "get_weather"});
    let unified = anthropic().to_unified(request).expect("to unified");
    let rendered = serde_json::to_value(unified.tool_choice.unwrap())
        .unwrap()
        .to_string();
    assert!(
        rendered.contains("get_weather"),
        "anthropic tool 具名保留: {rendered}"
    );
}

/// tool_result 同消息内的文本块不丢失（Anthropic user 消息多块并存）。
#[test]
fn tool_result_keeps_sibling_text_blocks() {
    let mut request = base_anthropic_request();
    request["messages"] = json!([
        {"role": "user", "content": [
            {"type": "text", "text": "what is the weather?"},
            {"type": "tool_result", "tool_use_id": "t1", "content": "sunny"},
        ]},
    ]);
    let unified = anthropic().to_unified(request).expect("to unified");
    let dumped = serde_json::to_value(&unified.messages).unwrap().to_string();
    assert!(
        dumped.contains("what is the weather?"),
        "同消息文本块不得丢失: {dumped}"
    );
    assert!(dumped.contains("sunny"), "tool_result 内容保留: {dumped}");

    // 出站 OpenAI：tool 结果进 role=tool 消息（content 非空）。
    let external = openai().request_to_external(unified).expect("to external");
    let dumped = external["messages"].to_string();
    assert!(dumped.contains("sunny"), "出站保留 tool 结果: {dumped}");
}

/// system 多块内容完整保留（Anthropic system 数组 → system_prompt 全含）。
#[test]
fn system_multi_block_preserved() {
    let mut request = base_anthropic_request();
    request["system"] = json!([
        {"type": "text", "text": "You are helpful."},
        {"type": "text", "text": "Answer briefly."},
    ]);
    let unified = anthropic().to_unified(request).expect("to unified");
    let prompt = unified
        .system_prompt
        .clone()
        .expect("system_prompt 必须回填");
    assert!(prompt.contains("You are helpful."), "首块保留: {prompt}");
    assert!(prompt.contains("Answer briefly."), "次块保留: {prompt}");

    // 出站 OpenAI：system 指令不丢（system 消息或顶层 system）。
    let external = openai().request_to_external(unified).expect("to external");
    let dumped = external.to_string();
    assert!(dumped.contains("You are helpful."), "出站保留: {dumped}");
}

/// stop_sequences / max_tokens / temperature / top_p 映射正确（双向；
/// OpenAI 面字段名为 `stop`，string 与 array 双形态）。
#[test]
fn sampling_params_map_both_directions() {
    for converter in ["openai", "anthropic"] {
        let mut request = if converter == "openai" {
            base_openai_request()
        } else {
            base_anthropic_request()
        };
        // 协议原生停止序列键名（OpenAI `stop`，Anthropic `stop_sequences`）。
        if converter == "openai" {
            request["stop"] = json!(["STOP", "END"]);
        } else {
            request["stop_sequences"] = json!(["STOP", "END"]);
        }
        request["max_tokens"] = json!(64);
        request["temperature"] = json!(0.5);
        request["top_p"] = json!(0.9);
        let unified = if converter == "openai" {
            openai().to_unified(request).expect("to unified")
        } else {
            anthropic().to_unified(request).expect("to unified")
        };
        assert_eq!(unified.max_tokens, Some(64), "{converter} max_tokens");
        assert_eq!(unified.temperature, Some(0.5), "{converter} temperature");
        assert_eq!(unified.top_p, Some(0.9), "{converter} top_p");
        assert!(
            unified.stop.iter().any(|s| s == "STOP"),
            "{converter} stop_sequences: {:?}",
            unified.stop
        );

        // 出站回写同值（OpenAI 面；f32 存储，epsilon 比较）。
        let external = openai().request_to_external(unified).expect("to external");
        assert_eq!(external["max_tokens"], 64, "{converter} 出站 max_tokens");
        for (key, expected) in [("temperature", 0.5f64), ("top_p", 0.9f64)] {
            let actual = external[key].as_f64().expect("数值");
            assert!(
                (actual - expected).abs() < 1e-6,
                "{converter} 出站 {key}: {actual}"
            );
        }
        assert!(
            external["stop"].to_string().contains("STOP"),
            "{converter} 出站 stop: {}",
            external["stop"]
        );
    }

    // OpenAI `stop` string 形态不丢（wire 允许 string 或 array）。
    let mut request = base_openai_request();
    request["stop"] = json!("STOP");
    let unified = openai().to_unified(request).expect("to unified");
    assert!(
        unified.stop.iter().any(|s| s == "STOP"),
        "openai stop string 形态: {:?}",
        unified.stop
    );
}

/// thinking 配置透传不丢（Anthropic → IR；OpenAI 无等价槽，降级有声明）。
#[test]
fn thinking_config_passthrough() {
    let mut request = base_anthropic_request();
    request["thinking"] = json!({"type": "enabled", "budget_tokens": 1000});
    let unified = anthropic().to_unified(request).expect("to unified");
    assert!(
        unified.thinking_config.is_some(),
        "thinking 必须进 IR（不丢）"
    );
    // Anthropic 出站回写 thinking（同协议往返保真）。
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert!(
        external.get("thinking").is_some(),
        "anthropic 出站 thinking 回写: {external}"
    );
}

/// 多模态 image 块双向映射（base64 与 url 形态）。
#[test]
fn image_blocks_map_both_directions() {
    for (name, url) in [
        ("base64", "data:image/png;base64,aGVsbG8="),
        ("url", "https://example.com/pic.png"),
    ] {
        // OpenAI 入站 → IR → Anthropic 出站：image 块存活。
        let mut request = base_openai_request();
        request["messages"] = json!([
            {"role": "user", "content": [
                {"type": "text", "text": "describe"},
                {"type": "image_url", "image_url": {"url": url}},
            ]},
        ]);
        let unified = openai().to_unified(request).expect("to unified");
        let dumped = serde_json::to_value(&unified.messages).unwrap().to_string();
        assert!(dumped.contains("describe"), "{name} 文本共存: {dumped}");
        let external = anthropic()
            .request_to_external(unified)
            .expect("to external");
        let dumped = external["messages"].to_string();
        assert!(
            dumped.contains("image"),
            "{name} → anthropic image 存活: {dumped}"
        );

        // Anthropic 入站 → IR → OpenAI 出站：image_url 存活。
        let mut request = base_anthropic_request();
        request["messages"] = json!([
            {"role": "user", "content": [
                {"type": "text", "text": "describe"},
                if name == "base64" {
                    json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}})
                } else {
                    json!({"type": "image", "source": {"type": "url", "url": url}})
                },
            ]},
        ]);
        let unified = anthropic().to_unified(request).expect("to unified");
        let external = openai().request_to_external(unified).expect("to external");
        let dumped = external["messages"].to_string();
        assert!(
            dumped.contains("image_url"),
            "{name} → openai image_url 存活: {dumped}"
        );
    }
}
