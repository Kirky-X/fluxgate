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
