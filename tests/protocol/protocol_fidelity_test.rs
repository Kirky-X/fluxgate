// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 协议转换语义保真参数化回归（表驱动：每语义点至少一条用例）。

use fluxweir::protocol::{AnthropicConverter, OpenAIConverter, ProtocolConverter};
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

/// effort 七值双向：OpenAI `reasoning_effort` ↔ Anthropic `output_config.effort`
///（Minimal→low 降级、None 省略字段；R-004/R-010）。
#[test]
fn effort_cross_protocol_roundtrip() {
    // OpenAI high → IR → Anthropic output_config.effort=high
    let mut request = base_openai_request();
    request["reasoning_effort"] = json!("high");
    let unified = openai().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert_eq!(external["output_config"]["effort"], "high");
    assert!(
        external.get("reasoning_effort").is_none(),
        "OpenAI 词不得直写 Anthropic 顶层: {external}"
    );

    // Anthropic xhigh → IR → OpenAI reasoning_effort=xhigh（顺路保真）
    let mut request = base_anthropic_request();
    request["output_config"] = json!({"effort": "xhigh"});
    let unified = anthropic().to_unified(request).expect("to unified");
    let external = openai().request_to_external(unified).expect("to external");
    assert_eq!(external["reasoning_effort"], "xhigh");

    // Minimal → Anthropic 降级 low；None → 不写 output_config
    for (wire, expected) in [("minimal", Some("low")), ("none", None)] {
        let mut request = base_openai_request();
        request["reasoning_effort"] = json!(wire);
        let unified = openai().to_unified(request).expect("to unified");
        let external = anthropic()
            .request_to_external(unified)
            .expect("to external");
        match expected {
            Some(effort) => assert_eq!(
                external["output_config"]["effort"], effort,
                "{wire} 必须降级为 {effort}"
            ),
            None => assert!(
                external.get("output_config").is_none(),
                "none 不得写 output_config: {external}"
            ),
        }
    }
}

/// thinking 全变体跨协议保真：Anthropic 入站（含 display）同协议往返逐字
/// 相等；thinking 预算不透传到 OpenAI 时走显性丢弃（不 panic、不出字段）。
#[test]
fn thinking_cross_protocol_fidelity() {
    for thinking in [
        json!({"type": "enabled", "budget_tokens": 2048, "display": "omitted"}),
        json!({"type": "adaptive", "display": "summarized"}),
        json!({"type": "between_tools"}),
    ] {
        let mut request = base_anthropic_request();
        request["thinking"] = thinking.clone();
        let unified = anthropic().to_unified(request).expect("to unified");
        assert!(
            unified.thinking_config.is_some(),
            "thinking 入 IR: {thinking}"
        );
        // 同协议往返逐字相等
        let external = anthropic()
            .request_to_external(unified)
            .expect("to external");
        assert_eq!(external["thinking"], thinking, "thinking 往返保真");

        // 跨协议到 OpenAI：无等价槽 → 不出 thinking 字段（显性丢弃）
        let mut request = base_anthropic_request();
        request["thinking"] = thinking.clone();
        let unified = anthropic().to_unified(request).expect("to unified");
        let external = openai().request_to_external(unified).expect("to external");
        assert!(
            external.get("thinking").is_none(),
            "OpenAI 无 thinking 槽: {external}"
        );
    }

    // Disabled：Anthropic 缺省即关闭 → 出站省略字段
    let mut request = base_anthropic_request();
    request["thinking"] = json!({"type": "disabled"});
    let unified = anthropic().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert!(external.get("thinking").is_none(), "Disabled 不写字段");
}

/// tool_choice none 双向：Anthropic 原生 `{"type":"none"}` ↔ OpenAI 字符串
/// "none"（R-006；不得降级为 auto 或省略）。
#[test]
fn tool_choice_none_cross_protocol() {
    // Anthropic {"type":"none"} → IR → OpenAI "none"
    let mut request = base_anthropic_request();
    request["tool_choice"] = json!({"type": "none"});
    let unified = anthropic().to_unified(request).expect("to unified");
    let upgraded = unified.tool_choice.clone().expect("保留");
    let rendered = serde_json::to_value(&upgraded).unwrap().to_string();
    assert!(rendered.contains("none"), "none 入 IR: {rendered}");
    let external = openai().request_to_external(unified).expect("to external");
    assert_eq!(external["tool_choice"], "none");

    // OpenAI "none" → IR → Anthropic 原生 {"type":"none"}
    let mut request = base_openai_request();
    request["tool_choice"] = json!("none");
    let unified = openai().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert_eq!(external["tool_choice"], json!({"type": "none"}));
}

/// metadata/user：Anthropic `metadata` 整体往返 + `user_id`↔IR user；
/// OpenAI `user` 跨协议落 Anthropic `metadata.user_id`（R-011）。
#[test]
fn metadata_user_cross_protocol() {
    // Anthropic 入站 → IR → Anthropic 出站：两键保留
    let mut request = base_anthropic_request();
    request["metadata"] = json!({"user_id": "u1", "trace": "t"});
    let unified = anthropic().to_unified(request).expect("to unified");
    assert_eq!(unified.user.as_deref(), Some("u1"), "user_id → IR user");
    assert_eq!(
        unified.metadata,
        Some(json!({"user_id": "u1", "trace": "t"})),
        "metadata 整体入 IR"
    );
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert_eq!(external["metadata"], json!({"user_id": "u1", "trace": "t"}));

    // OpenAI user → IR → Anthropic metadata.user_id（无 metadata 时补）
    let mut request = base_openai_request();
    request["user"] = json!("u2");
    let unified = openai().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert_eq!(external["metadata"], json!({"user_id": "u2"}));
}

/// 响应侧保真：usage details（cached/reasoning_tokens）与 logprobs 经
/// OpenAI 往返；cached_tokens 跨协议落 Anthropic cache_read_input_tokens；
/// Anthropic stop_sequence 实际值往返（R-011）。
#[test]
fn response_usage_details_logprobs_stop_sequence_roundtrip() {
    let response = json!({
        "id": "chatcmpl-1",
        "model": "gpt-4",
        "created": 1,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hi"},
            "finish_reason": "stop",
            "logprobs": {"content": [{"token": "hi", "logprob": -0.1}]}
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 7,
            "total_tokens": 12,
            "prompt_tokens_details": {"cached_tokens": 3},
            "completion_tokens_details": {"reasoning_tokens": 4}
        }
    });
    let unified = openai().parse_response(response).expect("parse");
    let external = openai().response_to_external(unified).expect("to external");
    assert_eq!(
        external["usage"]["prompt_tokens_details"]["cached_tokens"], 3,
        "prompt_tokens_details 回写"
    );
    assert_eq!(
        external["usage"]["completion_tokens_details"]["reasoning_tokens"], 4,
        "completion_tokens_details 回写"
    );
    assert_eq!(
        external["choices"][0]["logprobs"]["content"][0]["token"], "hi",
        "choices[].logprobs 回写"
    );

    // OpenAI 响应 → IR → Anthropic 响应：cached_tokens 落
    // cache_read_input_tokens（跨协议载体）
    let unified = openai()
        .parse_response(json!({
            "id": "chatcmpl-2",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 1,
                "total_tokens": 6,
                "prompt_tokens_details": {"cached_tokens": 3}
            }
        }))
        .expect("parse");
    let external = anthropic()
        .response_to_external(unified)
        .expect("to external");
    assert_eq!(external["usage"]["cache_read_input_tokens"], 3);

    // Anthropic 响应 stop_sequence 实际值往返
    let unified = anthropic()
        .parse_response(json!({
            "id": "msg_1",
            "model": "claude-x",
            "content": [{"type": "text", "text": "par"}],
            "stop_reason": "stop_sequence",
            "stop_sequence": "END",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .expect("parse");
    let external = anthropic()
        .response_to_external(unified)
        .expect("to external");
    assert_eq!(external["stop_sequence"], "END");
    assert_eq!(external["stop_reason"], "stop_sequence");
}

/// Anthropic `sampling` 段补缺省槽：top_k 直出；temperature/top_p 仅在 flat
/// 槽未设时补（flat 优先不覆写）。
#[test]
fn sampling_section_backfills_unset_flat_slots() {
    // wire 无 `sampling` 段解析：IR 槽由 core 对齐层喂入，此处手构注入
    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.sampling = Some(fluxweir::protocol::SamplingParams {
        temperature: Some(0.3),
        top_k: Some(40),
        top_p: Some(0.8),
        frequency_penalty: None,
        presence_penalty: None,
    });
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert_eq!(external["top_k"], 40, "top_k 直出");
    assert!(
        (external["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6,
        "flat 缺省时 sampling.temperature 补位: {}",
        external["temperature"]
    );
    assert!(
        (external["top_p"].as_f64().unwrap() - 0.8).abs() < 1e-6,
        "flat 缺省时 sampling.top_p 补位: {}",
        external["top_p"]
    );

    // flat 优先：显式 temperature 不被 sampling 覆写
    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.temperature = Some(0.9);
    unified.sampling = Some(fluxweir::protocol::SamplingParams {
        temperature: Some(0.3),
        ..Default::default()
    });
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert!(
        (external["temperature"].as_f64().unwrap() - 0.9).abs() < 1e-6,
        "flat temperature 必须优先: {}",
        external["temperature"]
    );
}

/// 工具结果与带文本的 tool_calls 出站为 Anthropic 块形态：tool_result 块
/// / 纯文本块（无 tool_call 语义时）。
#[test]
fn tool_result_and_tool_calls_text_outbound_blocks() {
    // OpenAI 入站：assistant tool_calls + tool 结果消息
    let mut request = base_openai_request();
    request["messages"] = json!([
        {"role": "user", "content": "run it"},
        {"role": "assistant", "content": "calling",
         "tool_calls": [{"id": "call_1", "type": "function",
                         "function": {"name": "get_weather", "arguments": "{}"}}]},
        {"role": "tool", "tool_call_id": "call_1", "content": "22C sunny"},
    ]);
    let unified = openai().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    let dumped = external["messages"].to_string();
    assert!(
        dumped.contains("tool_result"),
        "工具结果必须映射 tool_result 块: {dumped}"
    );
    assert!(dumped.contains("22C sunny"), "工具结果内容存活: {dumped}");
    assert!(
        dumped.contains("get_weather"),
        "assistant tool_calls 语义存活: {dumped}"
    );
}

/// Anthropic 入站 thinking 块（带 signature）→ IR → 同协议出站：
/// thinking 原生块 + signature 原样回写（D3）。
#[test]
fn assistant_thinking_block_roundtrip_keeps_signature() {
    let mut request = base_anthropic_request();
    request["messages"] = json!([
        {"role": "user", "content": "think"},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "internal chain", "signature": "sig_abc"},
        ]},
    ]);
    let unified = anthropic().to_unified(request).expect("to unified");
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    let dumped = external["messages"].to_string();
    assert!(
        dumped.contains(r#""thinking":"internal chain""#),
        "thinking 块出站: {dumped}"
    );
    assert!(
        dumped.contains("sig_abc"),
        "signature 必须原样回写: {dumped}"
    );
}

/// Anthropic `output_config.format`：json_schema 带 schema 入 IR；缺 schema
/// 显性丢弃（response_format 为空）。
#[test]
fn anthropic_output_config_json_schema_mapping() {
    let mut request = base_anthropic_request();
    request["output_config"] = json!({
        "format": {"type": "json_schema", "schema": {"type": "object"}}
    });
    let unified = anthropic().to_unified(request).expect("to unified");
    let rf = unified
        .response_format
        .as_ref()
        .expect("json_schema 必须入 IR");
    assert_eq!(rf["type"], "json_schema");
    assert_eq!(rf["json_schema"]["name"], "response");
    assert_eq!(rf["json_schema"]["schema"]["type"], "object");

    // 缺 schema → 显性丢弃
    let mut request = base_anthropic_request();
    request["output_config"] = json!({"format": {"type": "json_schema"}});
    let unified = anthropic().to_unified(request).expect("to unified");
    assert!(
        unified.response_format.is_none(),
        "缺 schema 必须丢弃且不报错"
    );
}

/// Responses 出站请求全分支：System 逐条合并进 instructions、
/// Tool 双形态、ToolCalls、TextWithToolCalls、普通消息。
#[test]
fn responses_converter_request_branches() {
    use fluxweir::protocol::{
        Message, MessageContent, ResponsesConverter, Role, ToolCall, ToolCallFunction, ToolResult,
        UnifiedRequest,
    };
    let converter = ResponsesConverter::new();
    let request = UnifiedRequest {
        model: "m".into(),
        system_prompt: Some("base".into()),
        messages: vec![
            Message {
                role: Role::System,
                content: MessageContent::Text("extra".into()),
            },
            Message {
                role: Role::System,
                content: MessageContent::Text("extra".into()),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "call_9".into(),
                    function_name: "fn".into(),
                    content: "42".into(),
                    is_error: false,
                }),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::Text("loose".into()),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(vec![ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "get_weather".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::TextWithToolCalls {
                    text: "calling".into(),
                    tool_calls: vec![ToolCall {
                        id: "call_2".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: "lookup".into(),
                            arguments: "[]".into(),
                        },
                    }],
                    reasoning: None,
                },
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("hi".into()),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Text("done".into()),
            },
        ],
        ..Default::default()
    };
    let external = converter.request_to_external(request).expect("to external");
    // System 逐条合并（去重仅当 instructions 整串与该条恰好相等）
    assert_eq!(external["instructions"], "base\nextra\nextra");
    let input = external["input"].as_array().expect("input 数组");
    let dumped = serde_json::to_string(&input).unwrap();
    assert!(
        dumped.contains(r#""type":"function_call_output""#),
        "ToolResult 形态: {dumped}"
    );
    assert!(dumped.contains("call_9"), "call_id 保留: {dumped}");
    assert!(dumped.contains("42"), "结果内容: {dumped}");
    // loose Tool 文本 → call_id 空串的 function_call_output
    assert!(
        input
            .iter()
            .any(|i| i["type"] == "function_call_output" && i["call_id"] == ""),
        "非 ToolResult 的 Tool 消息兜底: {dumped}"
    );
    assert!(dumped.contains("get_weather"), "ToolCalls item: {dumped}");
    assert!(
        dumped.contains("lookup"),
        "TextWithToolCalls item: {dumped}"
    );
    assert!(
        input
            .iter()
            .any(|i| i["role"] == "assistant" && i["content"] == "calling"),
        "TextWithToolCalls 文本先行: {dumped}"
    );
    assert!(
        dumped.contains("hi") && dumped.contains("done"),
        "普通消息: {dumped}"
    );
}

/// Responses 入站校验边界：metadata 超长 / background+stream / file/audio /
/// item_reference 缺 id / 无 role 无 type。
#[test]
fn responses_convert_in_validation_edges() {
    use fluxweir::protocol::ResponsesConverter;
    let converter = ResponsesConverter::new();

    let mut body = json!({
        "model": "m",
        "input": "hi",
        "max_output_tokens": 16,
        "metadata": {"k": "v"},
    });
    body["metadata"] = serde_json::Map::from_iter([("k".into(), json!("x".repeat(7000)))]).into();
    let err = converter
        .to_unified(body.clone())
        .expect_err("value 超长必须拒绝");
    assert!(err.to_string().contains("metadata"), "{err}");
    body["metadata"] = serde_json::Map::from_iter([("k".repeat(7000), json!("v"))]).into();
    assert!(
        converter.to_unified(body.clone()).is_err(),
        "key 超长必须拒绝"
    );

    let err = converter
        .to_unified(json!({
            "model": "m", "input": "hi", "max_output_tokens": 16,
            "background": true, "stream": true,
        }))
        .expect_err("background+stream 拒绝");
    assert!(err.to_string().contains("background"), "{err}");

    for bad_type in ["input_file", "input_audio"] {
        let err = converter
            .to_unified(json!({
                "model": "m", "max_output_tokens": 16,
                "input": [{"type": bad_type}],
            }))
            .expect_err("file/audio 必须拒绝");
        assert!(err.to_string().contains("native"), "{err}");
    }

    assert!(
        converter
            .to_unified(json!({
                "model": "m", "max_output_tokens": 16,
                "input": [{"type": "item_reference"}],
            }))
            .is_err(),
        "item_reference 缺 id 必须拒绝"
    );
    assert!(
        converter
            .to_unified(json!({
                "model": "m", "max_output_tokens": 16,
                "input": [{"foo": 1}],
            }))
            .is_err(),
        "无 role 无已知 type 必须拒绝"
    );
}

/// Responses 单 chunk 事件形态（trait 完整性实现）：Content/ToolCall/Usage。
#[test]
fn responses_converter_stream_chunk_shapes() {
    use fluxweir::protocol::{MessageContent, ResponsesConverter, StreamChunk};
    let converter = ResponsesConverter::new();

    let event = converter
        .stream_chunk_to_external(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("he".into()),
            finish_reason: None,
            logprobs: None,
        })
        .expect("content event");
    assert_eq!(event["type"], "response.output_text.delta");
    assert_eq!(event["delta"], "he");

    let event = converter
        .stream_chunk_to_external(StreamChunk::ToolCall {
            index: 0,
            id: "fc_1".into(),
            name: Some("fn".into()),
            arguments: Some(r#"{"a":1}"#.into()),
        })
        .expect("toolcall event");
    assert_eq!(event["type"], "response.function_call_arguments.delta");
    assert_eq!(event["item_id"], "fc_1");
    assert_eq!(event["delta"], r#"{"a":1}"#);

    let event = converter
        .stream_chunk_to_external(StreamChunk::Usage {
            prompt_tokens: 3,
            completion_tokens: 1,
            total_tokens: 4,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_1h_tokens: 0,
            finish_reason: None,
        })
        .expect("usage event");
    assert_eq!(event["type"], "response.completed");
    assert_eq!(event["response"]["usage"]["total_tokens"], 4);
}

/// Gemini 出站请求：system/user/model 角色、ToolCalls→functionCall parts、
/// 空 contents 显性拒绝。
#[test]
fn gemini_request_converts_roles_tools_and_system() {
    use fluxweir::protocol::gemini::convert_out::request_to_gemini;
    use fluxweir::protocol::{
        Message, MessageContent, Role, ToolCall, ToolCallFunction, ToolResult, UnifiedRequest,
    };
    let request = UnifiedRequest {
        model: "gemini-2.5-pro".into(),
        system_prompt: Some("be brief".into()),
        messages: vec![
            Message {
                role: Role::User,
                content: MessageContent::Text("hi".into()),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "fn".into(),
                        arguments: "{}".into(),
                    },
                }]),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "c1".into(),
                    function_name: "fn".into(),
                    content: "42".into(),
                    is_error: false,
                }),
            },
        ],
        ..Default::default()
    };
    let body = request_to_gemini(&request).expect("to gemini");
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"], "be brief",
        "system 指令"
    );
    let dumped = body["contents"].to_string();
    assert!(dumped.contains(r#""role":"user""#), "user 角色: {dumped}");
    assert!(
        dumped.contains(r#""role":"model""#),
        "assistant→model: {dumped}"
    );
    assert!(
        dumped.contains("functionCall"),
        "tool_calls→functionCall: {dumped}"
    );
    assert!(
        dumped.contains("functionResponse"),
        "tool_result→functionResponse: {dumped}"
    );

    // 空消息集：contents 为空必须显性拒绝
    let empty = UnifiedRequest {
        model: "m".into(),
        messages: vec![],
        ..Default::default()
    };
    assert!(request_to_gemini(&empty).is_err(), "空 contents 拒绝");
}

/// Gemini 响应解析与回写：正常 candidates、promptFeedback 阻断路径、
/// usage/cache 字段。
#[test]
fn gemini_response_shapes_and_block_feedback() {
    use fluxweir::protocol::gemini::convert_out::{parse_gemini_response, response_to_gemini};

    let unified = parse_gemini_response(
        &json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hello"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5,
                              "cachedContentTokenCount": 1},
            "modelVersion": "gemini-2.5-pro"
        }),
        "fallback-m",
    )
    .expect("parse");
    assert_eq!(unified.usage.prompt_tokens, 3);
    assert_eq!(unified.usage.cache_read_tokens, 1);
    let external = response_to_gemini(&unified);
    assert_eq!(external["usageMetadata"]["cachedContentTokenCount"], 1);
    assert_eq!(external["candidates"][0]["finishReason"], "STOP");

    // promptFeedback 阻断（无 candidates）：不报错，映射为空消息 + 过滤终止
    let blocked = parse_gemini_response(
        &json!({
            "promptFeedback": {"blockReason": "SAFETY"},
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 0, "totalTokenCount": 1}
        }),
        "m",
    )
    .expect("阻断也必须可解析");
    assert!(
        blocked
            .choices
            .iter()
            .all(|c| c.finish_reason.as_deref() != Some("stop")),
        "阻断终止原因非 stop: {:?}",
        blocked
            .choices
            .first()
            .and_then(|c| c.finish_reason.clone())
    );

    // 畸形响应（无 candidates 无 feedback）显性拒绝
    assert!(
        parse_gemini_response(&json!({}), "m").is_err(),
        "空响应拒绝"
    );
}

/// Gemini 流帧解析与编码：text/part 变体入 IR、Usage 结算、编码器出
/// Gemini SSE 事件（无 data: [DONE]）。
#[test]
fn gemini_stream_frame_parse_and_encode() {
    use fluxweir::protocol::gemini::stream::parse_stream_frame;
    use fluxweir::protocol::GeminiStreamEncoder;
    use fluxweir::protocol::{MessageContent, StreamChunk};

    let chunk = parse_stream_frame(
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"he"}]}}]}"#,
    )
    .expect("text 帧必须可解析");
    match chunk {
        StreamChunk::Content { delta, .. } => {
            assert!(
                matches!(&delta, MessageContent::Text(t) if t == "he"),
                "{delta:?}"
            );
        }
        other => panic!("text 帧必须是 Content: {other:?}"),
    }

    let usage = parse_stream_frame(
        r#"{"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"totalTokenCount":4},
            "candidates":[{"finishReason":"STOP"}]}"#,
    )
    .expect("usage 帧必须可解析");
    assert!(
        matches!(
            usage,
            StreamChunk::Usage {
                total_tokens: 4,
                ..
            }
        ),
        "{usage:?}"
    );

    assert!(
        parse_stream_frame("not json").is_none(),
        "畸形帧返回 None 不 panic"
    );

    let mut encoder = GeminiStreamEncoder::new();
    let events = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Text("yo".into()),
        finish_reason: None,
        logprobs: None,
    });
    assert!(!events.is_empty(), "content 编码产出事件");
    let dumped = serde_json::to_string(&events).unwrap();
    assert!(dumped.contains("yo"), "{dumped}");
    let tail = encoder.finish();
    assert!(
        !tail.iter().any(|e| e.to_string().contains("[DONE]")),
        "Gemini 流不发 [DONE]"
    );
}

/// Responses 入站 wire 形状边界：字段类型错报 body、空白 model、
/// input null / 非字符串非数组显式拒绝。
#[test]
fn responses_convert_in_wire_shape_edges() {
    use fluxweir::protocol::ResponsesConverter;
    let converter = ResponsesConverter::new();

    let err = converter
        .to_unified(json!({
            "model": "m",
            "input": "x",
            "max_output_tokens": "not-a-number",
        }))
        .expect_err("max_output_tokens 类型错必须拒绝");
    assert!(err.to_string().contains("body"), "{err}");

    let err = converter
        .to_unified(json!({"model": "   ", "input": "x", "max_output_tokens": 16}))
        .expect_err("空白 model 必须拒绝");
    assert!(err.to_string().contains("model"), "{err}");

    let err = converter
        .to_unified(json!({"model": "m", "input": null, "max_output_tokens": 16}))
        .expect_err("input null 必须拒绝");
    assert!(err.to_string().contains("input"), "{err}");

    let err = converter
        .to_unified(json!({"model": "m", "input": 42, "max_output_tokens": 16}))
        .expect_err("input 数字必须拒绝");
    assert!(err.to_string().contains("input"), "{err}");
}

/// Responses input 条数上限：当前回合超限与合并历史超限都显性拒绝
///（上限 10000，不静默截断）。
#[test]
fn responses_input_item_limit_and_merged_history_overflow() {
    use fluxweir::protocol::responses::{parse_request, to_unified_request};
    use fluxweir::protocol::ResponsesConverter;

    let oversized: Vec<Value> = (0..10_001)
        .map(|i| json!({"role": "user", "content": format!("m{i}")}))
        .collect();
    let err = ResponsesConverter::new()
        .to_unified(json!({"model": "m", "max_output_tokens": 16, "input": oversized}))
        .expect_err("input 超过 10000 items 必须拒绝");
    assert!(err.to_string().contains("exceeds limit"), "{err}");

    let parsed = parse_request(&json!({"model": "m", "max_output_tokens": 16, "input": "x"}))
        .expect("小请求解析");
    let merged: Vec<Value> = (0..10_001)
        .map(|i| json!({"role": "user", "content": format!("h{i}")}))
        .collect();
    let err = to_unified_request(&parsed, &merged).expect_err("合并历史超限必须拒绝");
    assert!(err.to_string().contains("exceeds limit"), "{err}");
}

/// Responses 原生直通判定与工具形状：prompt 标记 native_only、缺 type
/// 工具不判 native 但在 parse_tools 拒绝、内置工具 IR 转换显性拒绝。
#[test]
fn responses_native_only_and_tool_shape_edges() {
    use fluxweir::protocol::responses::parse_request;
    use fluxweir::protocol::ResponsesConverter;
    let converter = ResponsesConverter::new();

    let parsed = parse_request(&json!({
        "model": "m", "max_output_tokens": 16, "input": "x",
        "prompt": {"id": "pmpt_1"}
    }))
    .expect("prompt 请求可解析");
    assert_eq!(parsed.native_only.as_deref(), Some("prompt"));

    let parsed = parse_request(&json!({
        "model": "m", "max_output_tokens": 16, "input": "x",
        "tools": [{"name": "no_type"}]
    }))
    .expect("缺 type 的工具不触发 native_only");
    assert!(parsed.native_only.is_none());

    let err = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tools": [{"name": "no_type"}]
        }))
        .expect_err("缺 type 工具必须拒绝");
    assert!(err.to_string().contains("tools[0]"), "{err}");

    let err = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tools": [{"type": "web_search"}]
        }))
        .expect_err("内置工具走原生路径，IR 转换拒绝");
    assert!(err.to_string().contains("native"), "{err}");
}

/// Responses 函数调用历史边界：arguments 对象序列化、重复 output 拒绝、
/// reasoning 项丢弃、item_reference 必须先解析、output 数组/缺省/标量。
#[test]
fn responses_function_history_edges() {
    use fluxweir::protocol::{MessageContent, ResponsesConverter};
    let converter = ResponsesConverter::new();

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [
                {"role": "user", "content": "q"},
                {"type": "function_call", "call_id": "c1", "name": "f",
                 "arguments": {"a": 1}},
                {"type": "function_call_output", "call_id": "c1",
                 "output": [
                     {"type": "output_text", "text": "part1"},
                     "raw tail",
                     {"type": "skipped"}
                 ]}
            ]
        }))
        .expect("arguments 对象可转换");
    assert!(
        matches!(
            &unified.messages[1].content,
            MessageContent::ToolCalls(calls)
                if calls[0].function.arguments == "{\"a\":1}"
        ),
        "{:?}",
        unified.messages[1].content
    );
    assert!(
        matches!(
            &unified.messages[2].content,
            MessageContent::ToolResult(r) if r.content == "part1\nraw tail"
        ),
        "{:?}",
        unified.messages[2].content
    );

    let err = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "a"},
                {"type": "function_call_output", "call_id": "c1", "output": "b"}
            ]
        }))
        .expect_err("重复 output 必须拒绝");
    assert!(err.to_string().contains("duplicate"), "{err}");

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": "q"}, {"type": "reasoning"}]
        }))
        .expect("reasoning 项跳过");
    assert_eq!(unified.messages.len(), 1, "reasoning 不产消息");

    let err = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"type": "item_reference", "id": "msg_9"}]
        }))
        .expect_err("item_reference 必须先解析");
    assert!(err.to_string().contains("item_reference"), "{err}");

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1"}
            ]
        }))
        .expect("缺省 output 可转换");
    assert!(matches!(
        &unified.messages[1].content,
        MessageContent::ToolResult(r) if r.content.is_empty()
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": 42}
            ]
        }))
        .expect("数字 output 可转换");
    assert!(matches!(
        &unified.messages[1].content,
        MessageContent::ToolResult(r) if r.content == "42"
    ));
}

/// Responses content blocks：未知块型拒绝、无 type 块跳过、单文本块折叠、
/// null/标量 content、直链图片走 Url 形态。
#[test]
fn responses_content_block_edges() {
    use fluxweir::protocol::{ContentBlock, ImageSource, MessageContent, ResponsesConverter};
    let converter = ResponsesConverter::new();

    let err = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": [{"type": "summary_text", "text": "x"}]}]
        }))
        .expect_err("未知块型在 chat 上游拒绝");
    assert!(err.to_string().contains("summary_text"), "{err}");

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": [{"text": "no-type-block"}]}]
        }))
        .expect("无 type 块跳过");
    assert!(matches!(
        &unified.messages[0].content,
        MessageContent::Text(t) if t.is_empty()
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "only"}]}]
        }))
        .expect("单文本块折叠");
    assert!(matches!(
        &unified.messages[0].content,
        MessageContent::Text(t) if t == "only"
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user"}]
        }))
        .expect("content 缺省为空文本");
    assert!(matches!(
        &unified.messages[0].content,
        MessageContent::Text(t) if t.is_empty()
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": 7}]
        }))
        .expect("标量 content 文本化");
    assert!(matches!(
        &unified.messages[0].content,
        MessageContent::Text(t) if t == "7"
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16,
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": "https://example.com/a.png"},
                {"type": "input_image", "image_url": "data:application/pdf,AAAA"},
                {"type": "input_image", "image_url": "data:nocomma"}
            ]}]
        }))
        .expect("直链图片可转换");
    assert!(
        matches!(
            &unified.messages[0].content,
            MessageContent::MultiModal(blocks)
                if matches!(
                    &blocks[1],
                    ContentBlock::Image {
                        source: ImageSource::Url { url }, ..
                    } if url == "https://example.com/a.png"
                ) && matches!(
                    &blocks[2],
                    ContentBlock::Image {
                        source: ImageSource::Url { url }, ..
                    } if url == "data:application/pdf,AAAA"
                ) && matches!(
                    &blocks[3],
                    ContentBlock::Image {
                        source: ImageSource::Url { url }, ..
                    } if url == "data:nocomma"
                )
        ),
        "{:?}",
        unified.messages[0].content
    );
}

/// Responses 工具与杂项槽：nested function 工具、tool_choice 四形态、
/// text.format 非 json_schema 原样、metadata/service_tier 等进
/// vendor_extensions。
#[test]
fn responses_tools_choice_and_vendor_extras() {
    use fluxweir::protocol::{ResponsesConverter, ToolChoice};
    let converter = ResponsesConverter::new();

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tools": [{
                "type": "function",
                "function": {"name": "f", "description": "d",
                             "parameters": {"type": "object"}, "strict": true}
            }],
            "metadata": {"trace": "t1"},
            "service_tier": "priority",
            "safety_identifier": "u1",
            "max_tool_calls": 5,
            "include": ["reasoning.encrypted_content"],
            "truncation": "auto"
        }))
        .expect("nested 工具可转换");
    assert_eq!(unified.tools[0].function.name, "f");
    assert_eq!(unified.tools[0].function.strict, Some(true));
    assert_eq!(
        unified.vendor_extensions.get("metadata").unwrap()["trace"],
        "t1"
    );
    assert_eq!(
        unified.vendor_extensions.get("service_tier").unwrap(),
        "priority"
    );
    assert_eq!(unified.vendor_extensions.get("truncation").unwrap(), "auto");

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tool_choice": "required"
        }))
        .expect("字符串 tool_choice");
    assert!(matches!(
        unified.tool_choice,
        Some(ToolChoice::Strategy(ref s)) if s == "required"
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tool_choice": {"type": "function", "function": {"name": "nested_f"}}
        }))
        .expect("nested function.name 兜底");
    assert!(matches!(
        unified.tool_choice,
        Some(ToolChoice::Specific { ref function, .. }) if function.name == "nested_f"
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "tool_choice": 5
        }))
        .expect("非法 tool_choice 回落 auto");
    assert!(matches!(
        unified.tool_choice,
        Some(ToolChoice::Strategy(ref s)) if s == "auto"
    ));

    let unified = converter
        .to_unified(json!({
            "model": "m", "max_output_tokens": 16, "input": "x",
            "text": {"format": {"type": "text"}}
        }))
        .expect("text format 原样");
    assert_eq!(unified.response_format.unwrap()["type"], "text");
}

/// Gemini 出站工具/奇形消息：名字缺失拒绝、Tool 文本消息跳过、
/// TextWithToolCalls 名字回查、Reasoning 丢弃、assistant ToolResult 直映射。
#[test]
fn gemini_request_tool_result_shapes() {
    use fluxweir::protocol::gemini::convert_out::request_to_gemini;
    use fluxweir::protocol::{
        Message, MessageContent, Role, ToolCall, ToolCallFunction, ToolResult, UnifiedRequest,
    };

    fn with(messages: Vec<Message>) -> UnifiedRequest {
        UnifiedRequest {
            model: "gemini-2.5-pro".into(),
            messages,
            ..Default::default()
        }
    }
    fn user() -> Message {
        Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }
    }

    let err = request_to_gemini(&with(vec![
        user(),
        Message {
            role: Role::Tool,
            content: MessageContent::ToolResult(ToolResult {
                tool_call_id: "ghost".into(),
                function_name: String::new(),
                content: "42".into(),
                is_error: false,
            }),
        },
    ]))
    .expect_err("无名 functionResponse 必须拒绝");
    assert!(
        matches!(err, fluxweir::GateError::Validation { .. }),
        "{err}"
    );

    let body = request_to_gemini(&with(vec![
        user(),
        Message {
            role: Role::Tool,
            content: MessageContent::Text("junk".into()),
        },
    ]))
    .expect("Tool 文本消息跳过");
    assert_eq!(body["contents"].as_array().unwrap().len(), 1, "{body}");

    let call = ToolCall {
        id: "c1".into(),
        call_type: "function".into(),
        function: ToolCallFunction {
            name: "weather".into(),
            arguments: "{}".into(),
        },
    };
    let body = request_to_gemini(&with(vec![
        user(),
        Message {
            role: Role::Assistant,
            content: MessageContent::TextWithToolCalls {
                text: "calling".into(),
                tool_calls: vec![call],
                reasoning: None,
            },
        },
        Message {
            role: Role::Tool,
            content: MessageContent::ToolResult(ToolResult {
                tool_call_id: "c1".into(),
                function_name: String::new(),
                content: "sunny".into(),
                is_error: false,
            }),
        },
    ]))
    .expect("TextWithToolCalls 可转换");
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3, "{body}");
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["text"], "calling");
    assert_eq!(contents[1]["parts"][1]["functionCall"]["name"], "weather");
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"]["name"], "weather",
        "名字按 call_id 回查: {body}"
    );

    let body = request_to_gemini(&with(vec![
        Message {
            role: Role::Assistant,
            content: MessageContent::Reasoning {
                text: "think".into(),
                signature: None,
            },
        },
        user(),
    ]))
    .expect("Reasoning 消息丢弃");
    assert_eq!(body["contents"].as_array().unwrap().len(), 1, "{body}");

    let body = request_to_gemini(&with(vec![
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolResult(ToolResult {
                tool_call_id: "c2".into(),
                function_name: "fn".into(),
                content: "ok".into(),
                is_error: false,
            }),
        },
        user(),
    ]))
    .expect("assistant ToolResult 可转换");
    assert_eq!(body["contents"][0]["role"], "model", "{body}");
    assert_eq!(
        body["contents"][0]["parts"][0]["functionResponse"]["name"],
        "fn"
    );
}

/// Gemini system 多块聚合与未知块降级：MultiModal 文本拼接、非文本
/// system 忽略、未知块型文本占位、jpg/webp/gif MIME 推断。
#[test]
fn gemini_request_system_blocks_and_mime() {
    use fluxweir::protocol::gemini::convert_out::request_to_gemini;
    use fluxweir::protocol::{
        ContentBlock, ImageSource, Message, MessageContent, Role, ToolCall, ToolCallFunction,
        UnifiedRequest,
    };

    fn with(messages: Vec<Message>) -> UnifiedRequest {
        UnifiedRequest {
            model: "m".into(),
            messages,
            ..Default::default()
        }
    }
    fn user() -> Message {
        Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }
    }

    let body = request_to_gemini(&with(vec![
        Message {
            role: Role::System,
            content: MessageContent::MultiModal(vec![
                ContentBlock::Text { text: "s1".into() },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://x/s.png".into(),
                    },
                    detail: None,
                },
                ContentBlock::Text { text: "s2".into() },
            ]),
        },
        user(),
    ]))
    .expect("system 多文本块拼接");
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"], "s1s2",
        "非文本块不参与拼接: {body}"
    );

    let body = request_to_gemini(&with(vec![
        Message {
            role: Role::System,
            content: MessageContent::ToolCalls(vec![ToolCall {
                id: "c1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            }]),
        },
        user(),
    ]))
    .expect("非文本 system 忽略");
    assert!(body.get("systemInstruction").is_none(), "{body}");

    let body = request_to_gemini(&with(vec![Message {
        role: Role::User,
        content: MessageContent::MultiModal(vec![ContentBlock::Type("video".into())]),
    }]))
    .expect("未知块降级");
    let text = body["contents"][0]["parts"][0]["text"].as_str().unwrap();
    assert!(text.contains("video"), "占位文本含原块信息: {text}");

    for (url, mime) in [
        ("https://x/a.jpg", "image/jpeg"),
        ("https://x/a.webp", "image/webp"),
        ("https://x/a.gif", "image/gif"),
    ] {
        let body = request_to_gemini(&with(vec![Message {
            role: Role::User,
            content: MessageContent::MultiModal(vec![ContentBlock::Image {
                source: ImageSource::Url { url: url.into() },
                detail: None,
            }]),
        }]))
        .expect("url 图片");
        assert_eq!(
            body["contents"][0]["parts"][0]["fileData"]["mimeType"], mime,
            "{url}"
        );
    }
}

/// Gemini generationConfig 与 toolConfig 全槽：采样参数、stop、惩罚、
/// null 参数工具省略 parameters、tool_choice 策略词、response_format 三形态。
#[test]
fn gemini_request_sampling_and_tool_config() {
    use fluxweir::protocol::gemini::convert_out::request_to_gemini;
    use fluxweir::protocol::{
        Message, MessageContent, Role, SamplingParams, Tool, ToolChoice, ToolFunction,
        UnifiedRequest,
    };

    let mut request = UnifiedRequest {
        model: "m".into(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
        }],
        temperature: Some(0.7),
        top_p: Some(0.9),
        stop: vec!["END".into()],
        sampling: Some(SamplingParams {
            top_k: Some(40),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            ..Default::default()
        }),
        tools: vec![
            Tool {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "full".into(),
                    description: "d".into(),
                    parameters: json!({"type": "object"}),
                    strict: None,
                },
                ..Default::default()
            },
            Tool {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "bare".into(),
                    description: String::new(),
                    parameters: Value::Null,
                    strict: None,
                },
                ..Default::default()
            },
        ],
        tool_choice: Some(ToolChoice::Strategy("none".into())),
        ..Default::default()
    };
    let body = request_to_gemini(&request).expect("全槽请求");
    let gen = &body["generationConfig"];
    for (key, want) in [
        ("temperature", 0.7f64),
        ("topP", 0.9),
        ("frequencyPenalty", 0.1),
        ("presencePenalty", 0.2),
    ] {
        let got = gen[key].as_f64().unwrap();
        assert!((got - want).abs() < 1e-6, "{key}: {got}");
    }
    assert_eq!(gen["topK"], 40);
    assert_eq!(gen["stopSequences"][0], "END");
    let decls = body["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert!(decls[0].get("parameters").is_some(), "带参工具: {decls:?}");
    assert!(
        decls[1].get("parameters").is_none(),
        "null 参数省略: {decls:?}"
    );
    assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "NONE");

    request.tool_choice = Some(ToolChoice::Strategy("required".into()));
    let body = request_to_gemini(&request).expect("required 策略");
    assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");

    request.tool_choice = Some(ToolChoice::Strategy("banana".into()));
    let body = request_to_gemini(&request).expect("未知策略不写 toolConfig");
    assert!(body.get("toolConfig").is_none(), "{body}");

    request.tool_choice = None;
    request.response_format = Some(json!({"type": "json_schema"}));
    let body = request_to_gemini(&request).expect("无 schema 的 json_schema");
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert!(
        body["generationConfig"].get("responseSchema").is_none(),
        "{}",
        body
    );

    request.response_format = Some(json!({"type": "json_object"}));
    let body = request_to_gemini(&request).expect("json_object");
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );

    request.response_format = Some(json!({"type": "text"}));
    let body = request_to_gemini(&request).expect("text format 无 mime");
    assert!(
        body.get("generationConfig").is_none()
            || body["generationConfig"].get("responseMimeType").is_none(),
        "{}",
        body
    );
}

/// Gemini 响应候选边角：无 content/空 parts、inlineData/fileData、
/// 无 args functionCall、finishReason 词表、回写 message 缺省与 SAFETY。
#[test]
fn gemini_response_candidate_edges() {
    use fluxweir::protocol::gemini::convert_out::{parse_gemini_response, response_to_gemini};
    use fluxweir::protocol::{
        Choice, ContentBlock, FinishReason, ImageSource, Message, MessageContent, Role, TokenUsage,
        UnifiedResponse,
    };

    let unified = parse_gemini_response(
        &json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "look"},
                    {"inlineData": {"mimeType": "image/png", "data": "AA"}},
                    {"fileData": {"fileUri": "gs://b/f.png"}},
                    {}
                ]},
                "finishReason": "STOP"
            }]
        }),
        "m",
    )
    .expect("混合 parts 可解析");
    let message = unified.choices[0].message.as_ref().unwrap();
    assert!(
        matches!(
            &message.content,
            MessageContent::MultiModal(blocks)
                if matches!(&blocks[0], ContentBlock::Text { text } if text == "look")
                    && matches!(
                        &blocks[1],
                        ContentBlock::Image {
                            source: ImageSource::Base64 { media_type, .. }, ..
                        } if media_type == "image/png"
                    )
                    && matches!(
                        &blocks[2],
                        ContentBlock::Image {
                            source: ImageSource::Url { url }, ..
                        } if url == "gs://b/f.png"
                    )
        ),
        "{:?}",
        message.content
    );

    // candidates 类型错 → 反序列化失败，整体显性拒绝
    assert!(
        parse_gemini_response(&json!({"candidates": 5}), "m").is_err(),
        "畸形 candidates 拒绝"
    );

    let unified = parse_gemini_response(
        &json!({"candidates": [{"content": {"role": "model", "parts": [
            {"inlineData": {"mimeType": "image/jpeg", "data": "BB"}}
        ]}}]}),
        "m",
    )
    .expect("纯图可解析");
    let message = unified.choices[0].message.as_ref().unwrap();
    assert!(
        matches!(
            &message.content,
            MessageContent::MultiModal(blocks) if blocks.len() == 1
                && matches!(&blocks[0], ContentBlock::Image { .. })
        ),
        "{:?}",
        message.content
    );

    let unified = parse_gemini_response(
        &json!({"candidates": [{"index": 0, "finishReason": "STOP"}]}),
        "m",
    )
    .expect("无 content 候选可解析");
    let choice = &unified.choices[0];
    assert!(
        matches!(
            choice.message.as_ref().unwrap().content,
            MessageContent::Text(ref t) if t.is_empty()
        ),
        "{:?}",
        choice.message
    );

    let unified = parse_gemini_response(
        &json!({"candidates": [{"content": {"role": "model", "parts": []},
                                 "finishReason": "STOP"}]}),
        "m",
    )
    .expect("空 parts 可解析");
    assert!(matches!(
        unified.choices[0].message.as_ref().unwrap().content,
        MessageContent::Text(ref t) if t.is_empty()
    ));

    let unified = parse_gemini_response(
        &json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "x"}]}}]}),
        "m",
    )
    .expect("无 finishReason 可解析");
    let choice = &unified.choices[0];
    assert!(choice.finish.is_none() && choice.finish_reason.is_none());

    let unified = parse_gemini_response(
        &json!({"candidates": [{
            "content": {"role": "model", "parts": [{"functionCall": {"name": "f"}}]},
            "finishReason": "MALFORMED_FUNCTION_CALL"
        }]}),
        "m",
    )
    .expect("无 args functionCall 可解析");
    let choice = &unified.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    assert!(choice.finish.is_none());
    match &choice.message.as_ref().unwrap().content {
        MessageContent::TextWithToolCalls { tool_calls, .. } => {
            assert_eq!(tool_calls[0].function.arguments, "{}");
        }
        other => panic!("期望 TextWithToolCalls: {other:?}"),
    }

    let unified = parse_gemini_response(
        &json!({"candidates": [{
            "content": {"role": "model", "parts": [{"text": "a"}]},
            "finishReason": "WEIRD"
        }]}),
        "m",
    )
    .expect("未知 finishReason 保真");
    let choice = &unified.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("WEIRD"));
    assert!(choice.finish.is_none());

    let response = UnifiedResponse {
        id: "r1".into(),
        object: "chat.completion".into(),
        created: 1,
        model: "m".into(),
        choices: vec![Choice {
            index: 0,
            message: None,
            finish_reason: None,
            finish: None,
            logprobs: None,
        }],
        usage: TokenUsage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            ..Default::default()
        },
        stop_sequence: None,
    };
    let body = response_to_gemini(&response);
    assert_eq!(body["candidates"][0]["finishReason"], "STOP");
    assert!(body["candidates"][0]["content"]["parts"]
        .as_array()
        .unwrap()
        .is_empty());

    let response = UnifiedResponse {
        id: "r2".into(),
        object: "chat.completion".into(),
        created: 1,
        model: "m".into(),
        choices: vec![Choice {
            index: 0,
            message: Some(Message {
                role: Role::Assistant,
                content: MessageContent::Text("hi".into()),
            }),
            finish_reason: None,
            finish: Some(FinishReason::ContentFilter),
            logprobs: None,
        }],
        usage: TokenUsage::default(),
        stop_sequence: None,
    };
    let body = response_to_gemini(&response);
    assert_eq!(body["candidates"][0]["finishReason"], "SAFETY");
}

/// Gemini 流帧多形态与编码器变体：不可映射帧 None、多工具调用聚合、
/// 思考增量丢弃、ToolResult 帧、未知终止词 OTHER、SAFETY 入站映射。
#[test]
fn gemini_stream_parse_and_encoder_variants() {
    use fluxweir::protocol::gemini::stream::parse_stream_frame;
    use fluxweir::protocol::{
        ContentBlock, GeminiStreamEncoder, ImageSource, MessageContent, StreamChunk, ToolCall,
        ToolCallFunction, ToolResult,
    };

    assert!(
        parse_stream_frame(
            r#"{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":"AA"}}]}}]}"#
        )
        .is_none(),
        "无文本无终止的不可映射帧返回 None"
    );

    let chunk =
        parse_stream_frame(r#"{"candidates":[{"finishReason":"STOP"}]}"#).expect("无 content 帧");
    assert!(
        matches!(
            &chunk,
            StreamChunk::Content {
                delta: MessageContent::Text(ref t),
                finish_reason: Some(ref f),
                ..
            } if t.is_empty() && f == "stop"
        ),
        "{chunk:?}"
    );

    let chunk = parse_stream_frame(
        r#"{"candidates":[{"index":2,"content":{"parts":[
            {"functionCall":{"name":"a"}},{"functionCall":{"name":"b"}}]}}]}"#,
    )
    .expect("单帧双工具调用");
    assert!(
        matches!(
            &chunk,
            StreamChunk::Content {
                index: 2,
                delta: MessageContent::ToolCalls(ref calls),
                ..
            } if calls.len() == 2
        ),
        "{chunk:?}"
    );

    let chunk = parse_stream_frame(
        &json!({"candidates": [{"content": {"parts": [
            {"text": "ans"},
            {"functionCall": {"name": "f", "args": {"x": 1}}}
        ]}}]})
        .to_string(),
    )
    .expect("文本 + 工具调用同帧");
    assert!(
        matches!(
            &chunk,
            StreamChunk::Content {
                delta: MessageContent::TextWithToolCalls {
                    text, tool_calls, ..
                },
                ..
            } if text == "ans" && tool_calls.len() == 1
        ),
        "{chunk:?}"
    );

    let chunk = parse_stream_frame(
        r#"{"candidates":[{"content":{"parts":[{"text":"a"}]},"finishReason":"SAFETY"}]}"#,
    )
    .expect("SAFETY 帧可解析");
    assert!(
        matches!(
            &chunk,
            StreamChunk::Content { finish_reason: Some(ref f), .. } if f == "content_filter"
        ),
        "{chunk:?}"
    );

    let mut encoder = GeminiStreamEncoder::default();
    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::MultiModal(vec![
            ContentBlock::Text { text: "v".into() },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "AA".into(),
                },
                detail: None,
            },
        ]),
        finish_reason: None,
        logprobs: None,
    });
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][0]["text"],
        "v"
    );
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );

    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::ToolCalls(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "f".into(),
                arguments: "{\"x\":9}".into(),
            },
        }]),
        finish_reason: None,
        logprobs: None,
    });
    assert!(frames.is_empty(), "工具片段先缓冲");

    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Text(String::new()),
        finish_reason: Some("tool_calls".into()),
        logprobs: None,
    });
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][0]["functionCall"]["name"],
        "f"
    );
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["x"],
        9
    );
    assert_eq!(frames[0]["candidates"][0]["finishReason"], "STOP");

    let mut encoder = GeminiStreamEncoder::new();
    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::TextWithToolCalls {
            text: "calling".into(),
            tool_calls: vec![ToolCall {
                id: "c2".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "g".into(),
                    arguments: "{}".into(),
                },
            }],
            reasoning: None,
        },
        finish_reason: None,
        logprobs: None,
    });
    assert_eq!(frames.len(), 1, "文本帧即时发出");
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][0]["text"],
        "calling"
    );
    let frames = encoder.finish();
    assert!(
        frames
            .iter()
            .any(|f| f["candidates"][0]["content"]["parts"][0]["functionCall"]["name"] == "g"),
        "{frames:?}"
    );

    let mut encoder = GeminiStreamEncoder::new();
    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::ToolResult(ToolResult {
            tool_call_id: "c1".into(),
            function_name: "fn".into(),
            content: "{\"ok\":true}".into(),
            is_error: false,
        }),
        finish_reason: None,
        logprobs: None,
    });
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["candidates"][0]["content"]["parts"][0]["functionResponse"]["response"]["ok"],
        true
    );

    let frames = encoder.encode(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Reasoning {
            text: "hidden".into(),
            signature: None,
        },
        finish_reason: None,
        logprobs: None,
    });
    assert!(frames.is_empty(), "思考帧无 Gemini 对应，不产帧");

    let mut encoder = GeminiStreamEncoder::new();
    let frames = encoder.encode(StreamChunk::Usage {
        prompt_tokens: 1,
        completion_tokens: 1,
        total_tokens: 2,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        cache_creation_1h_tokens: 0,
        finish_reason: Some("bizarre".into()),
    });
    assert!(
        frames
            .iter()
            .any(|f| f["candidates"][0]["finishReason"] == "OTHER"),
        "{frames:?}"
    );
}

/// OpenAI 流编码器与片段解析边角：Default 等价、无 id 帧原样、
/// 无内容首帧不补 role、退化工具片段、非对象 tool_call 回退 None。
#[test]
fn openai_stream_encoder_and_fragment_edges() {
    use fluxweir::protocol::{OpenAIStreamEncoder, StreamChunk};

    let mut encoder = OpenAIStreamEncoder::default();
    let frame = json!({
        "id": "chunk-1",
        "object": "chat.completion.chunk",
        "created": 1,
        "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]
    });
    let normalized = encoder.normalize(frame);
    let id = normalized["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("chatcmpl-"), "流 id 收敛: {id}");
    assert_eq!(
        normalized["choices"][0]["delta"]["role"], "assistant",
        "首内容帧补 role"
    );

    let again = encoder.normalize(json!({
        "id": "chunk-2", "created": 2,
        "choices": [{"index": 0, "delta": {"content": "yo"}, "finish_reason": null}]
    }));
    assert_eq!(again["id"].as_str().unwrap(), id, "全流 id 固定");
    assert!(
        again["choices"][0]["delta"].get("role").is_none(),
        "role 只补一次"
    );

    let raw = json!({":": ""});
    let out = encoder.normalize(raw.clone());
    assert_eq!(out, raw, "非 chunk 帧不归一");

    let mut encoder = OpenAIStreamEncoder::new();
    let out = encoder.normalize(json!({
        "id": "x", "created": 1,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    }));
    assert!(
        out["choices"][0]["delta"].get("role").is_none(),
        "无内容 delta 不补 role: {out}"
    );

    let mut encoder = OpenAIStreamEncoder::new();
    let out = encoder.normalize(json!({
        "id": "y", "created": 1, "choices": []
    }));
    assert_eq!(out["choices"].as_array().unwrap().len(), 0, "{out}");

    let out = openai()
        .stream_chunk_to_external(StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: None,
        })
        .expect("退化片段可编码");
    let fragment = &out["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(fragment["index"], 0);
    assert_eq!(fragment["type"], "function");
    assert!(fragment.get("id").is_none(), "{fragment}");
    assert!(fragment.get("function").is_none(), "{fragment}");

    assert!(
        openai()
            .parse_stream_frame(r#"{"choices":[{"index":0,"delta":{"tool_calls":["oops"]}}]}"#)
            .is_none(),
        "非对象片段无载荷返回 None"
    );
}

/// OpenAI 出站：非文本 System 消息原位保留；sampling 段补缺省 flat 槽。
#[test]
fn openai_request_system_merge_and_sampling_backfill() {
    use fluxweir::protocol::{
        ContentBlock, ImageSource, Message, MessageContent, Role, SamplingParams,
    };

    let mut unified = openai()
        .to_unified(base_openai_request())
        .expect("to unified");
    unified.system_prompt = Some("base".into());
    unified.messages.insert(
        0,
        Message {
            role: Role::System,
            content: MessageContent::MultiModal(vec![
                ContentBlock::Text {
                    text: "block-sys".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://x/i.png".into(),
                    },
                    detail: None,
                },
            ]),
        },
    );
    let external = openai().request_to_external(unified).expect("to external");
    let messages = external["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{external}");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "base", "合并 system 在前");
    assert_eq!(messages[1]["role"], "system", "非文本 system 原位保留");
    assert!(messages[1]["content"].is_array(), "{}", messages[1]);

    let mut unified = openai()
        .to_unified(base_openai_request())
        .expect("to unified");
    unified.sampling = Some(SamplingParams {
        temperature: Some(0.3),
        top_p: Some(0.8),
        top_k: Some(40),
        frequency_penalty: Some(0.1),
        presence_penalty: Some(0.2),
    });
    let external = openai()
        .request_to_external(unified.clone())
        .expect("to external");
    for (key, want) in [
        ("temperature", 0.3f64),
        ("top_p", 0.8),
        ("frequency_penalty", 0.1),
        ("presence_penalty", 0.2),
    ] {
        let got = external[key].as_f64().unwrap();
        assert!((got - want).abs() < 1e-6, "{key}: {got}");
    }
    assert_eq!(external["top_k"], 40, "top_k 直出");

    // flat 惩罚槽优先：sampling 同槽不覆写
    unified.frequency_penalty = Some(0.9);
    unified.presence_penalty = Some(0.8);
    let external = openai().request_to_external(unified).expect("to external");
    for (key, want) in [("frequency_penalty", 0.9f64), ("presence_penalty", 0.8)] {
        let got = external[key].as_f64().unwrap();
        assert!((got - want).abs() < 1e-6, "{key}: {got}");
    }
}

/// OpenAI 内容块与 tool_choice 边角：非 base64 data URL 保 Url、file 直链
/// 与非 base64 data URL 落 octet-stream、Document cache_control 丢弃、
/// 缺 arguments 空串、none/banana tool_choice。
#[test]
fn openai_content_blocks_files_and_choices() {
    use fluxweir::protocol::{ContentBlock, ImageSource, MessageContent, ToolChoice};

    let unified = openai()
        .to_unified(json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "data:application/pdf,AAAA"}},
                {"type": "file", "file": {"file_data": "https://x/f.bin", "filename": "f.bin"}},
                {"type": "file", "file": {"file_data": "data:text/plain,hi"}}
            ]}]
        }))
        .expect("文件与图片块可解析");
    match &unified.messages[0].content {
        MessageContent::MultiModal(blocks) => {
            assert!(
                matches!(
                    &blocks[0],
                    ContentBlock::Image {
                        source: ImageSource::Url { url }, ..
                    } if url == "data:application/pdf,AAAA"
                ),
                "{blocks:?}"
            );
            assert!(
                matches!(
                    &blocks[1],
                    ContentBlock::Document {
                        source: ImageSource::Url { url },
                        media_type,
                        title,
                        ..
                    } if url == "https://x/f.bin"
                        && media_type == "application/octet-stream"
                        && title.as_deref() == Some("f.bin")
                ),
                "{blocks:?}"
            );
            assert!(
                matches!(
                    &blocks[2],
                    ContentBlock::Document {
                        source: ImageSource::Url { url },
                        media_type,
                        ..
                    } if url == "data:text/plain,hi"
                        && media_type == "application/octet-stream"
                ),
                "{blocks:?}"
            );
        }
        other => panic!("期望多模态: {other:?}"),
    }

    let mut unified = openai()
        .to_unified(base_openai_request())
        .expect("to unified");
    unified.messages[0].content = MessageContent::MultiModal(vec![ContentBlock::Document {
        source: ImageSource::Base64 {
            media_type: "application/pdf".into(),
            data: "QUJD".into(),
        },
        media_type: "application/pdf".into(),
        title: Some("doc.pdf".into()),
        cache_control: Some(json!({"type": "ephemeral"})),
    }]);
    let external = openai().request_to_external(unified).expect("to external");
    let part = &external["messages"][0]["content"][0];
    assert_eq!(part["type"], "file");
    assert_eq!(
        part["file"]["file_data"],
        "data:application/pdf;base64,QUJD"
    );
    assert_eq!(part["file"]["filename"], "doc.pdf");
    assert!(part["file"].get("cache_control").is_none(), "{part}");

    let unified = openai()
        .to_unified(json!({
            "model": "gpt-4",
            "messages": [{"role": "assistant", "content": null,
                "tool_calls": [{"id": "c1", "type": "function",
                                "function": {"name": "f"}}]}]
        }))
        .expect("缺 arguments 可解析");
    // 缺 arguments 的片段被整体丢弃（与流式路径的空串占位不同，
    // 静默丢弃无 drop_unsupported 日志——疑似缺陷，见交付报告）
    assert!(
        matches!(
            &unified.messages[0].content,
            MessageContent::Text(text) if text.is_empty()
        ),
        "{:?}",
        unified.messages[0].content
    );

    let mut request = base_openai_request();
    request["tool_choice"] = json!({"type": "none"});
    let unified = openai().to_unified(request).expect("none 保留");
    assert!(matches!(
        unified.tool_choice,
        Some(ToolChoice::Strategy(ref s)) if s == "none"
    ));

    let mut request = base_openai_request();
    request["tool_choice"] = json!({"type": "banana"});
    let unified = openai().to_unified(request).expect("未知形态回落 auto");
    assert!(matches!(
        unified.tool_choice,
        Some(ToolChoice::Strategy(ref s)) if s == "auto"
    ));
}

/// OpenAI usage 明细空对象视为缺省；Reasoning 带 signature 出站丢弃
/// signature 只回写 reasoning_content。
#[test]
fn openai_usage_empty_details_and_reasoning_signature() {
    use fluxweir::protocol::{MessageContent, StreamChunk};

    let unified = openai()
        .parse_response(json!({
            "id": "chatcmpl-e", "model": "gpt-4",
            "choices": [{"index": 0,
                         "message": {"role": "assistant", "content": "hi"},
                         "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6,
                "prompt_tokens_details": {},
                "completion_tokens_details": {}
            }
        }))
        .expect("空明细对象可解析");
    assert_eq!(unified.usage.cache_read_tokens, 0, "空对象视为缺省");

    let out = openai()
        .stream_chunk_to_external(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Reasoning {
                text: "th".into(),
                signature: Some("sig-1".into()),
            },
            finish_reason: None,
            logprobs: None,
        })
        .expect("reasoning 增量可编码");
    assert_eq!(out["choices"][0]["delta"]["reasoning_content"], "th");
    assert!(
        out["choices"][0]["delta"].get("signature").is_none(),
        "signature 无 OpenAI 槽"
    );
}

/// Anthropic 缓存断点策略注入；flat top_p 抑制 sampling 补位（现状语义）。
#[test]
fn anthropic_request_cache_policy_and_top_p_flat() {
    use fluxweir::config::model_catalog::CacheBreakpointPolicy;

    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.system_prompt = Some("sys".into());
    let policy = CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: 4,
        anchors: vec!["system".into()],
    };
    let external = anthropic()
        .request_to_external_with_cache_policy(unified, &policy)
        .expect("policy 请求");
    assert_eq!(
        external["system"][0]["cache_control"]["type"], "ephemeral",
        "system 字符串提升为断点块: {external}"
    );

    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.top_p = Some(0.5);
    unified.sampling = Some(fluxweir::protocol::SamplingParams {
        top_p: Some(0.9),
        top_k: Some(7),
        ..Default::default()
    });
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    // flat top_p 抑制 sampling 补位，但 flat 值本身也不写出——出站路径
    // 没有 flat top_p 写槽（疑似缺陷，见交付报告），此处按现状断言
    assert!(
        external.get("top_p").is_none(),
        "flat top_p 既不覆写也不透传: {external}"
    );
    assert_eq!(external["top_k"], 7);
}

/// Anthropic 无状态单块路径：非文本 delta 产空 text_delta；Usage 帧带
/// 缓存读写计数字段。
#[test]
fn anthropic_stream_chunk_stateless_shapes() {
    use fluxweir::protocol::{ContentBlock, ImageSource, MessageContent, StreamChunk};

    let out = anthropic()
        .stream_chunk_to_external(StreamChunk::Content {
            index: 0,
            delta: MessageContent::MultiModal(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://x/i.png".into(),
                },
                detail: None,
            }]),
            finish_reason: None,
            logprobs: None,
        })
        .expect("图片 delta 可编码");
    assert_eq!(out["delta"]["text"], "", "无状态路径只产 text_delta: {out}");

    let out = anthropic()
        .stream_chunk_to_external(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Reasoning {
                text: "t".into(),
                signature: None,
            },
            finish_reason: None,
            logprobs: None,
        })
        .expect("reasoning delta 可编码");
    assert_eq!(out["delta"]["text"], "");

    let out = anthropic()
        .stream_chunk_to_external(StreamChunk::Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens: 7,
            cache_read_tokens: 5,
            cache_creation_tokens: 6,
            cache_creation_1h_tokens: 0,
            finish_reason: Some("stop".into()),
        })
        .expect("usage 帧可编码");
    assert_eq!(out["type"], "message_delta");
    assert_eq!(out["usage"]["cache_read_input_tokens"], 5);
    assert_eq!(out["usage"]["cache_creation_input_tokens"], 6);
}

/// Anthropic 流编码器：工具片段先行 flush 时补 message_start、文本→思考
/// 块切换先闭块、signature 单独开块、思考帧携带终止即闭块。
#[test]
fn anthropic_stream_encoder_thinking_and_tools() {
    use fluxweir::protocol::{AnthropicStreamEncoder, MessageContent, StreamChunk};

    fn parse(events: &[String]) -> Vec<Value> {
        events
            .iter()
            .map(|e| serde_json::from_str(e).unwrap())
            .collect()
    }

    let mut encoder = AnthropicStreamEncoder::new();
    assert!(encoder
        .encode_chunk(StreamChunk::ToolCall {
            index: 0,
            id: "c1".into(),
            name: Some("f".into()),
            arguments: Some("{\"x\":".into()),
        })
        .is_empty());
    let events = parse(&encoder.finish());
    assert_eq!(events[0]["type"], "message_start", "{events:?}");
    assert!(
        events.iter().any(|e| e["type"] == "content_block_start"
            && e["content_block"]["type"] == "tool_use"),
        "{events:?}"
    );
    assert!(events.iter().any(|e| e["type"] == "message_stop"));

    let mut encoder = AnthropicStreamEncoder::new();
    encoder.encode_chunk(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Text("hi".into()),
        finish_reason: None,
        logprobs: None,
    });
    let events = parse(&encoder.encode_chunk(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Reasoning {
            text: "th".into(),
            signature: None,
        },
        finish_reason: None,
        logprobs: None,
    }));
    let think_start = events
        .iter()
        .position(|e| {
            e["type"] == "content_block_start" && e["content_block"]["type"] == "thinking"
        })
        .expect("思考块开启");
    assert!(
        events[..think_start]
            .iter()
            .any(|e| e["type"] == "content_block_stop"),
        "文本块先闭合: {events:?}"
    );
    assert!(
        events[think_start..]
            .iter()
            .any(|e| e["type"] == "content_block_delta" && e["delta"]["type"] == "thinking_delta"),
        "{events:?}"
    );

    let mut encoder = AnthropicStreamEncoder::new();
    let events = parse(&encoder.encode_chunk(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Reasoning {
            text: String::new(),
            signature: Some("sig".into()),
        },
        finish_reason: None,
        logprobs: None,
    }));
    assert!(
        events.iter().any(|e| e["type"] == "content_block_start"
            && e["content_block"]["type"] == "thinking"),
        "signature 单独开块: {events:?}"
    );
    assert!(
        events.iter().any(|e| e["type"] == "content_block_delta"
            && e["delta"]["type"] == "signature_delta"
            && e["delta"]["signature"] == "sig"),
        "{events:?}"
    );

    let mut encoder = AnthropicStreamEncoder::new();
    encoder.encode_chunk(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Reasoning {
            text: "t".into(),
            signature: None,
        },
        finish_reason: None,
        logprobs: None,
    });
    let events = parse(&encoder.encode_chunk(StreamChunk::Content {
        index: 0,
        delta: MessageContent::Reasoning {
            text: String::new(),
            signature: None,
        },
        finish_reason: Some("stop".into()),
        logprobs: None,
    }));
    assert!(
        events.iter().any(|e| e["type"] == "content_block_stop"),
        "终止到达闭思考块: {events:?}"
    );
}

/// Anthropic 入站丢弃与回落边角：tool_result 同消息未知块忽略、图片/文档
/// 未知 source 丢弃、纯非文本 system 不产 system、空 tool_choice 回落
/// auto、未知 thinking 类型丢弃。
#[test]
fn anthropic_inbound_drop_and_choice_edges() {
    use fluxweir::protocol::{ContentBlock, ImageSource, Message, MessageContent, Role};

    let mut request = base_anthropic_request();
    request["messages"] = json!([{"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AA"}}
    ]}]);
    let unified = anthropic().to_unified(request).expect("聚合可解析");
    assert!(
        matches!(
            &unified.messages[0].content,
            MessageContent::ToolResult(r) if r.content == "ok"
        ),
        "{:?}",
        unified.messages[0].content
    );

    let mut request = base_anthropic_request();
    request["messages"] = json!([{"role": "user", "content": [
        {"type": "text", "text": "x"},
        {"type": "image", "source": {"type": "file", "file_id": "f_1"}},
        {"type": "document", "source": {"type": "bytes", "data": "AA"}}
    ]}]);
    let unified = anthropic()
        .to_unified(request)
        .expect("未知 source 丢弃不报错");
    let dumped = serde_json::to_string(&unified.messages).unwrap();
    assert!(dumped.contains("x"), "{dumped}");
    assert!(
        !dumped.contains("f_1") && !dumped.contains("bytes"),
        "{dumped}"
    );

    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.system_prompt = None;
    unified.messages.insert(
        0,
        Message {
            role: Role::System,
            content: MessageContent::MultiModal(vec![ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "AA".into(),
                },
                detail: None,
            }]),
        },
    );
    let external = anthropic()
        .request_to_external(unified)
        .expect("to external");
    assert!(external.get("system").is_none(), "{external}");

    let mut request = base_anthropic_request();
    request["tool_choice"] = json!({});
    let unified = anthropic().to_unified(request).expect("空对象回落 auto");
    let rendered = serde_json::to_value(unified.tool_choice.unwrap())
        .unwrap()
        .to_string();
    assert!(rendered.contains("auto"), "{rendered}");

    let mut request = base_anthropic_request();
    request["thinking"] = json!({"type": "quantum"});
    let unified = anthropic()
        .to_unified(request)
        .expect("未知 thinking 可解析");
    assert!(unified.thinking_config.is_none());
}

/// Anthropic 缓存断点注入：system 数组标记尾块、未知锚名忽略。
#[test]
fn anthropic_cache_breakpoint_injection_edges() {
    use fluxweir::config::model_catalog::CacheBreakpointPolicy;
    use fluxweir::protocol::anthropic_converter::inject_cache_control;

    let mut body = json!({
        "model": "claude-x",
        "system": [
            {"type": "text", "text": "a"},
            {"type": "text", "text": "b"}
        ],
        "messages": [{"role": "user", "content": "hi"}]
    });
    let policy = CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: 4,
        anchors: vec!["bogus".into(), "system".into()],
    };
    let injected = inject_cache_control(&mut body, &policy);
    assert_eq!(injected, 1, "未知锚忽略，system 尾块标记一次: {body}");
    assert!(body["system"][0].get("cache_control").is_none(), "{body}");
    assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
}

/// Anthropic 缓存路径 debug 日志的字段求值：tracing 字段在无订阅者时
/// 惰性跳过，挂线程级全兴趣订阅者后 policy 注入与未知锚路径完整求值。
#[test]
fn anthropic_cache_debug_logging_paths() {
    use fluxweir::config::model_catalog::CacheBreakpointPolicy;
    use fluxweir::protocol::anthropic_converter::inject_cache_control;
    use tracing_subscriber::prelude::*;

    struct AllInterest;
    impl tracing_subscriber::Layer<tracing_subscriber::Registry> for AllInterest {
        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }
        fn enabled(
            &self,
            _metadata: &tracing::Metadata<'_>,
            _: tracing_subscriber::layer::Context<'_, tracing_subscriber::Registry>,
        ) -> bool {
            true
        }
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, tracing_subscriber::Registry>,
        ) {
            struct ConsumeAll;
            impl tracing::field::Visit for ConsumeAll {
                fn record_debug(
                    &mut self,
                    _field: &tracing::field::Field,
                    _value: &dyn std::fmt::Debug,
                ) {
                }
            }
            let mut visitor = ConsumeAll;
            event.record(&mut visitor);
        }
    }

    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(AllInterest));

    let mut unified = anthropic()
        .to_unified(base_anthropic_request())
        .expect("to unified");
    unified.system_prompt = Some("s".into());
    let policy = CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: 4,
        anchors: vec!["system".into()],
    };
    let external = anthropic()
        .request_to_external_with_cache_policy(unified, &policy)
        .expect("policy 请求");
    assert_eq!(external["system"][0]["cache_control"]["type"], "ephemeral");

    let mut body = json!({
        "system": [{"type": "text", "text": "a"}],
        "messages": [{"role": "user", "content": "hi"}]
    });
    let policy = CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: 4,
        anchors: vec!["bogus".into()],
    };
    assert_eq!(
        inject_cache_control(&mut body, &policy),
        0,
        "未知锚不注入: {body}"
    );
}

/// Gemini 出站 message_parts 全分支：MultiModal 块、ToolCalls arguments 非法
/// JSON 回退 {"raw":...}、ToolResult 非 JSON 回退 {"result":...}、Reasoning 丢弃。
#[test]
fn gemini_converter_multimodal_reasoning_and_fallbacks() {
    use fluxweir::protocol::unified::ContentBlock;
    use fluxweir::protocol::{
        GeminiConverter, Message, MessageContent, Role, ToolCall, ToolCallFunction, ToolResult,
        UnifiedRequest,
    };
    let converter = GeminiConverter::new();
    let request = UnifiedRequest {
        model: "gemini-2.5-pro".into(),
        messages: vec![
            Message {
                role: Role::User,
                content: MessageContent::MultiModal(vec![
                    ContentBlock::Text {
                        text: "look".into(),
                    },
                    ContentBlock::Image {
                        source: fluxweir::protocol::unified::ImageSource::Url {
                            url: "https://x.example/a.png".into(),
                        },
                        detail: None,
                    },
                ]),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "bad_json_fn".into(),
                        arguments: "not-json{".into(),
                    },
                }]),
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "c1".into(),
                    function_name: "bad_json_fn".into(),
                    content: "plain text result".into(),
                    is_error: false,
                }),
            },
            Message {
                role: Role::Assistant,
                content: MessageContent::Reasoning {
                    text: "hidden thought".into(),
                    signature: None,
                },
            },
        ],
        ..Default::default()
    };
    let body = converter.request_to_external(request).expect("to gemini");
    let dumped = body["contents"].to_string();
    assert!(dumped.contains("look"), "MultiModal 文本 part: {dumped}");
    assert!(dumped.contains("image"), "MultiModal 图片 part: {dumped}");
    assert!(dumped.contains("bad_json_fn"), "tool 名存活: {dumped}");
    assert!(
        dumped.contains(r#""raw":"not-json{""#),
        "非法 JSON arguments 回退 raw 形态: {dumped}"
    );
    assert!(
        dumped.contains(r#""result":"plain text result""#),
        "ToolResult 非对象回退 result 形态: {dumped}"
    );
    assert!(
        !dumped.contains("hidden thought"),
        "Reasoning 无 Gemini part 必须丢弃: {dumped}"
    );
}

/// Responses 出站 tools/format 映射：Tool 的 parameters/strict 变体、
/// ToolChoice 双形态、response_format→text.format。
#[test]
fn responses_converter_tools_and_format_mapping() {
    use fluxweir::protocol::{
        ResponsesConverter, Tool, ToolChoice, ToolChoiceFunction, ToolFunction, UnifiedRequest,
    };
    let converter = ResponsesConverter::new();

    let tool_with_schema = Tool {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search things".into(),
            parameters: json!({"type": "object"}),
            strict: Some(true),
        },
        raw: None,
        cache_control: None,
    };
    let base = UnifiedRequest {
        model: "m".into(),
        tools: vec![tool_with_schema],
        ..Default::default()
    };

    // tool_choice=strategy + json_schema format
    let mut request = base.clone();
    request.tool_choice = Some(ToolChoice::Strategy("required".into()));
    request.response_format = Some(
        json!({"type": "json_schema", "json_schema": {"name": "r", "schema": {"type": "object"}}}),
    );
    let external = converter.request_to_external(request).expect("to external");
    let tools = external["tools"].as_array().expect("tools 数组");
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(tools[0]["parameters"]["type"], "object");
    assert_eq!(tools[0]["strict"], true);
    assert_eq!(external["tool_choice"], "required", "Strategy 形态");
    assert_eq!(external["text"]["format"]["type"], "json_schema");

    // tool_choice=specific
    let mut request = base;
    request.tool_choice = Some(ToolChoice::Specific {
        choice_type: "function".into(),
        function: ToolChoiceFunction {
            name: "search".into(),
        },
    });
    let external = converter.request_to_external(request).expect("to external");
    assert_eq!(external["tool_choice"]["type"], "function");
    assert_eq!(external["tool_choice"]["name"], "search");
}
