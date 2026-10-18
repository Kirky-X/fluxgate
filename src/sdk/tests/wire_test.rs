// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK 类型层 serde 往返测试（对齐 unified.rs serde 测试口径
//! 构造全字段 → 序列化 wire 断言 → 反序列化往返相等）。
//!
//! SDK 类型自有（零 crate 内依赖），wire 形状以网关真实产出为锚：
//! - 非流式响应 = `response_to_external` 产物（含
//!   `prompt_tokens_details.cached_tokens`，无 vendor 键）；
//! - 流式帧 = `stream_chunk_to_external` 产物（工具片段 / usage 尾帧）；
//! - 请求面 = OpenAI wire（tools/tool_choice/vendor_extensions flatten）。

use crate::sdk::chat::*;
use crate::sdk::error::FluxgateSdkError;
use crate::sdk::stream::ChatStream;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::json;

#[test]
fn chat_message_text_serializes_as_plain_string() {
    let msg = ChatMessage::user("hello");
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["role"], "user");
    assert_eq!(v["content"], "hello", "Text 内容序列化为裸字符串");
    assert!(v.get("tool_call_id").is_none());
    assert!(v.get("tool_calls").is_none());
    // 往返
    let back: ChatMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn chat_message_tool_roundtrip_with_call_id() {
    let msg = ChatMessage::tool("call_1", "sunny");
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["role"], "tool");
    assert_eq!(v["tool_call_id"], "call_1");
    assert_eq!(v["content"], "sunny");
    let back: ChatMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn chat_message_multimodal_parts_wire_shape() {
    let msg = ChatMessage::user(ChatContent::Parts(vec![
        ChatContentPart::text("what is this?"),
        ChatContentPart::image_url("https://example.com/a.png"),
    ]));
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["content"][0]["type"], "text");
    assert_eq!(v["content"][1]["type"], "image_url");
    assert_eq!(
        v["content"][1]["image_url"]["url"],
        "https://example.com/a.png"
    );
    let back: ChatMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

/// base64 data URL 编码：标准字母表 + padding 正确（零依赖实现回归锚）
#[test]
fn image_data_url_encodes_standard_base64() {
    let part = ChatContentPart::image_data_url("image/png", b"hello");
    let v = serde_json::to_value(&part).unwrap();
    let url = v["image_url"]["url"].as_str().unwrap();
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
    assert!(url.ends_with("aGVsbG8="), "RFC 4648 'hello' 样例: {url}");
}

#[test]
fn assistant_tool_calls_roundtrip() {
    let msg = ChatMessage::assistant_with_tool_calls(
        ChatContent::text(""),
        vec![ChatToolCall {
            id: "call_1".into(),
            function: ChatToolFunction {
                name: "get_weather".into(),
                arguments: r#"{"city":"SF"}"#.into(),
            },
        }],
    );
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["tool_calls"][0]["id"], "call_1");
    assert_eq!(v["tool_calls"][0]["function"]["name"], "get_weather");
    let back: ChatMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn chat_finish_reason_four_variants_roundtrip() {
    for (wire, expected) in [
        ("stop", StandardFinishReason::Stop),
        ("length", StandardFinishReason::Length),
        ("tool_calls", StandardFinishReason::ToolCalls),
        ("content_filter", StandardFinishReason::ContentFilter),
    ] {
        let parsed = ChatFinishReason::from_wire(wire);
        assert_eq!(parsed, ChatFinishReason::Standard(expected));
        assert_eq!(parsed.as_wire(), wire);
        // serde 往返
        let v = serde_json::to_value(&parsed).unwrap();
        assert_eq!(v, json!(wire));
        let back: ChatFinishReason = serde_json::from_value(v).unwrap();
        assert_eq!(back, parsed);
    }
    // 上游自定义词兜底透传（pause_turn/refusal 等 Anthropic 词）
    let custom = ChatFinishReason::from_wire("pause_turn");
    assert_eq!(custom, ChatFinishReason::Other("pause_turn".into()));
    assert_eq!(custom.as_wire(), "pause_turn");
}

/// 非流式响应 wire → ChatResponse（网关 response_to_external 真实形状：
/// 含 cached_tokens / tool_calls，无 vendor 键）
#[test]
fn chat_response_parses_gateway_wire() {
    let wire = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1_700_000_000u64,
        "model": "claude-sonnet-4-20250514",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_9", "type": "function",
                                "function": {"name": "f", "arguments": "{}"}}]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
            "prompt_tokens_details": {"cached_tokens": 80}
        }
    });
    let resp = ChatResponse::from_wire(&wire).unwrap();
    assert_eq!(resp.id, "chatcmpl-1");
    assert_eq!(resp.model, "claude-sonnet-4-20250514");
    assert_eq!(resp.content, "");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(
        resp.finish_reason,
        ChatFinishReason::Standard(StandardFinishReason::ToolCalls)
    );
    assert_eq!(resp.usage.cache_read_tokens, 80);
    // wire 无 vendor 键（ChatResponse 不再有 vendor 字段—— BREAKING）
    assert!(wire.get("vendor").is_none());
}

/// 流式帧解析（网关 后真实事件序）：内容帧 → 工具片段帧 →
/// usage 尾帧 → [DONE]
#[tokio::test]
async fn stream_parses_gateway_event_sequence() {
    let frames = vec![
        // 内容帧（网关 chunk id 形如 chunk-{index}）
        r#"data: {"id":"chunk-0","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hel"},"finish_reason":null}]}"#,
        // 工具片段帧（id/name 仅首片段携带）
        r#"data: {"id":"chunk-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"type":"function","id":"call_1","function":{"name":"get_weather"}}]},"finish_reason":null}]}"#,
        // 续片段（arguments 增量分片，无 id/name）
        r#"data: {"id":"chunk-2","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\""}}]},"finish_reason":null}]}"#,
        // usage 尾帧（choices 空 + prompt_tokens_details）
        r#"data: {"id":"usage","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":60}}}"#,
        "data: [DONE]",
    ];
    let byte_stream = futures::stream::iter(
        frames
            .into_iter()
            .map(|f| Ok::<Bytes, reqwest::Error>(Bytes::from(format!("{f}\n\n")))),
    );
    let mut stream = ChatStream::from_bytes_stream(byte_stream);

    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.push(chunk.unwrap());
    }
    assert_eq!(collected.len(), 4, "[DONE] 过滤");

    // 内容帧
    assert_eq!(
        collected[0].delta.as_ref().unwrap().content.as_deref(),
        Some("Hel")
    );
    // 工具首片段
    let tc = &collected[1]
        .delta
        .as_ref()
        .unwrap()
        .tool_calls
        .as_ref()
        .unwrap()[0];
    assert_eq!(tc.index, Some(0));
    assert_eq!(tc.id.as_deref(), Some("call_1"));
    assert_eq!(
        tc.function.as_ref().unwrap().name.as_deref(),
        Some("get_weather")
    );
    // 续片段（无 id/name，仅 arguments 分片）
    let tc2 = &collected[2]
        .delta
        .as_ref()
        .unwrap()
        .tool_calls
        .as_ref()
        .unwrap()[0];
    assert_eq!(tc2.id, None, "续片段不携带 id");
    assert_eq!(
        tc2.function.as_ref().unwrap().arguments.as_deref(),
        Some("{\"city\"")
    );
    // usage 尾帧
    let usage = collected[3].usage.as_ref().expect("usage 尾帧");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.cache_read_tokens, 60);
    assert!(collected[3].delta.is_none(), "usage 尾帧无 delta");
}

/// 审查 HIGH：vendor_extensions 保留键 denylist——typed 键（model/messages
/// 等）被拒（防 flatten 覆写请求骨架），任意扩展键放行
#[test]
fn vendor_extension_reserved_keys_rejected_and_free_keys_allowed() {
    for reserved in [
        "model",
        "messages",
        "stream",
        "tools",
        "tool_choice",
        "max_tokens",
        "temperature",
        "top_p",
        "top_k",
        "frequency_penalty",
        "presence_penalty",
        "stop",
        "user",
        "vendor_extensions",
    ] {
        let err = ChatRequest::new("gpt-4", "hi")
            .with_vendor_extension(reserved, json!("injected"))
            .unwrap_err();
        assert!(
            matches!(err, FluxgateSdkError::Protocol { .. }),
            "保留键 {reserved} 必须拒绝"
        );
    }
    // 任意扩展键放行并进入 wire
    let request = ChatRequest::new("gpt-4", "hi")
        .with_vendor_extension("thinking_mode", json!("off"))
        .unwrap();
    assert_eq!(
        request.vendor_extensions.get("thinking_mode"),
        Some(&json!("off"))
    );
}

/// 审查 LOW×2：base64 %3==1 分支锚（4 字节输入 → 双 `==` padding）
#[test]
fn image_data_url_pads_double_equals_for_rem1() {
    let part = ChatContentPart::image_data_url("image/png", b"abcd");
    let v = serde_json::to_value(&part).unwrap();
    let url = v["image_url"]["url"].as_str().unwrap();
    assert!(url.ends_with("YWJjZA=="), "%3==1 必须双等号 padding: {url}");
}

/// 多 choice 解析：`n>1` 非流式响应的全量候选保留（index 槽位），
/// 首 choice 便捷字段与 `choices[0]` 同源。
#[test]
fn chat_response_parses_multi_choice() {
    let wire = json!({
        "id": "chatcmpl-2",
        "object": "chat.completion",
        "model": "gpt-4",
        "choices": [
            {"index": 0, "message": {"role": "assistant", "content": "A"}, "finish_reason": "stop"},
            {"index": 1, "message": {"role": "assistant", "content": "B", "reasoning_content": "think"}, "finish_reason": "length"}
        ],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
    });
    let resp = ChatResponse::from_wire(&wire).unwrap();

    assert_eq!(resp.choices.len(), 2, "多候选全量保留");
    assert_eq!(resp.choices[0].index, 0);
    assert_eq!(resp.choices[1].index, 1);
    assert_eq!(resp.choices[1].content, "B");
    assert_eq!(resp.choices[1].reasoning_content, "think");
    assert_eq!(
        resp.choices[1].finish_reason,
        ChatFinishReason::Standard(StandardFinishReason::Length)
    );

    assert_eq!(resp.content, "A", "首 choice 便捷视图");
    assert_eq!(
        resp.finish_reason,
        ChatFinishReason::Standard(StandardFinishReason::Stop)
    );
    assert_eq!(resp.usage.total_tokens, 3);
    assert_eq!(resp.choices.len(), 2);
}

/// 工具面 wire：function 工具补 `type` 键（缺省读兼容旧 wire）、
/// custom 工具原对象保真、allowed_tools 形状（D7）。
#[test]
fn chat_tool_wire_shapes() {
    // function 工具：type 键补全 + parameters 原样
    let tool = ChatTool::function("get_weather", json!({"type": "object"}));
    assert_eq!(
        serde_json::to_value(&tool).unwrap(),
        json!({"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}})
    );

    // 描述链式设置；parameters 未设（null）时省略键（不写显式 null）
    let described = tool.clone().with_description("查天气");
    let v = serde_json::to_value(&described).unwrap();
    assert_eq!(v["function"]["description"], json!("查天气"));
    let no_params = ChatTool::function("noop", json!(null));
    let v = serde_json::to_value(&no_params).unwrap();
    assert!(v["function"].get("parameters").is_none(), "null 参数省略");

    // custom 工具原样保真（含 format 嵌套）
    let custom = json!({
        "type": "custom",
        "custom": {"name": "code_execution", "description": "跑代码", "format": {"type": "text"}}
    });
    let tool = ChatTool::Raw(custom.clone());
    assert_eq!(serde_json::to_value(&tool).unwrap(), custom);
    assert_eq!(tool.tool_type(), "custom");

    // 反序列化分类：custom → Raw；旧 wire（无 type）→ Function
    let back: ChatTool = serde_json::from_value(custom).unwrap();
    assert!(matches!(back, ChatTool::Raw(_)), "custom 对象归 Raw");
    let legacy: ChatTool =
        serde_json::from_value(json!({"function": {"name": "f", "parameters": {}}})).unwrap();
    assert_eq!(legacy.tool_type(), "function", "缺省 type 键兼容");
}

/// allowed_tools 形状：`{"type":"allowed_tools","allowed_tools":{mode,tools}}`；
/// 策略词 / 具名函数既有形状不变。
#[test]
fn chat_tool_choice_allowed_tools_wire() {
    let choice = ChatToolChoice::allowed_tools(
        AllowedToolsMode::Auto,
        vec![ChatTool::function("get_weather", json!(null))],
    );
    assert_eq!(
        serde_json::to_value(&choice).unwrap(),
        json!({
            "type": "allowed_tools",
            "allowed_tools": {
                "mode": "auto",
                "tools": [{"type": "function", "function": {"name": "get_weather"}}]
            }
        })
    );

    // 策略词与具名函数形状不回归
    assert_eq!(
        serde_json::to_value(ChatToolChoice::none()).unwrap(),
        json!("none")
    );
    assert_eq!(
        serde_json::to_value(ChatToolChoice::specific("get_weather")).unwrap(),
        json!({"type": "function", "function": {"name": "get_weather"}})
    );

    // untagged 往返（分类到 AllowedTools）
    let roundtrip: ChatToolChoice =
        serde_json::from_value(serde_json::to_value(&choice).unwrap()).unwrap();
    assert_eq!(roundtrip, choice);

    // required 模式词
    let required = ChatToolChoice::allowed_tools(AllowedToolsMode::Required, vec![]);
    assert_eq!(
        serde_json::to_value(&required).unwrap(),
        json!({"type": "allowed_tools", "allowed_tools": {"mode": "required", "tools": []}})
    );
}
