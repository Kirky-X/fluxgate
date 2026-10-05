// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod converter {
    use llm_gate::protocol::{
        AnthropicConverter, AnthropicStreamEncoder, OpenAIConverter, Protocol, ProtocolConverter,
        StreamChunk,
    };
    use serde_json::json;

    #[tokio::test]
    async fn test_openai_converter_input_protocol() {
        let converter = OpenAIConverter::new();
        assert_eq!(converter.input_protocol(), Protocol::OpenAI);
    }

    #[tokio::test]
    async fn test_openai_converter_output_protocol() {
        let converter = OpenAIConverter::new();
        assert_eq!(converter.output_protocol(), Protocol::OpenAI);
    }

    #[tokio::test]
    async fn test_anthropic_converter_input_protocol() {
        let converter = AnthropicConverter::new();
        assert_eq!(converter.input_protocol(), Protocol::Anthropic);
    }

    #[tokio::test]
    async fn test_anthropic_converter_output_protocol() {
        let converter = AnthropicConverter::new();
        assert_eq!(converter.output_protocol(), Protocol::Anthropic);
    }

    #[tokio::test]
    async fn test_openai_to_unified_basic() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ],
            "temperature": 0.7,
            "max_tokens": 100
        });

        let unified = converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "gpt-4");
        assert_eq!(unified.messages.len(), 2);
        assert_eq!(unified.temperature, Some(0.7));
        assert_eq!(unified.max_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_openai_to_unified_missing_model() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "messages": [{"role": "user", "content": "Hello!"}]
        });

        let result = converter.to_unified(request);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_openai_to_unified_missing_messages() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4"
        });

        let result = converter.to_unified(request);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_anthropic_to_unified_basic() {
        let converter = AnthropicConverter::new();
        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "Hello!"}
            ],
            "max_tokens": 100
        });

        let unified = converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "claude-3-5-sonnet-20241022");
        assert_eq!(unified.messages.len(), 1);
        assert_eq!(unified.max_tokens, Some(100));
    }

    #[tokio::test]
    async fn test_parse_openai_response() {
        let converter = OpenAIConverter::new();
        let response = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1677858242,
            "model": "gpt-4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I help you?"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        });

        let result = converter.parse_response(response);
        assert!(result.is_ok());
        let parsed = result.unwrap();

        assert_eq!(parsed.id, "chatcmpl-abc123");
        assert_eq!(parsed.model, "gpt-4");
        assert_eq!(parsed.choices.len(), 1);
        assert_eq!(parsed.usage.prompt_tokens, 10);
        assert_eq!(parsed.usage.completion_tokens, 8);
    }

    #[tokio::test]
    async fn test_parse_anthropic_response() {
        let converter = AnthropicConverter::new();
        let response = json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                {
                    "type": "text",
                    "text": "Hello! How can I help you?"
                }
            ],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 8
            }
        });

        let result = converter.parse_response(response);
        assert!(result.is_ok());
        let parsed = result.unwrap();

        assert_eq!(parsed.id, "msg_abc123");
        assert_eq!(parsed.model, "claude-3-5-sonnet-20241022");
        assert_eq!(parsed.choices.len(), 1);
    }

    // ==================== Tool Calling Tests ====================

    #[tokio::test]
    async fn test_openai_to_unified_with_tools() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather in Beijing?"}
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather information for a city",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {
                                    "type": "string",
                                    "description": "The city name"
                                }
                            },
                            "required": ["city"]
                        }
                    }
                }
            ],
            "tool_choice": "auto"
        });

        let unified = converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.model, "gpt-4");
        assert_eq!(unified.tools.len(), 1);
        assert_eq!(unified.tools[0].function.name, "get_weather");
        assert!(unified.tool_choice.is_some());
    }

    #[tokio::test]
    async fn test_openai_to_unified_with_tool_result() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather in Beijing?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Beijing\"}"
                        }
                    }
                ]},
                {"role": "tool", "content": "Sunny, 25°C", "tool_call_id": "call_123"}
            ]
        });

        let unified = converter.to_unified(request);
        if unified.is_err() {
            eprintln!("Error: {:?}", unified.as_ref().err());
        }
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.messages.len(), 3);
        // Check tool message
        let tool_msg = &unified.messages[2];
        assert_eq!(tool_msg.role, llm_gate::protocol::Role::Tool);
    }

    #[tokio::test]
    async fn test_openai_request_with_tools_to_external() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather in Beijing?"}
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "string"}
                            }
                        }
                    }
                }
            ],
            "tool_choice": "auto"
        });

        let unified = converter.to_unified(request).unwrap();
        let external = converter.request_to_external(unified).unwrap();

        // Verify tools are present in external request
        assert!(external.get("tools").is_some());
        let tools = external.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 1);

        // Verify tool_choice is present
        assert!(external.get("tool_choice").is_some());
    }

    #[tokio::test]
    async fn test_anthropic_to_unified_with_tools() {
        let converter = AnthropicConverter::new();
        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "What's the weather in Beijing?"}
            ],
            "tools": [
                {
                    "name": "get_weather",
                    "description": "Get weather information",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "city": {
                                "type": "string",
                                "description": "The city name"
                            }
                        },
                        "required": ["city"]
                    }
                }
            ],
            "tool_choice": "auto"
        });

        let unified = converter.to_unified(request);
        assert!(unified.is_ok());
        let unified = unified.unwrap();

        assert_eq!(unified.tools.len(), 1);
        assert_eq!(unified.tools[0].function.name, "get_weather");
    }

    #[tokio::test]
    async fn test_anthropic_to_unified_with_tool_use_response() {
        let converter = AnthropicConverter::new();
        let response = json!({
            "id": "msg_abc123",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "get_weather",
                    "input": {"city": "Beijing"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });

        let result = converter.parse_response(response);
        assert!(result.is_ok());
        let parsed = result.unwrap();

        assert_eq!(parsed.choices.len(), 1);
        let msg = parsed.choices[0].message.as_ref().unwrap();
        // Verify tool call is parsed correctly
        match &msg.content {
            llm_gate::protocol::MessageContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "get_weather");
            }
            _ => panic!("Expected ToolCalls content"),
        }
    }

    #[tokio::test]
    async fn test_anthropic_tool_result_message() {
        let converter = AnthropicConverter::new();
        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "What's the weather?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_call_id": "toolu_1", "content": "Sunny, 25C"}
                ]}
            ]
        });

        let unified = converter.to_unified(request);
        assert!(unified.is_ok());
        assert_eq!(unified.unwrap().messages.len(), 3);
    }

    #[tokio::test]
    async fn test_tool_choice_specific_function() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"}
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather",
                        "parameters": {"type": "object", "properties": {}}
                    }
                },
                {
                    "type": "function",
                    "function": {
                        "name": "get_time",
                        "description": "Get time",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }
            ],
            "tool_choice": {
                "type": "function",
                "function": {"name": "get_weather"}
            }
        });

        let unified = converter.to_unified(request).unwrap();

        match &unified.tool_choice {
            Some(llm_gate::protocol::ToolChoice::Specific { function, .. }) => {
                assert_eq!(function.name, "get_weather");
            }
            _ => panic!("Expected Specific tool_choice"),
        }
    }

    // ==================== 流式转换与 SSE 增量分帧（recId=3） ====================

    use llm_gate::protocol::sse_frame::SseFrameParser;

    /// 把上游 SSE 原文（可劈块）经分帧器取出的 data 负载串。
    fn parse_frames_to_data(payload: &str, split_at: Option<usize>) -> Vec<String> {
        let mut parser = SseFrameParser::new();
        let bytes = payload.as_bytes();
        match split_at {
            Some(pos) => {
                parser.feed(&bytes[..pos]);
                parser.feed(&bytes[pos..]);
            }
            None => parser.feed(bytes),
        }
        let mut out = Vec::new();
        while let Some(frame) = parser.next_frame() {
            if let Some(data) = frame.data {
                out.push(data);
            }
        }
        out
    }

    /// ⑤ OpenAI chunk 帧 → StreamChunk → Anthropic 事件序列含
    /// message_start / content_block_delta / message_stop（完整合法流）。
    ///
    /// 跨协议：parse 用 OpenAIConverter（上游协议），encode 用
    /// AnthropicConverter（目标协议）。
    #[tokio::test]
    async fn test_openai_stream_frame_to_anthropic_event_lifecycle() {
        let openai = OpenAIConverter::new();

        let chunk1 = openai
            .parse_stream_frame("{\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}")
            .expect("text delta 帧应解析为 IR chunk");
        let chunk2 = openai
            .parse_stream_frame(
                "{\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}",
            )
            .expect("finish_reason 帧应解析为 IR chunk");

        // Anthropic 出站状态机补齐生命周期
        let mut encoder = AnthropicStreamEncoder::new();
        let mut all_events: Vec<String> = Vec::new();
        for chunk in [chunk1, chunk2] {
            all_events.extend(encoder.encode_chunk(chunk));
        }
        all_events.extend(encoder.finish());

        let types: Vec<String> = all_events
            .iter()
            .filter_map(|e| {
                let v: serde_json::Value = serde_json::from_str(e).ok()?;
                v.get("type")?.as_str().map(|s| s.to_string())
            })
            .collect();

        assert_eq!(
            types[0], "message_start",
            "首个事件必须是 message_start: {types:?}"
        );
        assert!(
            types.iter().any(|t| t == "content_block_start"),
            "text delta 前应有 content_block_start: {types:?}"
        );
        assert!(
            types.iter().any(|t| t == "content_block_delta"),
            "应含 content_block_delta: {types:?}"
        );
        assert!(
            types.iter().any(|t| t == "content_block_stop"),
            "finish_reason 后应闭合 content_block_stop: {types:?}"
        );
        assert_eq!(
            types.last().unwrap(),
            "message_stop",
            "流必须以 message_stop 收尾: {types:?}"
        );

        // content_block_delta 携带文本
        let delta_event = all_events
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(e).unwrap())
            .find(|v| v["type"] == "content_block_delta")
            .expect("delta event present");
        assert_eq!(delta_event["delta"]["type"], "text_delta");
        assert_eq!(delta_event["delta"]["text"], "Hel");
    }

    /// ⑥ usage 尾帧 → message_delta.output_tokens。
    #[tokio::test]
    async fn test_usage_tail_frame_becomes_message_delta_output_tokens() {
        let openai = OpenAIConverter::new();

        // OpenAI usage 尾帧（stream_options.include_usage，choices 空）
        let usage_frame = openai
            .parse_stream_frame(
                "{\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":7,\"total_tokens\":17}}",
            )
            .expect("usage 尾帧应解析为 Usage chunk");
        match &usage_frame {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => {
                assert_eq!(
                    (*prompt_tokens, *completion_tokens, *total_tokens),
                    (10, 7, 17)
                );
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }

        // Anthropic 出站：Usage → message_delta（output_tokens）
        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(usage_frame);
        let message_delta = events
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(e).unwrap())
            .find(|v| v["type"] == "message_delta")
            .expect("Usage 应产出 message_delta");
        assert_eq!(message_delta["usage"]["output_tokens"], 7);
    }

    /// ⑦ tool_call delta 帧不静默丢弃（recId=8 联动回归）。
    #[tokio::test]
    async fn test_tool_call_delta_frame_not_silently_dropped() {
        let converter = OpenAIConverter::new();
        let chunk = converter
            .parse_stream_frame(
                "{\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\"\"}}]},\"finish_reason\":null}]}",
            )
            .expect("tool_call delta 帧不应被丢弃");
        // T004 表示统一：单片段帧 → ToolCall 片段（id/name 首片段携带）
        match chunk {
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, 0);
                assert_eq!(id, "call_1");
                assert_eq!(name.as_deref(), Some("get_weather"));
                assert!(arguments
                    .as_deref()
                    .unwrap_or_default()
                    .contains("\"city\""));
            }
            other => panic!("expected ToolCall fragment, got {other:?}"),
        }
    }

    /// Anthropic 上游流式帧反向解析：content_block_delta / message_delta / ping。
    #[tokio::test]
    async fn test_anthropic_stream_frames_parse_to_ir() {
        let converter = AnthropicConverter::new();

        let text = converter
            .parse_stream_frame(
                "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}",
            )
            .expect("content_block_delta 应解析");
        assert!(matches!(
            text,
            StreamChunk::Content { ref delta, .. }
                if matches!(delta, llm_gate::protocol::MessageContent::Text(t) if t == "hi")
        ));

        let usage = converter
            .parse_stream_frame(
                "{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}",
            )
            .expect("message_delta 应解析");
        assert!(matches!(
            usage,
            StreamChunk::Usage {
                completion_tokens: 9,
                ..
            }
        ));

        assert!(matches!(
            converter.parse_stream_frame("{\"type\":\"ping\"}"),
            Some(StreamChunk::Heartbeat)
        ));
        // 生命周期帧不映射 IR（由出站状态机重建）
        assert!(converter
            .parse_stream_frame("{\"type\":\"message_start\",\"message\":{}}")
            .is_none());
        assert!(converter
            .parse_stream_frame("{\"type\":\"message_stop\"}")
            .is_none());
    }

    /// `[DONE]` 终止标记与 role-only 首帧不产出 IR 事件。
    #[tokio::test]
    async fn test_openai_done_marker_and_role_only_frame_skipped() {
        let converter = OpenAIConverter::new();
        assert!(converter.parse_stream_frame("[DONE]").is_none());
        assert!(converter
            .parse_stream_frame(
                "{\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}"
            )
            .is_none());
    }

    /// 端到端管线：上游 OpenAI SSE 原文劈在两块之间 → 分帧器拼帧 →
    /// parse_stream_frame 反向解析 → IR chunk（manager 流的等价语义）。
    #[tokio::test]
    async fn test_split_sse_bytes_through_parser_to_ir_chunk() {
        let converter = OpenAIConverter::new();
        // split=79：劈在帧定界符 \n\n 中间（第一块含首个 \n）
        let frames = parse_frames_to_data(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\nda",
            Some(79),
        );
        // 第一块 + 第二块拼回后恰出第一帧（"da" 是第二帧的截断前缀）
        assert_eq!(frames.len(), 1);

        let mut parser = SseFrameParser::new();
        // 第二帧的 "data:" 前缀同样被劈开（"da" 在上一块末尾）
        parser.feed(b"da");
        parser.feed(b"ta: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n");
        let rest: Vec<String> = parser
            .next_frame()
            .and_then(|f| f.data)
            .into_iter()
            .collect();
        assert_eq!(rest.len(), 1);

        let chunk1 = converter.parse_stream_frame(&frames[0]).unwrap();
        let chunk2 = converter.parse_stream_frame(&rest[0]).unwrap();
        match (chunk1, chunk2) {
            (
                StreamChunk::Content { delta: d1, .. },
                StreamChunk::Content {
                    finish_reason: fr2, ..
                },
            ) => {
                assert!(matches!(d1, llm_gate::protocol::MessageContent::Text(t) if t == "Hel"));
                assert_eq!(fr2.as_deref(), Some("stop"));
            }
            _ => panic!("expected two Content chunks"),
        }
    }

    // ==================== 协议断点修复（recId=8） ====================

    use llm_gate::protocol::Role;

    /// ① Anthropic 多轮 tool_use+tool_result 历史 → to_unified →
    /// request_to_external(OpenAI vendor) 往返：assistant.tool_calls 与
    /// role:"tool"+tool_call_id 保留（此前 tool_use/tool_result 静默丢弃）。
    #[tokio::test]
    async fn test_anthropic_tool_history_roundtrip_preserves_tools() {
        let anthropic = AnthropicConverter::new();
        let openai = OpenAIConverter::new();

        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [
                {"role": "user", "content": "What's the weather in Beijing?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Beijing"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Sunny, 25C"}
                ]},
                {"role": "user", "content": "Thanks!"}
            ]
        });

        let unified = anthropic
            .to_unified(request)
            .expect("to_unified should succeed");

        // IR 层：assistant 聚合 TextWithToolCalls（text + tool_use 并存）
        let assistant = &unified.messages[1];
        assert_eq!(assistant.role, Role::Assistant);
        match &assistant.content {
            llm_gate::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "Let me check.");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "toolu_1");
                assert_eq!(tool_calls[0].function.name, "get_weather");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }

        // IR 层：tool_result 聚合为 Role::Tool + ToolResult
        let tool_msg = &unified.messages[2];
        assert_eq!(tool_msg.role, Role::Tool);
        match &tool_msg.content {
            llm_gate::protocol::MessageContent::ToolResult(result) => {
                assert_eq!(result.tool_call_id, "toolu_1");
                assert_eq!(result.content, "Sunny, 25C");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        // 出站 OpenAI：assistant 消息带顶层 tool_calls，工具结果为
        // role:"tool" + tool_call_id
        let external = openai.request_to_external(unified).expect("to OpenAI");
        let msgs = external["messages"].as_array().expect("messages array");

        let assistant_out = &msgs[1];
        assert_eq!(assistant_out["role"], "assistant");
        let out_calls = assistant_out["tool_calls"]
            .as_array()
            .expect("top-level tool_calls");
        assert_eq!(out_calls.len(), 1);
        assert_eq!(out_calls[0]["id"], "toolu_1");
        assert_eq!(out_calls[0]["function"]["name"], "get_weather");

        let tool_out = &msgs[2];
        assert_eq!(tool_out["role"], "tool");
        assert_eq!(tool_out["tool_call_id"], "toolu_1");
        assert_eq!(tool_out["content"], "Sunny, 25C");
    }

    /// ② OpenAI assistant 消息 String content + 顶层 tool_calls →
    /// TextWithToolCalls（此前 String 分支提前返回、tool_calls 丢失）。
    #[tokio::test]
    async fn test_openai_string_content_with_tool_calls_becomes_text_with_tool_calls() {
        let converter = OpenAIConverter::new();
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "assistant",
                    "content": "Let me check the weather.",
                    "tool_calls": [
                        {"id": "call_9", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}}
                    ]
                }
            ]
        });

        let unified = converter.to_unified(request).expect("to_unified");
        match &unified.messages[0].content {
            llm_gate::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "Let me check the weather.");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call_9");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }
    }

    /// ③ OpenAI parse_response 读 message.tool_calls（此前只读 content）。
    #[tokio::test]
    async fn test_openai_parse_response_extracts_tool_calls() {
        let converter = OpenAIConverter::new();
        let response = json!({
            "id": "chatcmpl-tool",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Checking...",
                    "tool_calls": [
                        {"id": "call_r1", "type": "function", "function": {"name": "lookup", "arguments": "{}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });

        let parsed = converter.parse_response(response).expect("parse_response");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        match &msg.content {
            llm_gate::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "Checking...");
                assert_eq!(tool_calls[0].id, "call_r1");
                assert_eq!(tool_calls[0].function.name, "lookup");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }
    }

    /// ④ Anthropic 顶层 system → UnifiedRequest 首条 System 消息 →
    /// OpenAI 出站 messages[0].role=="system"。
    #[tokio::test]
    async fn test_anthropic_top_level_system_to_first_system_message() {
        let anthropic = AnthropicConverter::new();
        let openai = OpenAIConverter::new();

        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 10
        });

        let unified = anthropic.to_unified(request).expect("to_unified");
        assert_eq!(unified.messages.len(), 2, "system 提升为首条消息");
        assert_eq!(unified.messages[0].role, Role::System);
        assert!(matches!(
            &unified.messages[0].content,
            llm_gate::protocol::MessageContent::Text(s) if s == "You are a helpful assistant."
        ));

        // 出站 OpenAI：system 保持为 messages[0]
        let external = openai.request_to_external(unified).expect("to OpenAI");
        assert_eq!(external["messages"][0]["role"], "system");
        assert_eq!(
            external["messages"][0]["content"],
            "You are a helpful assistant."
        );
    }

    /// ④ 反向：OpenAI system 消息 → Anthropic 出站顶层 system 字段
    /// （Anthropic messages 数组不接受 system 角色）。
    #[tokio::test]
    async fn test_openai_system_message_lifted_to_anthropic_top_level() {
        let converter = AnthropicConverter::new();
        let request = llm_gate::protocol::UnifiedRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            messages: vec![
                llm_gate::protocol::Message {
                    role: Role::System,
                    content: llm_gate::protocol::MessageContent::Text("Be terse.".into()),
                },
                llm_gate::protocol::Message {
                    role: Role::User,
                    content: llm_gate::protocol::MessageContent::Text("Hi".into()),
                },
            ],
            max_tokens: Some(10),
            ..Default::default()
        };

        let external = converter
            .request_to_external(request)
            .expect("to Anthropic");
        assert_eq!(
            external["system"], "Be terse.",
            "System 消息应提升为顶层 system 参数"
        );
        let msgs = external["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "system 不得留在 messages 数组");
        assert_eq!(msgs[0]["role"], "user");
    }

    /// ⑤ stop_reason ↔ finish_reason 静态映射（双向）。
    #[tokio::test]
    async fn test_stop_reason_finish_reason_bidirectional_mapping() {
        let anthropic = AnthropicConverter::new();

        // Anthropic → IR（OpenAI 词汇）
        let parsed = anthropic
            .parse_response(json!({
                "id": "m", "model": "claude-x",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .expect("parse");
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("tool_calls"),
            "tool_use 应映射为 tool_calls"
        );

        let parsed = anthropic
            .parse_response(json!({
                "id": "m", "model": "claude-x",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "max_tokens",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .expect("parse");
        assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("length"));

        let parsed = anthropic
            .parse_response(json!({
                "id": "m", "model": "claude-x",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .expect("parse");
        assert_eq!(parsed.choices[0].finish_reason.as_deref(), Some("stop"));

        // IR → Anthropic（反向）
        let make_response = |finish: Option<&str>| llm_gate::protocol::UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude-x".into(),
            choices: vec![llm_gate::protocol::Choice {
                index: 0,
                message: Some(llm_gate::protocol::Message {
                    role: Role::Assistant,
                    content: llm_gate::protocol::MessageContent::Text("ok".into()),
                }),
                finish_reason: finish.map(|s| s.to_string()),
                finish: None,
                logprobs: None,
            }],
            usage: llm_gate::protocol::TokenUsage::default(),
        };

        let out = anthropic
            .response_to_external(make_response(Some("tool_calls")))
            .expect("to Anthropic");
        assert_eq!(
            out["stop_reason"], "tool_use",
            "tool_calls 应反向映射为 tool_use"
        );

        let out = anthropic
            .response_to_external(make_response(Some("length")))
            .expect("to Anthropic");
        assert_eq!(out["stop_reason"], "max_tokens");

        let out = anthropic
            .response_to_external(make_response(Some("stop")))
            .expect("to Anthropic");
        assert_eq!(out["stop_reason"], "end_turn");
    }

    /// 评审 major 回归：出站 Anthropic 方向的工具历史 —— tool_result 块
    /// 必须用 Anthropic 规范字段 tool_use_id（此前误写 tool_call_id，缺
    /// 必填字段会被上游 400 拒收；recId=8① 使本分支首次可达）。
    #[tokio::test]
    async fn test_anthropic_outbound_tool_result_uses_tool_use_id_field() {
        let openai = OpenAIConverter::new();
        let anthropic = AnthropicConverter::new();

        // OpenAI 入站：assistant tool_calls 历史 + role:"tool" 结果
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "Sunny, 25C"}
            ]
        });

        let unified = openai.to_unified(request).expect("to_unified");
        let external = anthropic
            .request_to_external(unified)
            .expect("to Anthropic");
        let msgs = external["messages"].as_array().expect("messages array");

        // assistant：tool_calls 转 tool_use 块
        let assistant = &msgs[1];
        assert_eq!(assistant["role"], "assistant");
        let assistant_blocks = assistant["content"].as_array().expect("content array");
        let tool_use = assistant_blocks
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("tool_use block");
        assert_eq!(tool_use["id"], "call_1");
        assert_eq!(tool_use["name"], "get_weather");
        assert_eq!(tool_use["input"]["city"], "Beijing");

        // 工具结果消息：Anthropic 用 user 角色携带 tool_result 块，
        // 字段必须是规范名 tool_use_id
        let tool_msg = &msgs[2];
        assert_eq!(tool_msg["role"], "user");
        let blocks = tool_msg["content"].as_array().expect("content array");
        let result_block = blocks
            .iter()
            .find(|b| b["type"] == "tool_result")
            .expect("tool_result block");
        assert_eq!(
            result_block["tool_use_id"], "call_1",
            "tool_result 必须引用规范的 tool_use_id"
        );
        assert_eq!(result_block["content"], "Sunny, 25C");
        assert!(
            result_block.get("tool_call_id").is_none(),
            "不得残留非规范字段 tool_call_id: {result_block}"
        );
    }

    // ==================== 覆盖缺口补测：AnthropicConverter 边角分支 ====================

    use llm_gate::protocol::{
        Choice, ContentBlock, ImageSource, Message, MessageContent, TokenUsage, Tool, ToolCall,
        ToolCallFunction, ToolChoice, ToolChoiceFunction, ToolFunction, ToolResult, UnifiedRequest,
        UnifiedResponse,
    };
    use llm_gate::GateError;

    /// 构造仅含 model + 单条 user 文本消息的最小 UnifiedRequest（其余字段缺省）。
    fn minimal_unified_request(model: &str, text: &str) -> UnifiedRequest {
        UnifiedRequest {
            model: model.into(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::Text(text.into()),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_anthropic_converter_default() {
        let converter = <AnthropicConverter as Default>::default();
        assert_eq!(converter.input_protocol(), Protocol::Anthropic);
    }

    /// 顶层 system 支持 content blocks 数组形状；非字符串/数组类型静默忽略。
    #[tokio::test]
    async fn test_anthropic_to_unified_system_block_array_and_invalid_shape() {
        let converter = AnthropicConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "system": [
                    {"type": "text", "text": "Be terse."},
                    {"type": "text", "text": "Cite sources."}
                ],
                "messages": [{"role": "user", "content": "Hi"}]
            }))
            .expect("blocks 数组形状的 system");
        assert_eq!(unified.messages[0].role, Role::System);
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::Text(s) if s == "Be terse.\nCite sources."
        ));

        let unified = converter
            .to_unified(json!({
                "model": "claude-3-5-sonnet-20241022",
                "system": 42,
                "messages": [{"role": "user", "content": "Hi"}]
            }))
            .expect("非法形状 system 被忽略而非报错");
        assert_eq!(
            unified.messages.len(),
            1,
            "非法 system 形状不得产生 System 消息"
        );
        assert_eq!(unified.messages[0].role, Role::User);
    }

    /// tool_choice 三种入站形状：Specific 对象 / 无 function 的对象 / 其他标量。
    #[tokio::test]
    async fn test_anthropic_to_unified_tool_choice_variants() {
        let converter = AnthropicConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
            }))
            .expect("specific tool_choice");
        match unified.tool_choice {
            Some(ToolChoice::Specific {
                choice_type,
                function,
            }) => {
                assert_eq!(choice_type, "function");
                assert_eq!(function.name, "get_weather");
            }
            other => panic!("expected Specific, got {other:?}"),
        }

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "any"}
            }))
            .expect("object without function");
        assert!(
            matches!(
                unified.tool_choice,
                Some(ToolChoice::Strategy(s)) if s == "required"
            ),
            "Anthropic 原生 any 必须映射 IR required"
        );

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": 42
            }))
            .expect("标量 tool_choice 走兜底");
        assert!(matches!(
            unified.tool_choice,
            Some(ToolChoice::Strategy(s)) if s == "auto"
        ));
    }

    /// to_unified → request_to_external 往返：stop_sequences、tool_choice
    /// Strategy、采样参数与完整工具 schema（description/enum/required）。
    #[tokio::test]
    async fn test_anthropic_request_roundtrip_with_full_tool_schema() {
        let converter = AnthropicConverter::new();
        let request = json!({
            "model": "claude-3-5-sonnet-20241022",
            "messages": [{"role": "user", "content": "What's the weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "city": {
                            "type": "string",
                            "description": "City name",
                            "enum": ["Beijing", "Shanghai"]
                        }
                    },
                    "required": ["city"]
                }
            }],
            "tool_choice": "any",
            "temperature": 0.5,
            "max_tokens": 256,
            "stream": true,
            "stop_sequences": ["END", "STOP"]
        });

        let unified = converter.to_unified(request).expect("to_unified");
        assert_eq!(
            unified.stop,
            vec!["END".to_string(), "STOP".to_string()],
            "stop_sequences 应进入 IR stop"
        );

        let external = converter.request_to_external(unified).expect("to external");
        assert_eq!(external["temperature"], 0.5);
        assert_eq!(external["stream"], true);
        assert_eq!(external["stop_sequences"], json!(["END", "STOP"]));
        assert_eq!(
            external["tool_choice"],
            json!({"type": "any"}),
            "Strategy(any) 出站必须写 Anthropic 原生对象形状"
        );
        assert_eq!(external["max_tokens"], 256);

        let tools = external["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get weather for a city");
        let schema = &tools[0]["input_schema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["city"]));
        assert_eq!(schema["properties"]["city"]["type"], "string");
        assert_eq!(schema["properties"]["city"]["description"], "City name");
        assert_eq!(
            schema["properties"]["city"]["enum"],
            json!(["Beijing", "Shanghai"])
        );
    }

    /// 出站 tool_choice Specific → Anthropic 原生 {"type":"tool","name"} 形状。
    #[tokio::test]
    async fn test_anthropic_request_to_external_specific_tool_choice() {
        let converter = AnthropicConverter::new();
        let mut request = minimal_unified_request("claude", "hi");
        request.tool_choice = Some(ToolChoice::Specific {
            choice_type: "function".into(),
            function: ToolChoiceFunction {
                name: "get_weather".into(),
            },
        });

        let external = converter.request_to_external(request).expect("to external");
        assert_eq!(external["tool_choice"]["type"], "tool");
        assert_eq!(external["tool_choice"]["name"], "get_weather");
    }

    /// 上游错误对象 → GateError::Protocol，type/message 进入错误文案。
    #[tokio::test]
    async fn test_anthropic_parse_response_error_object() {
        let converter = AnthropicConverter::new();
        let err = converter
            .parse_response(json!({
                "error": {
                    "type": "invalid_request_error",
                    "message": "max_tokens is required"
                }
            }))
            .unwrap_err(); // error 对象必须转为 Err
        match err {
            GateError::Protocol { source, inner } => {
                assert!(source.contains("invalid_request_error"), "{source}");
                assert!(source.contains("max_tokens is required"), "{source}");
                assert!(inner.is_none());
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    /// content 数组混合 text + tool_use + 未知块 → TextWithToolCalls，
    /// 未知块类型在聚合循环中被跳过。
    #[tokio::test]
    async fn test_anthropic_parse_response_mixed_text_tool_use_and_unknown_blocks() {
        let converter = AnthropicConverter::new();
        let parsed = converter
            .parse_response(json!({
                "id": "m", "model": "claude",
                "content": [
                    {"type": "text", "text": "Checking."},
                    {"type": "tool_use", "id": "t1", "name": "get_weather", "input": {"city": "BJ"}},
                    {"type": "mystery"}
                ],
                "stop_reason": "tool_use"
            }))
            .expect("parse mixed content");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        match &msg.content {
            MessageContent::TextWithToolCalls { text, tool_calls } => {
                assert_eq!(text, "Checking.");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].function.name, "get_weather");
                assert_eq!(tool_calls[0].function.arguments, "{\"city\":\"BJ\"}");
            }
            other => panic!("expected TextWithToolCalls, got {other:?}"),
        }
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    /// tool_use 块缺 id/name 导致解析失败时聚合降级为纯文本（不得臆造空工具调用）。
    #[tokio::test]
    async fn test_anthropic_parse_response_unparsable_tool_use_degrades_to_text() {
        let converter = AnthropicConverter::new();
        let parsed = converter
            .parse_response(json!({
                "id": "m", "model": "claude",
                "content": [
                    {"type": "tool_use", "input": {}},
                    {"type": "text", "text": "fallback text"}
                ]
            }))
            .expect("parse degraded tool_use");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        assert!(matches!(
            &msg.content,
            MessageContent::Text(s) if s == "fallback text"
        ));
    }

    /// content 为字符串形状与 content 缺失两种响应形状。
    #[tokio::test]
    async fn test_anthropic_parse_response_string_and_missing_content() {
        let converter = AnthropicConverter::new();

        let parsed = converter
            .parse_response(json!({
                "id": "m1", "model": "claude", "content": "plain string"
            }))
            .expect("string content");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        assert!(matches!(&msg.content, MessageContent::Text(s) if s == "plain string"));

        let parsed = converter
            .parse_response(json!({
                "id": "m2", "model": "claude"
            }))
            .expect("无 content 字段");
        assert!(
            parsed.choices[0].message.is_none(),
            "缺 content 不得臆造 assistant 消息"
        );
    }

    /// stream_chunk_to_external 全变体：文本 / 多模态 / 工具调用 / 工具结果 /
    /// 混合 / usage / 心跳。非文本 delta 首版不产出流式文本，但事件形状稳定。
    #[tokio::test]
    async fn test_anthropic_stream_chunk_to_external_variants() {
        let converter = AnthropicConverter::new();

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::Text("hello".into()),
                finish_reason: Some("stop".into()),
            })
            .expect("text delta");
        assert_eq!(out["type"], "content_block_delta");
        assert_eq!(out["delta"]["type"], "text_delta");
        assert_eq!(out["delta"]["text"], "hello");
        assert_eq!(out["content_block"]["text"], "hello");
        // T004：stop_reason 出站映射 Anthropic 词表（stop→end_turn）
        assert_eq!(out["stop_reason"], "end_turn");

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::MultiModal(vec![
                    ContentBlock::Text { text: "a".into() },
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: "image/png".into(),
                            data: "Zm9v".into(),
                        },
                        detail: None,
                    },
                    ContentBlock::Type("weird".into()),
                ]),
                finish_reason: None,
            })
            .expect("multimodal delta");
        assert_eq!(out["type"], "content_block_delta");
        assert_eq!(out["delta"]["text"], "", "MultiModal delta 首版不产出文本");

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::ToolCalls(vec![ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "f".into(),
                        arguments: "{}".into(),
                    },
                }]),
                finish_reason: None,
            })
            .expect("tool calls delta");
        assert_eq!(out["delta"]["text"], "");

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::ToolResult(ToolResult {
                    tool_call_id: "c1".into(),
                    function_name: String::new(),
                    content: "sunny".into(),
                    is_error: false,
                }),
                finish_reason: None,
            })
            .expect("tool result delta");
        assert_eq!(out["delta"]["text"], "");

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 0,
                delta: MessageContent::TextWithToolCalls {
                    text: "checking".into(),
                    tool_calls: vec![],
                },
                finish_reason: None,
            })
            .expect("mixed delta");
        assert_eq!(out["delta"]["text"], "");

        let out = converter
            .stream_chunk_to_external(StreamChunk::Usage {
                prompt_tokens: 3,
                completion_tokens: 7,
                total_tokens: 10,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                finish_reason: None,
            })
            .expect("usage");
        assert_eq!(out["type"], "message_delta");
        assert_eq!(out["usage"]["output_tokens"], 7);

        let out = converter
            .stream_chunk_to_external(StreamChunk::Heartbeat)
            .expect("heartbeat");
        assert_eq!(out, json!({"type": "ping"}));
    }

    /// 空负载 / `[DONE]` / 错误帧不映射 IR。
    #[tokio::test]
    async fn test_anthropic_parse_stream_frame_skips_non_data_frames() {
        let converter = AnthropicConverter::new();
        assert!(converter.parse_stream_frame("").is_none());
        assert!(converter.parse_stream_frame("   ").is_none());
        assert!(converter.parse_stream_frame("[DONE]").is_none());
        assert!(converter
            .parse_stream_frame("{\"error\": {\"type\": \"overloaded\"}}")
            .is_none());
    }

    /// Usage 到达时未闭合的 content block 先闭合再发 message_delta。
    #[tokio::test]
    async fn test_anthropic_stream_encoder_usage_closes_open_block() {
        let mut encoder = AnthropicStreamEncoder::default();
        let mut events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("hi".into()),
            finish_reason: None,
        });
        events.extend(encoder.encode_chunk(StreamChunk::Usage {
            prompt_tokens: 2,
            completion_tokens: 5,
            total_tokens: 7,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: None,
        }));

        let types: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let v: serde_json::Value = serde_json::from_str(e).ok()?;
                v["type"].as_str().map(|s| s.to_string())
            })
            .collect();
        let stop_pos = types
            .iter()
            .position(|t| *t == "content_block_stop")
            .expect("Usage 前有未闭合 block");
        let delta_pos = types
            .iter()
            .position(|t| *t == "message_delta")
            .expect("Usage 应产出 message_delta");
        assert!(
            stop_pos < delta_pos,
            "content_block_stop 必须先于 message_delta: {types:?}"
        );
    }

    /// Heartbeat 直接映射为 ping 帧。
    #[tokio::test]
    async fn test_anthropic_stream_encoder_heartbeat() {
        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(StreamChunk::Heartbeat);
        assert_eq!(events, vec![json!({"type": "ping"}).to_string()]);
    }

    /// finish 补齐语义：空流产出最小合法流；未闭合 block 先闭合。
    #[tokio::test]
    async fn test_anthropic_stream_encoder_finish_variants() {
        let types_of = |events: &[String]| -> Vec<String> {
            events
                .iter()
                .filter_map(|e| {
                    let v: serde_json::Value = serde_json::from_str(e).ok()?;
                    v["type"].as_str().map(|s| s.to_string())
                })
                .collect()
        };

        let mut encoder = AnthropicStreamEncoder::new();
        assert_eq!(
            types_of(&encoder.finish()),
            vec!["message_start".to_string(), "message_stop".to_string()],
            "空流必须产出合法最小 Anthropic 流"
        );

        let mut encoder = AnthropicStreamEncoder::new();
        encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("x".into()),
            finish_reason: None,
        });
        assert_eq!(
            types_of(&encoder.finish()),
            vec!["content_block_stop".to_string(), "message_stop".to_string()],
            "finish 前未闭合的 block 必须先补 content_block_stop"
        );
    }

    /// 流式 delta 内容变体：多模态拼接文本块、混合取文本、纯工具调用不产文本。
    #[tokio::test]
    async fn test_anthropic_stream_encoder_delta_content_variants() {
        let delta_texts_of = |events: &[String]| -> Vec<String> {
            events
                .iter()
                .filter_map(|e| {
                    let v: serde_json::Value = serde_json::from_str(e).ok()?;
                    v["delta"]["text"].as_str().map(|s| s.to_string())
                })
                .collect()
        };

        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::MultiModal(vec![
                ContentBlock::Text { text: "a".into() },
                ContentBlock::Type("weird".into()),
                ContentBlock::Text { text: "b".into() },
            ]),
            finish_reason: None,
        });
        assert_eq!(
            delta_texts_of(&events),
            vec!["ab".to_string()],
            "多模态 delta 拼接全部文本块为单一事件"
        );

        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::TextWithToolCalls {
                text: "checking".into(),
                tool_calls: vec![],
            },
            finish_reason: None,
        });
        assert_eq!(delta_texts_of(&events), vec!["checking".to_string()]);

        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::ToolCalls(vec![ToolCall {
                id: "c1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            }]),
            finish_reason: None,
        });
        assert!(
            delta_texts_of(&events).is_empty(),
            "纯工具调用 delta 首版不产出文本"
        );
    }

    /// 消息级边角：未知 role 归一 user、tool_use 解析失败降级纯文本、
    /// 未知块跳过、content 缺失/非法。
    #[tokio::test]
    async fn test_anthropic_to_unified_message_edge_cases() {
        let converter = AnthropicConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "function", "content": "legacy"}]
            }))
            .expect("未知 role");
        assert_eq!(unified.messages[0].role, Role::User);

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "assistant", "content": [
                    {"type": "tool_use", "input": {}},
                    {"type": "text", "text": "partial"},
                    {"type": "mystery"}
                ]}]
            }))
            .expect("tool_use 解析失败");
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::Text(s) if s == "partial"
        ));

        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user"}]
            }))
            .expect("content 缺失");
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::Text(s) if s.is_empty()
        ));

        let err = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": 42}]
            }))
            .unwrap_err(); // content 数字必须报错
        match err {
            GateError::Validation { field } => assert_eq!(field, "content"),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    /// 并行工具调用的多 tool_result 聚合：首块 tool_use_id 为准、
    /// content 支持字符串与 blocks 数组、缺 content 块跳过、is_error 任一为真即真。
    #[tokio::test]
    async fn test_anthropic_tool_result_aggregation_multi_block() {
        let converter = AnthropicConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "first"},
                    {"type": "tool_result", "tool_use_id": "t2", "content": [
                        {"type": "text", "text": "blk1"},
                        {"type": "text", "text": "blk2"}
                    ]},
                    {"type": "tool_result", "tool_use_id": "t3"},
                    {"type": "tool_result", "tool_use_id": "t4", "content": "boom", "is_error": true}
                ]}]
            }))
            .expect("aggregated tool_result");
        let msg = &unified.messages[0];
        assert_eq!(msg.role, Role::Tool);
        match &msg.content {
            MessageContent::ToolResult(r) => {
                assert_eq!(r.tool_call_id, "t1");
                assert_eq!(
                    r.content, "first\nblk1\nblk2\nboom",
                    "各块 content 按序拼接，is_error 块内容同样参与"
                );
                assert!(r.is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// System 消息为多模态 / 工具调用内容时的顶层 system 提取：
    /// 文本块拼接、非文本内容为空并被过滤。
    #[tokio::test]
    async fn test_anthropic_system_message_multimodal_lifted_to_top_level() {
        let converter = AnthropicConverter::new();
        let request = UnifiedRequest {
            model: "claude".into(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: MessageContent::MultiModal(vec![
                        ContentBlock::Text {
                            text: "visual rules".into(),
                        },
                        ContentBlock::Image {
                            source: ImageSource::Base64 {
                                media_type: "image/png".into(),
                                data: "Zm9v".into(),
                            },
                            detail: None,
                        },
                    ]),
                },
                Message {
                    role: Role::System,
                    content: MessageContent::ToolCalls(vec![]),
                },
                Message {
                    role: Role::User,
                    content: MessageContent::Text("hi".into()),
                },
            ],
            ..Default::default()
        };

        let external = converter.request_to_external(request).expect("to external");
        assert_eq!(
            external["system"], "visual rules",
            "多模态 System 取文本块拼接，非文本 System 被过滤"
        );
    }

    /// stop_reason 未知值双向原样保留（静态映射表不吞未知词汇）。
    #[tokio::test]
    async fn test_anthropic_stop_reason_unknown_passthrough() {
        let converter = AnthropicConverter::new();

        let parsed = converter
            .parse_response(json!({
                "id": "m", "model": "claude",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "pause_turn"
            }))
            .expect("parse");
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("pause_turn")
        );

        let response = UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content: MessageContent::Text("ok".into()),
                }),
                finish_reason: Some("content_filter".into()),
                finish: Some(FinishReason::ContentFilter),
                logprobs: None,
            }],
            usage: TokenUsage::default(),
        };
        let out = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(out["stop_reason"], "content_filter");
    }

    /// user 消息普通 blocks 数组：text/image/未知块 → MultiModal（未知块丢弃）。
    #[tokio::test]
    async fn test_anthropic_to_unified_multimodal_content_blocks() {
        let converter = AnthropicConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": "aGk="
                    }},
                    {"type": "unknown_block"}
                ]}]
            }))
            .expect("multimodal content");
        match &unified.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 2, "未知块类型应被丢弃");
                assert!(matches!(
                    &blocks[0],
                    ContentBlock::Text { text } if text == "describe this"
                ));
                match &blocks[1] {
                    ContentBlock::Image { source, detail } => {
                        match source {
                            ImageSource::Base64 { media_type, data } => {
                                assert_eq!(media_type, "image/png");
                                assert_eq!(data, "aGk=");
                            }
                            other => panic!("expected Base64 source, got {other:?}"),
                        }
                        assert!(detail.is_none());
                    }
                    other => panic!("expected Image block, got {other:?}"),
                }
            }
            other => panic!("expected MultiModal, got {other:?}"),
        }
    }

    /// 出站 MultiModal 消息 → Anthropic content blocks（text/image/unknown）。
    #[tokio::test]
    async fn test_anthropic_request_to_external_multimodal_message() {
        let converter = AnthropicConverter::new();
        let mut request = minimal_unified_request("claude", "");
        request.messages[0].content = MessageContent::MultiModal(vec![
            ContentBlock::Text {
                text: "describe".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/jpeg".into(),
                    data: "aGk=".into(),
                },
                detail: Some("high".into()),
            },
            ContentBlock::Type("weird".into()),
        ]);

        let external = converter.request_to_external(request).expect("to external");
        let content = external["messages"][0]["content"]
            .as_array()
            .expect("content array");
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], json!({"type": "text", "text": "describe"}));
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "aGk=");
        assert_eq!(content[2], json!({"type": "unknown"}));
    }

    /// response_to_external：System 角色消息 + 文本与工具调用并列 →
    /// Anthropic 形状（tool_use 块 input 反序列化）。
    #[tokio::test]
    async fn test_anthropic_response_to_external_system_role_with_tool_calls() {
        let converter = AnthropicConverter::new();
        let response = UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::System,
                    content: MessageContent::TextWithToolCalls {
                        text: "let me check".into(),
                        tool_calls: vec![ToolCall {
                            id: "t1".into(),
                            call_type: "function".into(),
                            function: ToolCallFunction {
                                name: "get_weather".into(),
                                arguments: "{\"city\":\"BJ\"}".into(),
                            },
                        }],
                    },
                }),
                finish_reason: Some("tool_calls".into()),
                finish: Some(FinishReason::ToolCalls),
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                ..Default::default()
            },
        };

        let out = converter
            .response_to_external(response)
            .expect("to external");
        assert_eq!(out["content"]["role"], "system");
        let blocks = out["content"]["content"]
            .as_array()
            .expect("content blocks");
        assert_eq!(blocks[0], json!({"type": "text", "text": "let me check"}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "t1");
        assert_eq!(blocks[1]["name"], "get_weather");
        assert_eq!(blocks[1]["input"], json!({"city": "BJ"}));
    }

    // ==================== 覆盖缺口补测：OpenAIConverter 边角分支 ====================

    #[tokio::test]
    async fn test_openai_converter_default() {
        let converter = <OpenAIConverter as Default>::default();
        assert_eq!(converter.input_protocol(), Protocol::OpenAI);
    }

    /// 采样参数与 logit_bias 入站解析：非数值 logit_bias 条目被丢弃。
    #[tokio::test]
    async fn test_openai_to_unified_sampling_and_logit_bias() {
        let converter = OpenAIConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "stop": ["END", "STOP"],
                "top_p": 0.9,
                "frequency_penalty": 0.5,
                "presence_penalty": 0.25,
                "user": "u-1",
                "logit_bias": {"50256": -100.0, "bad": "not-a-number"}
            }))
            .expect("to_unified");
        assert_eq!(unified.stop, vec!["END".to_string(), "STOP".to_string()]);
        assert_eq!(unified.top_p, Some(0.9));
        assert_eq!(unified.frequency_penalty, Some(0.5));
        assert_eq!(unified.presence_penalty, Some(0.25));
        assert_eq!(unified.user, Some("u-1".into()));
        assert_eq!(unified.logit_bias.len(), 1, "非数值 logit_bias 应被丢弃");
        assert_eq!(unified.logit_bias["50256"], -100.0);
    }

    /// 出站采样参数全字段落位。
    #[tokio::test]
    async fn test_openai_request_to_external_sampling_params() {
        let converter = OpenAIConverter::new();
        let request = UnifiedRequest {
            model: "gpt-4".into(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::Text("hi".into()),
            }],
            temperature: Some(0.3),
            max_tokens: Some(64),
            stream: true,
            stop: vec!["END".into()],
            top_p: Some(0.8),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
            logit_bias: Default::default(),
            user: Some("u-9".into()),
            ..Default::default()
        };

        let external = converter.request_to_external(request).expect("to external");
        // f32 经序列化后按 f32 精度比较，避免 0.3 这类二进制不精确值误报
        assert_eq!(external["temperature"], json!(0.3_f32));
        assert_eq!(external["stop"], json!(["END"]));
        assert_eq!(external["top_p"], json!(0.8_f32));
        assert_eq!(external["frequency_penalty"], json!(0.1_f32));
        assert_eq!(external["presence_penalty"], json!(0.2_f32));
        assert_eq!(external["user"], "u-9");
        assert_eq!(external["stream"], true);
    }

    /// parse_response：未知 role 归一 user；空 content + tool_calls → ToolCalls。
    #[tokio::test]
    async fn test_openai_parse_response_role_fallback_and_empty_content_tool_calls() {
        let converter = OpenAIConverter::new();

        let parsed = converter
            .parse_response(json!({
                "id": "r1", "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": {"role": "developer", "content": "hi"},
                    "finish_reason": "stop"
                }]
            }))
            .expect("unknown role");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        assert_eq!(msg.role, Role::User);

        let parsed = converter
            .parse_response(json!({
                "id": "r2", "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "", "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}
                    ]},
                    "finish_reason": "tool_calls"
                }]
            }))
            .expect("empty content with tool_calls");
        let msg = parsed.choices[0].message.as_ref().unwrap();
        assert!(matches!(
            &msg.content,
            MessageContent::ToolCalls(calls) if calls.len() == 1
        ));
    }

    /// 响应缺 usage 对象 → token 记账归零而非报错。
    #[tokio::test]
    async fn test_openai_parse_response_without_usage_defaults_to_zero() {
        let converter = OpenAIConverter::new();
        let parsed = converter
            .parse_response(json!({
                "id": "r", "model": "gpt-4",
                "choices": []
            }))
            .expect("parse without usage");
        assert_eq!(parsed.usage.prompt_tokens, 0);
        assert_eq!(parsed.usage.completion_tokens, 0);
        assert_eq!(parsed.usage.total_tokens, 0);
    }

    /// 流式出站全变体：Content（各 delta 形状）/ Usage 尾帧 / 心跳注释帧。
    #[tokio::test]
    async fn test_openai_stream_chunk_to_external_variants() {
        let converter = OpenAIConverter::new();

        let out = converter
            .stream_chunk_to_external(StreamChunk::Content {
                index: 2,
                delta: MessageContent::Text("hi".into()),
                finish_reason: Some("stop".into()),
            })
            .expect("content chunk");
        assert_eq!(out["id"], "chunk-2");
        assert_eq!(out["object"], "chat.completion.chunk");
        assert_eq!(out["choices"][0]["index"], 2);
        assert_eq!(out["choices"][0]["delta"]["content"], "hi");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");

        let delta_cases: Vec<(MessageContent, serde_json::Value)> = vec![
            (
                MessageContent::MultiModal(vec![ContentBlock::Text { text: "mm".into() }]),
                json!("mm"),
            ),
            (
                MessageContent::MultiModal(vec![ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "x".into(),
                    },
                    detail: None,
                }]),
                json!(""),
            ),
            (
                MessageContent::ToolCalls(vec![ToolCall {
                    id: "c".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "f".into(),
                        arguments: "{}".into(),
                    },
                }]),
                json!(""),
            ),
            (
                MessageContent::ToolResult(ToolResult {
                    tool_call_id: "c".into(),
                    function_name: String::new(),
                    content: "result-body".into(),
                    is_error: false,
                }),
                json!("result-body"),
            ),
            (
                MessageContent::TextWithToolCalls {
                    text: "mixed".into(),
                    tool_calls: vec![],
                },
                json!("mixed"),
            ),
        ];
        for (delta, expected) in delta_cases {
            let out = converter
                .stream_chunk_to_external(StreamChunk::Content {
                    index: 0,
                    delta,
                    finish_reason: None,
                })
                .expect("delta chunk");
            assert_eq!(out["choices"][0]["delta"]["content"], expected);
        }

        let out = converter
            .stream_chunk_to_external(StreamChunk::Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                finish_reason: None,
            })
            .expect("usage chunk");
        assert_eq!(out["choices"], json!([]));
        assert_eq!(out["usage"]["prompt_tokens"], 5);
        assert_eq!(out["usage"]["completion_tokens"], 3);
        assert_eq!(out["usage"]["total_tokens"], 8);

        let out = converter
            .stream_chunk_to_external(StreamChunk::Heartbeat)
            .expect("heartbeat");
        assert_eq!(out, json!({ ":": "" }));
    }

    /// 错误帧不映射 IR；choices 非空且带 usage 的普通帧不得误判为 usage 尾帧。
    #[tokio::test]
    async fn test_openai_parse_stream_frame_error_frame_and_inline_usage() {
        let converter = OpenAIConverter::new();
        assert!(converter
            .parse_stream_frame("{\"error\": {\"message\": \"upstream exploded\"}}")
            .is_none());

        let chunk = converter
            .parse_stream_frame(
                "{\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":5}}",
            )
            .expect("choices 非空的 usage 帧应按普通 chunk 解析");
        assert!(matches!(
            chunk,
            StreamChunk::Content { ref delta, .. }
                if matches!(delta, MessageContent::Text(t) if t == "a")
        ));
    }

    /// role 非法 / content 数字 → Validation 错误（field 精确）。
    #[tokio::test]
    async fn test_openai_to_unified_message_validation_errors() {
        let converter = OpenAIConverter::new();

        let err = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "developer", "content": "hi"}]
            }))
            .unwrap_err(); // 未知 role 必须报错
        match err {
            GateError::Validation { field } => assert_eq!(field, "role"),
            other => panic!("expected Validation error, got {other:?}"),
        }

        let err = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": 42}]
            }))
            .unwrap_err(); // content 数字必须报错
        match err {
            GateError::Validation { field } => assert_eq!(field, "content"),
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    /// content 空串 + tool_calls → ToolCalls；content null 无 tool_calls →
    /// 空文本；tool_calls 空数组视为无工具调用。
    #[tokio::test]
    async fn test_openai_to_unified_empty_and_null_content_variants() {
        let converter = OpenAIConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "assistant", "content": "", "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}
                ]}]
            }))
            .expect("empty content with tool_calls");
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::ToolCalls(calls) if calls.len() == 1
        ));

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": null}]
            }))
            .expect("null content");
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::Text(s) if s.is_empty()
        ));

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "assistant", "content": "", "tool_calls": []}]
            }))
            .expect("空 tool_calls 数组");
        assert!(matches!(
            &unified.messages[0].content,
            MessageContent::Text(s) if s.is_empty()
        ));
    }

    /// content 数组四种形状：tool_call 块 / 兼容 function 块（arguments 支持
    /// 对象）/ function 缺参降级 MultiModal / 纯多模态（image_url detail 有无）。
    #[tokio::test]
    async fn test_openai_to_unified_array_content_variants() {
        let converter = OpenAIConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "assistant", "content": [
                    {"type": "tool_call", "id": "c1", "function": {"name": "f", "arguments": "{}"}}
                ]}]
            }))
            .expect("tool_call 块");
        match &unified.messages[0].content {
            MessageContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "c1");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "assistant", "content": [
                    {"type": "custom_tool", "id": "c2", "function": {"name": "g", "arguments": {"city": "BJ"}}},
                    {"type": "text", "text": "ignored alongside tools"}
                ]}]
            }))
            .expect("兼容 function 块");
        match &unified.messages[0].content {
            MessageContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1, "非工具块在工具聚合中被忽略");
                assert_eq!(calls[0].id, "c2");
                assert_eq!(calls[0].function.arguments, "{\"city\":\"BJ\"}");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "assistant", "content": [
                    {"type": "custom_tool", "function": {"name": "g"}}
                ]}]
            }))
            .expect("function 块缺 arguments");
        assert!(
            matches!(&unified.messages[0].content, MessageContent::MultiModal(_)),
            "解析全部失败时降级为 MultiModal 而非臆造工具调用"
        );

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png", "detail": "high"}},
                    {"type": "image_url", "image_url": {"url": "https://example.com/b.png"}},
                    {"type": "skipped_type"}
                ]}]
            }))
            .expect("纯多模态数组");
        match &unified.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert_eq!(blocks.len(), 3, "未知块类型应被丢弃");
                assert!(matches!(
                    &blocks[0],
                    ContentBlock::Text { text } if text == "look"
                ));
                match &blocks[1] {
                    ContentBlock::Image { source, detail } => {
                        // T003：URL 保真——https 直链进 Url 形态（旧实现硬编码
                        // jpeg 并把 URL 塞 data 字段，往返产出坏 URI）
                        match source {
                            ImageSource::Url { url } => {
                                assert_eq!(url, "https://example.com/a.png");
                            }
                            other => panic!("expected Url source, got {other:?}"),
                        }
                        assert_eq!(detail.as_deref(), Some("high"));
                    }
                    other => panic!("expected Image, got {other:?}"),
                }
                assert!(matches!(
                    &blocks[2],
                    ContentBlock::Image { detail: None, .. }
                ));
            }
            other => panic!("expected MultiModal, got {other:?}"),
        }
    }

    /// 出站 MultiModal（data URL 拼 base64、detail 缺省回退 auto、未知块空对象）
    /// 与 ToolCalls（content 为 null + 顶层 tool_calls）两种消息形状。
    #[tokio::test]
    async fn test_openai_request_to_external_multimodal_and_tool_calls_messages() {
        let converter = OpenAIConverter::new();
        let mut request = minimal_unified_request("gpt-4", "");
        request.messages[0].content = MessageContent::MultiModal(vec![
            ContentBlock::Text {
                text: "look".into(),
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".into(),
                    data: "aGk=".into(),
                },
                detail: None,
            },
            ContentBlock::Type("weird".into()),
        ]);
        request.messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCalls(vec![ToolCall {
                id: "c1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                },
            }]),
        });

        let external = converter.request_to_external(request).expect("to external");
        let msgs = external["messages"].as_array().expect("messages");

        let blocks = msgs[0]["content"].as_array().expect("multimodal array");
        assert_eq!(blocks[0], json!({"type": "text", "text": "look"}));
        assert_eq!(blocks[1]["type"], "image_url");
        assert_eq!(blocks[1]["image_url"]["url"], "data:image/png;base64,aGk=");
        assert_eq!(
            blocks[1]["image_url"]["detail"], "auto",
            "detail 缺省时回退 auto"
        );
        assert_eq!(blocks[2], json!({}), "未知块类型出站为空对象");

        assert!(
            msgs[1]["content"].is_null(),
            "工具调用消息 content 必须为 null"
        );
        assert_eq!(msgs[1]["tool_calls"][0]["id"], "c1");
        assert_eq!(msgs[1]["tool_calls"][0]["type"], "function");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], "{}");
    }

    /// 出站工具 schema：property 的 description/enum 与 required 数组落位，
    /// Specific tool_choice 序列化为 OpenAI 对象形状。
    #[tokio::test]
    async fn test_openai_request_to_external_full_tool_schema() {
        let converter = OpenAIConverter::new();
        let mut request = minimal_unified_request("gpt-4", "hi");
        request.tools = vec![Tool {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "city": {
                            "type": "string",
                            "description": "City name",
                            "enum": ["Beijing", "Shanghai"],
                        },
                    },
                    "required": ["city"],
                }),
            },
        }];
        request.tool_choice = Some(ToolChoice::Specific {
            choice_type: "function".into(),
            function: ToolChoiceFunction {
                name: "get_weather".into(),
            },
        });

        let external = converter.request_to_external(request).expect("to external");
        let tool = &external["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "get_weather");
        let params = &tool["function"]["parameters"];
        assert_eq!(params["properties"]["city"]["description"], "City name");
        assert_eq!(
            params["properties"]["city"]["enum"],
            json!(["Beijing", "Shanghai"])
        );
        assert_eq!(params["required"], json!(["city"]));

        assert_eq!(external["tool_choice"]["type"], "function");
        assert_eq!(external["tool_choice"]["function"]["name"], "get_weather");
    }

    /// 工具参数枚举值入站解析。
    #[tokio::test]
    async fn test_openai_to_unified_tool_with_enum_property() {
        let converter = OpenAIConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "city": {"type": "string", "description": "City", "enum": ["Beijing", "Shanghai"]}
                            },
                            "required": ["city"]
                        }
                    }
                }]
            }))
            .expect("to_unified");
        // A3 保真改造：parameters 原样 Value——enum 数组原样幸存
        assert_eq!(
            unified.tools[0].function.parameters["properties"]["city"]["enum"],
            json!(["Beijing", "Shanghai"])
        );
        assert_eq!(
            unified.tools[0].function.parameters["required"],
            json!(["city"])
        );
    }

    /// tool_choice 兜底：无 function 的对象与其他标量都归 Strategy("auto")。
    #[tokio::test]
    async fn test_openai_to_unified_tool_choice_fallbacks() {
        let converter = OpenAIConverter::new();

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": {"type": "any"}
            }))
            .expect("object without function");
        assert!(matches!(
            unified.tool_choice,
            Some(ToolChoice::Strategy(s)) if s == "auto"
        ));

        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "tool_choice": 42
            }))
            .expect("标量 tool_choice");
        assert!(matches!(
            unified.tool_choice,
            Some(ToolChoice::Strategy(s)) if s == "auto"
        ));
    }

    // ==================== ProtocolConverter trait 默认方法契约 ====================

    /// can_passthrough 仅在同协议时为真；透传方法原样返回请求/响应体。
    #[tokio::test]
    async fn test_converter_passthrough_defaults() {
        let openai = OpenAIConverter::new();
        let anthropic = AnthropicConverter::new();

        assert!(openai.can_passthrough(Protocol::OpenAI));
        assert!(
            !openai.can_passthrough(Protocol::Anthropic),
            "跨协议不得透传"
        );
        assert!(anthropic.can_passthrough(Protocol::Anthropic));

        let body = json!({"model": "gpt-4", "messages": []});
        assert_eq!(
            openai.passthrough_request(body.clone()).unwrap(),
            body,
            "请求透传必须零改动"
        );
        assert_eq!(
            openai.passthrough_response(body.clone()).unwrap(),
            body,
            "响应透传必须零改动"
        );
    }

    // ==================== T003：IR 一次补全（design.md D2） ====================

    use llm_gate::protocol::{EffortLevel, FinishReason, SamplingParams, ThinkingConfig};

    /// UnifiedRequest 全字段字面量构造（覆盖 IR serde default 之外的显式路径）。
    fn full_unified_request() -> UnifiedRequest {
        UnifiedRequest {
            model: "gpt-4".into(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::Text("hi".into()),
            }],
            ..Default::default()
        }
    }

    // ---- OpenAI：effort / vendor_extensions ----

    /// 入站：reasoning_effort → effort；未识别顶层字段（top_k/seed）吸收进
    /// vendor_extensions（防丢）；已知键不吸收。出站：双通道透传。
    #[tokio::test]
    async fn test_openai_effort_and_vendor_extensions_roundtrip() {
        let openai = OpenAIConverter::new();
        let unified = openai
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}],
                "reasoning_effort": "high",
                "top_k": 40,
                "seed": 42
            }))
            .expect("to_unified");
        assert_eq!(unified.effort, Some(EffortLevel::High));
        assert_eq!(unified.vendor_extensions.get("seed"), Some(&json!(42)));
        assert_eq!(unified.vendor_extensions.get("top_k"), Some(&json!(40)));
        assert!(!unified.vendor_extensions.contains_key("model"));
        assert!(!unified.vendor_extensions.contains_key("messages"));
        assert!(!unified.vendor_extensions.contains_key("temperature"));

        let out = openai.request_to_external(unified).expect("to external");
        assert_eq!(out["reasoning_effort"], "high", "effort → reasoning_effort");
        assert_eq!(out["seed"], 42, "vendor_extensions 顶层透传");
        assert_eq!(out["top_k"], 40);
    }

    /// F1/M1 denylist：协议键永不由扩展表补入——openai 侧 stream 缺位
    /// 注入与 effort 经扩展表物化均被拦截；非协议键仍透传。
    #[tokio::test]
    async fn test_openai_vendor_extensions_denylist_protocol_keys() {
        let openai = OpenAIConverter::new();
        let mut request = full_unified_request();
        request.stream = false;
        request
            .vendor_extensions
            .insert("stream".into(), json!(true));
        request
            .vendor_extensions
            .insert("reasoning_effort".into(), json!("low"));
        request
            .vendor_extensions
            .insert("x_allowed".into(), json!("pass"));

        let out = openai.request_to_external(request).expect("to external");
        assert!(
            out.get("stream").is_none(),
            "stream 协议键不得由扩展表补入（缺位注入消灭）"
        );
        assert!(
            out.get("reasoning_effort").is_none(),
            "effort 协议键只经类型化字段可达"
        );
        assert_eq!(out["x_allowed"], "pass", "非协议键仍透传（D2 语义）");
    }

    /// F1/M1 denylist：跨协议计费/行为键不得补入——anthropic 侧 thinking
    /// 协议键过滤；metadata 为非协议键，同协议透传保留（400 风险 opt-in，D7）。
    #[tokio::test]
    async fn test_anthropic_vendor_extensions_denylist_protocol_keys() {
        let anthropic = AnthropicConverter::new();
        let mut request = minimal_unified_request("claude", "hi");
        request.vendor_extensions.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": 1}),
        );
        request
            .vendor_extensions
            .insert("metadata".into(), json!({"user_id": "u"}));
        request
            .vendor_extensions
            .insert("x_allowed".into(), json!("pass"));

        let out = anthropic.request_to_external(request).expect("to external");
        assert!(
            out.get("thinking").is_none(),
            "thinking 协议键不得由扩展表补入（跨协议计费行为键消灭）"
        );
        assert_eq!(out["x_allowed"], "pass", "非协议键仍透传（D2 语义）");
        assert_eq!(
            out["metadata"]["user_id"], "u",
            "metadata 非 IR 建模键，同协议透传保留"
        );
    }

    /// 出站：sampling 只补 flat 未设置的槽（flat 优先不覆写）；sampling.top_k
    /// 直出 top_k；effort → reasoning_effort 小写。
    #[tokio::test]
    async fn test_openai_request_to_external_sampling_and_effort() {
        let openai = OpenAIConverter::new();
        let mut request = full_unified_request();
        request.temperature = Some(0.5);
        request.sampling = Some(SamplingParams {
            temperature: Some(0.9),
            top_k: Some(40),
            ..Default::default()
        });
        request.effort = Some(EffortLevel::Medium);

        let out = openai.request_to_external(request).expect("to external");
        assert_eq!(out["temperature"], 0.5, "flat 槽优先，sampling 不覆写");
        assert_eq!(out["top_k"], 40);
        assert_eq!(out["reasoning_effort"], "medium");
    }

    /// 出站：system_prompt → 首条 system 消息；与 Role::System 消息并存时
    /// system_prompt 优先合并去重；messages 为空时新建。
    #[tokio::test]
    async fn test_openai_request_to_external_system_prompt_merge() {
        let openai = OpenAIConverter::new();

        // 并存去重：相同文本只保留一份
        let mut request = full_unified_request();
        request.system_prompt = Some("be terse".into());
        request.messages.insert(
            0,
            Message {
                role: Role::System,
                content: MessageContent::Text("be terse".into()),
            },
        );
        let out = openai.request_to_external(request).expect("to external");
        let msgs = out["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 2, "重复 system 文本应去重");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be terse");
        assert_eq!(msgs[1]["role"], "user");

        // 不同文本：system_prompt 在前合并
        let mut request = full_unified_request();
        request.system_prompt = Some("new policy".into());
        request.messages.insert(
            0,
            Message {
                role: Role::System,
                content: MessageContent::Text("old policy".into()),
            },
        );
        let out = openai.request_to_external(request).expect("to external");
        let msgs = out["messages"].as_array().expect("messages");
        assert_eq!(msgs[0]["content"], "new policy\n\nold policy");

        // messages 为空：新建首条 system 消息
        let mut request = full_unified_request();
        request.messages.clear();
        request.system_prompt = Some("be terse".into());
        let out = openai.request_to_external(request).expect("to external");
        let msgs = out["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be terse");

        // 无 system_prompt：行为不变（system 消息原位保留）
        let request = full_unified_request();
        let out = openai.request_to_external(request).expect("to external");
        assert!(out["messages"].as_array().unwrap().len() == 1);
        assert!(out.get("system").is_none());
    }

    // ---- OpenAI：usage cache token + finish_reason 结构化 ----

    /// 入站：prompt_tokens_details.cached_tokens → cache_read_tokens
    ///（OpenAI 无 cache_creation 概念 → 0）；finish_reason 已知词双写结构化槽。
    #[tokio::test]
    async fn test_openai_parse_response_cache_tokens_and_finish() {
        let openai = OpenAIConverter::new();
        let parsed = openai
            .parse_response(json!({
                "id": "c1", "model": "gpt-4", "created": 1,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25,
                    "prompt_tokens_details": {"cached_tokens": 8}
                }
            }))
            .expect("parse_response");
        assert_eq!(parsed.usage.cache_read_tokens, 8);
        assert_eq!(parsed.usage.cache_creation_tokens, 0);
        assert_eq!(parsed.choices[0].finish, Some(FinishReason::ToolCalls));
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );

        let parsed = openai
            .parse_response(json!({
                "id": "c1", "model": "gpt-4", "created": 1,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "content_filter"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
            .expect("parse_response");
        assert_eq!(parsed.choices[0].finish, Some(FinishReason::ContentFilter));

        // 未知词：结构化槽 None，原始槽原样保留
        let parsed = openai
            .parse_response(json!({
                "id": "c1", "model": "gpt-4", "created": 1,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "exotic_reason"
                }],
                "usage": {}
            }))
            .expect("parse_response");
        assert_eq!(parsed.choices[0].finish, None);
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("exotic_reason")
        );
    }

    /// 出站：cache_read_tokens → usage.prompt_tokens_details.cached_tokens
    ///（非零才携带）；cache_creation 无 OpenAI 槽，不产出。
    #[tokio::test]
    async fn test_openai_response_to_external_cache_tokens() {
        let openai = OpenAIConverter::new();
        let response = UnifiedResponse {
            id: "c1".into(),
            object: "chat.completion".into(),
            created: 1,
            model: "gpt-4".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content: MessageContent::Text("ok".into()),
                }),
                finish_reason: Some("stop".into()),
                finish: Some(FinishReason::Stop),
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 20,
                completion_tokens: 5,
                total_tokens: 25,
                cache_read_tokens: 8,
                cache_creation_tokens: 4,
                cost_usd: None,
                cost_cny: None,
            },
        };
        let out = openai.response_to_external(response).expect("to external");
        assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 8);
        assert!(
            out["usage"].get("cache_creation_tokens").is_none(),
            "cache_creation 无 OpenAI 槽，不得外泄"
        );

        // 零值不携带 prompt_tokens_details
        let parsed = openai
            .parse_response(json!({
                "id": "c2", "model": "gpt-4", "created": 1,
                "choices": [], "usage": {}
            }))
            .expect("parse_response");
        let out = openai.response_to_external(parsed).expect("to external");
        assert!(out["usage"].get("prompt_tokens_details").is_none());
    }

    /// 流式：usage 尾帧带 prompt_tokens_details → Usage{cache_read_tokens}。
    #[tokio::test]
    async fn test_openai_stream_usage_frame_cache_tokens() {
        let openai = OpenAIConverter::new();
        let chunk = openai
            .parse_stream_frame(
                "{\"id\":\"x\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":7,\"total_tokens\":17,\"prompt_tokens_details\":{\"cached_tokens\":6}}}",
            )
            .expect("usage 尾帧应解析");
        match chunk {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: _,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
            } => {
                assert_eq!((prompt_tokens, completion_tokens), (10, 7));
                assert_eq!(cache_read_tokens, 6);
                assert_eq!(cache_creation_tokens, 0);
                assert_eq!(finish_reason, None);
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }
    }

    // ---- OpenAI：StreamChunk::ToolCall ----

    /// 出站：ToolCall → OpenAI delta.tool_calls 片段（首片段带 id/name，
    /// 续片段省略）；该片段可经 parse_stream_frame 回读聚合。
    #[tokio::test]
    async fn test_openai_stream_tool_call_chunk_roundtrip() {
        let openai = OpenAIConverter::new();
        let out = openai
            .stream_chunk_to_external(StreamChunk::ToolCall {
                index: 0,
                id: "call_1".into(),
                name: Some("get_weather".into()),
                arguments: Some("{\"city\":".into()),
            })
            .expect("tool_call chunk");
        assert_eq!(out["choices"][0]["finish_reason"], serde_json::Value::Null);
        let fragment = &out["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(fragment["index"], 0);
        assert_eq!(fragment["id"], "call_1");
        assert_eq!(fragment["function"]["name"], "get_weather");
        assert_eq!(fragment["function"]["arguments"], "{\"city\":");

        // 续片段：id/name 为空时省略
        let out = openai
            .stream_chunk_to_external(StreamChunk::ToolCall {
                index: 0,
                id: String::new(),
                name: None,
                arguments: Some(":5}".into()),
            })
            .expect("tool_call continuation chunk");
        let fragment = &out["choices"][0]["delta"]["tool_calls"][0];
        assert!(fragment.get("id").is_none(), "空 id 应省略");
        assert!(
            fragment["function"].get("name").is_none(),
            "None name 应省略"
        );
        assert_eq!(fragment["function"]["arguments"], ":5}");

        // 回读：片段帧 → ToolCall 续片段（T004 表示统一）
        let chunk = openai
            .parse_stream_frame(&out.to_string())
            .expect("片段帧应回读");
        match chunk {
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, 0);
                assert_eq!(id, "", "续片段无 id");
                assert_eq!(name, None);
                assert_eq!(arguments.as_deref(), Some(":5}"));
            }
            other => panic!("expected ToolCall fragment, got {other:?}"),
        }
    }

    // ---- OpenAI：ImageSource::Url 保真 ----

    /// 入站：https URL → Url 形态（不再硬编码 jpeg/塞 data）；data URI →
    /// Base64 形态（media_type 从 URI 解析）。出站：Url 直出 url 字段。
    #[tokio::test]
    async fn test_openai_image_url_source_roundtrip() {
        let openai = OpenAIConverter::new();
        let unified = openai
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/cat.png"}},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGk="}}
                ]}]
            }))
            .expect("to_unified");
        match &unified.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                match &blocks[1] {
                    ContentBlock::Image { source, .. } => match source {
                        ImageSource::Url { url } => {
                            assert_eq!(url, "https://example.com/cat.png");
                        }
                        other => panic!("expected Url source, got {other:?}"),
                    },
                    other => panic!("expected Image block, got {other:?}"),
                }
                match &blocks[2] {
                    ContentBlock::Image { source, .. } => match source {
                        ImageSource::Base64 { media_type, data } => {
                            assert_eq!(media_type, "image/png");
                            assert_eq!(data, "aGk=");
                        }
                        other => panic!("expected Base64 source, got {other:?}"),
                    },
                    other => panic!("expected Image block, got {other:?}"),
                }
            }
            other => panic!("expected MultiModal, got {other:?}"),
        }

        // 出站：Url 直出（不得再包 data: 前缀）
        let mut request = full_unified_request();
        request.messages[0].content = MessageContent::MultiModal(vec![
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/cat.png".into(),
                },
                detail: None,
            },
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/jpeg".into(),
                    data: "aGk=".into(),
                },
                detail: None,
            },
        ]);
        let out = openai.request_to_external(request).expect("to external");
        let blocks = out["messages"][0]["content"].as_array().expect("content");
        assert_eq!(
            blocks[0]["image_url"]["url"], "https://example.com/cat.png",
            "URL 必须原样直出，不得塞进 data 字段"
        );
        assert_eq!(blocks[1]["image_url"]["url"], "data:image/jpeg;base64,aGk=");
    }

    // ---- Anthropic：system_prompt 双写与合并 ----

    /// 入站：顶层 system → Role::System 消息（兼容形态照旧）+ system_prompt
    /// 回填（规范槽）。string 与 blocks 数组两形状。
    #[tokio::test]
    async fn test_anthropic_system_prompt_backfill() {
        let anthropic = AnthropicConverter::new();
        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "system": "be terse",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        assert_eq!(unified.system_prompt.as_deref(), Some("be terse"));
        match &unified.messages[0] {
            Message {
                role: Role::System,
                content: MessageContent::Text(text),
            } => assert_eq!(text, "be terse"),
            other => panic!("expected leading System message, got {other:?}"),
        }

        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "system": [
                    {"type": "text", "text": "a"},
                    {"type": "text", "text": "b"}
                ],
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("to_unified");
        assert_eq!(unified.system_prompt.as_deref(), Some("a\nb"));
    }

    /// 出站：system_prompt → 顶层 system；与 Role::System 消息并存时
    /// system_prompt 优先合并去重；仅 Role::System 消息时行为照旧。
    #[tokio::test]
    async fn test_anthropic_request_to_external_system_prompt_merge() {
        let anthropic = AnthropicConverter::new();

        // 仅 system_prompt
        let mut request = minimal_unified_request("claude", "hi");
        request.system_prompt = Some("be terse".into());
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(out["system"], "be terse");

        // 并存去重：相同文本一份
        let mut request = minimal_unified_request("claude", "hi");
        request.system_prompt = Some("be terse".into());
        request.messages.insert(
            0,
            Message {
                role: Role::System,
                content: MessageContent::Text("be terse".into()),
            },
        );
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(out["system"], "be terse", "相同文本只保留一份");

        // 不同文本：system_prompt 优先在前
        let mut request = minimal_unified_request("claude", "hi");
        request.system_prompt = Some("new policy".into());
        request.messages.insert(
            0,
            Message {
                role: Role::System,
                content: MessageContent::Text("old policy".into()),
            },
        );
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(out["system"], "new policy\n\nold policy");

        // 仅 Role::System 消息（无 system_prompt）：既有提升行为不变
        let request = minimal_unified_request("claude", "hi");
        let out = anthropic.request_to_external(request).expect("to external");
        assert!(
            out.get("system").is_none(),
            "无 system 内容不得出 system 键"
        );
    }

    // ---- Anthropic：usage cache token ----

    /// 入站：cache_read_input_tokens/cache_creation_input_tokens → IR cache 字段。
    #[tokio::test]
    async fn test_anthropic_parse_response_cache_tokens() {
        let anthropic = AnthropicConverter::new();
        let parsed = anthropic
            .parse_response(json!({
                "id": "m", "model": "claude",
                "content": [{"type": "text", "text": "ok"}],
                "usage": {
                    "input_tokens": 25, "output_tokens": 9,
                    "cache_read_input_tokens": 12,
                    "cache_creation_input_tokens": 4
                }
            }))
            .expect("parse_response");
        assert_eq!(parsed.usage.prompt_tokens, 25);
        assert_eq!(parsed.usage.completion_tokens, 9);
        assert_eq!(parsed.usage.cache_read_tokens, 12);
        assert_eq!(parsed.usage.cache_creation_tokens, 4);
    }

    /// 出站：IR cache 字段 → cache_read_input_tokens/cache_creation_input_tokens
    ///（非零才携带）。
    #[tokio::test]
    async fn test_anthropic_response_to_external_cache_tokens() {
        let anthropic = AnthropicConverter::new();
        let response = UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content: MessageContent::Text("ok".into()),
                }),
                finish_reason: None,
                finish: None,
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 25,
                completion_tokens: 9,
                total_tokens: 34,
                cache_read_tokens: 12,
                cache_creation_tokens: 4,
                cost_usd: None,
                cost_cny: None,
            },
        };
        let out = anthropic
            .response_to_external(response)
            .expect("to external");
        assert_eq!(out["usage"]["cache_read_input_tokens"], 12);
        assert_eq!(out["usage"]["cache_creation_input_tokens"], 4);

        // 零值不携带
        let response = UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude".into(),
            choices: vec![Choice::default()],
            usage: TokenUsage::default(),
        };
        let out = anthropic
            .response_to_external(response)
            .expect("to external");
        assert!(out["usage"].get("cache_read_input_tokens").is_none());
        assert!(out["usage"].get("cache_creation_input_tokens").is_none());
    }

    // ---- Anthropic：finish_reason 结构化（非流式全映射 + 流式 Stop/ToolCalls）----

    /// 非流式：stop_reason → finish 结构化槽四变体映射（含未知词不猜变体）。
    #[tokio::test]
    async fn test_anthropic_parse_response_finish_structured() {
        let anthropic = AnthropicConverter::new();
        let cases = [
            ("end_turn", FinishReason::Stop),
            ("stop_sequence", FinishReason::Stop),
            ("max_tokens", FinishReason::Length),
            ("tool_use", FinishReason::ToolCalls),
        ];
        for (stop_reason, expected) in cases {
            let parsed = anthropic
                .parse_response(json!({
                    "id": "m", "model": "claude",
                    "content": [{"type": "text", "text": "ok"}],
                    "stop_reason": stop_reason,
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }))
                .expect("parse_response");
            assert_eq!(
                parsed.choices[0].finish,
                Some(expected),
                "stop_reason={stop_reason}"
            );
        }

        // 未知词：finish None，string 槽原样透传
        let parsed = anthropic
            .parse_response(json!({
                "id": "m", "model": "claude",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "pause_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .expect("parse_response");
        assert_eq!(parsed.choices[0].finish, None);
        assert_eq!(
            parsed.choices[0].finish_reason.as_deref(),
            Some("pause_turn")
        );
    }

    /// 流式：message_delta 尾帧 stop_reason 与 usage 同帧到达 → Usage
    /// finish_reason 承载（映射后）；cache token 同帧读取。
    #[tokio::test]
    async fn test_anthropic_stream_message_delta_stop_reason_and_cache() {
        let anthropic = AnthropicConverter::new();
        let chunk = anthropic
            .parse_stream_frame(
                "{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9,\"cache_read_input_tokens\":12,\"cache_creation_input_tokens\":4}}",
            )
            .expect("message_delta 应解析");
        match chunk {
            StreamChunk::Usage {
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
                ..
            } => {
                assert_eq!(completion_tokens, 9);
                assert_eq!(cache_read_tokens, 12);
                assert_eq!(cache_creation_tokens, 4);
                assert_eq!(finish_reason.as_deref(), Some("stop"));
                assert_eq!(
                    finish_reason.as_deref().and_then(FinishReason::from_wire),
                    Some(FinishReason::Stop),
                    "流式 Stop 变体"
                );
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }

        let chunk = anthropic
            .parse_stream_frame(
                "{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}",
            )
            .expect("message_delta 应解析");
        match chunk {
            StreamChunk::Usage { finish_reason, .. } => {
                assert_eq!(
                    finish_reason.as_deref().and_then(FinishReason::from_wire),
                    Some(FinishReason::ToolCalls),
                    "流式 ToolCalls 变体"
                );
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }

        // 无 stop_reason：finish_reason None
        let chunk = anthropic
            .parse_stream_frame("{\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}")
            .expect("message_delta 应解析");
        match chunk {
            StreamChunk::Usage { finish_reason, .. } => assert_eq!(finish_reason, None),
            other => panic!("expected Usage chunk, got {other:?}"),
        }
    }

    // ---- Anthropic：tool_choice 原生形状 ----

    /// 入站：Anthropic 原生 any/tool/auto/none → IR；OpenAI function 对象
    /// 形状保持容错；标量兜底 auto。
    #[tokio::test]
    async fn test_anthropic_to_unified_tool_choice_native_mapping() {
        let anthropic = AnthropicConverter::new();
        let parse = |tool_choice: serde_json::Value| {
            anthropic
                .to_unified(json!({
                    "model": "claude",
                    "messages": [{"role": "user", "content": "hi"}],
                    "tool_choice": tool_choice
                }))
                .expect("to_unified")
                .tool_choice
        };

        assert!(
            matches!(parse(json!({"type": "any"})), Some(ToolChoice::Strategy(s)) if s == "required"),
            "{{type:any}} 应映射 required"
        );
        assert!(
            matches!(parse(json!({"type": "auto"})), Some(ToolChoice::Strategy(s)) if s == "auto"),
        );
        assert!(
            matches!(parse(json!({"type": "none"})), Some(ToolChoice::Strategy(s)) if s == "none"),
        );
        match parse(json!({"type": "tool", "name": "get_weather"})) {
            Some(ToolChoice::Specific { function, .. }) => {
                assert_eq!(function.name, "get_weather");
            }
            other => panic!("expected Specific, got {other:?}"),
        }
    }

    /// 出站：IR → Anthropic 原生形状（required→any；Specific→tool），
    /// 不再写 OpenAI 形状。
    #[tokio::test]
    async fn test_anthropic_request_to_external_tool_choice_native_shape() {
        let anthropic = AnthropicConverter::new();
        let external_of = |tool_choice: ToolChoice| {
            let mut request = minimal_unified_request("claude", "hi");
            request.tool_choice = Some(tool_choice);
            anthropic.request_to_external(request).expect("to external")["tool_choice"].clone()
        };

        assert_eq!(
            external_of(ToolChoice::Strategy("required".into())),
            json!({"type": "any"}),
            "required 必须写 Anthropic any 形状"
        );
        assert_eq!(
            external_of(ToolChoice::Strategy("any".into())),
            json!({"type": "any"})
        );
        assert_eq!(
            external_of(ToolChoice::Strategy("auto".into())),
            json!({"type": "auto"})
        );
        assert_eq!(
            external_of(ToolChoice::Specific {
                choice_type: "function".into(),
                function: ToolChoiceFunction {
                    name: "get_weather".into(),
                },
            }),
            json!({"type": "tool", "name": "get_weather"}),
            "Specific 必须写 Anthropic tool 形状"
        );

        // L3："none" 省略 tool_choice 字段（Anthropic 传统词汇为
        // auto/any/tool，无 none 形状；缺省即 auto 语义）
        let mut request = minimal_unified_request("claude", "hi");
        request.tool_choice = Some(ToolChoice::Strategy("none".into()));
        let out = anthropic.request_to_external(request).expect("to external");
        assert!(
            out.get("tool_choice").is_none(),
            "none 必须省略 tool_choice 字段而非写未知词"
        );
    }

    // ---- Anthropic：thinking_config / top_k / effort / vendor_extensions ----

    /// 入站：thinking enabled+budget → Enabled；出站 Enabled → thinking 块；
    /// Adaptive/Disabled 出站省略（保守选择，见待裁决项）。
    #[tokio::test]
    async fn test_anthropic_thinking_config_roundtrip() {
        let anthropic = AnthropicConverter::new();
        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "enabled", "budget_tokens": 2048}
            }))
            .expect("to_unified");
        assert_eq!(
            unified.thinking_config,
            Some(ThinkingConfig::Enabled {
                budget_tokens: 2048
            })
        );

        let mut request = minimal_unified_request("claude", "hi");
        request.thinking_config = Some(ThinkingConfig::Enabled {
            budget_tokens: 2048,
        });
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(
            out["thinking"],
            json!({"type": "enabled", "budget_tokens": 2048})
        );

        // Adaptive：Anthropic 无对应，保守省略（不出站）
        let mut request = minimal_unified_request("claude", "hi");
        request.thinking_config = Some(ThinkingConfig::Adaptive);
        let out = anthropic.request_to_external(request).expect("to external");
        assert!(out.get("thinking").is_none(), "Adaptive 保守省略");

        // Disabled：Anthropic 缺省即禁用，省略
        let mut request = minimal_unified_request("claude", "hi");
        request.thinking_config = Some(ThinkingConfig::Disabled);
        let out = anthropic.request_to_external(request).expect("to external");
        assert!(out.get("thinking").is_none());
    }

    /// 入站：top_k/temperature/top_p → sampling；出站 sampling.top_k → top_k，
    /// temperature flat 优先、sampling 不覆写。
    #[tokio::test]
    async fn test_anthropic_sampling_top_k_roundtrip() {
        let anthropic = AnthropicConverter::new();
        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "temperature": 0.5,
                "top_p": 0.9,
                "top_k": 40
            }))
            .expect("to_unified");
        let sampling = unified.sampling.expect("sampling 应回填");
        assert_eq!(sampling.temperature, Some(0.5));
        assert_eq!(sampling.top_p, Some(0.9));
        assert_eq!(sampling.top_k, Some(40));
        assert_eq!(unified.temperature, Some(0.5), "flat 槽照旧");

        let mut request = minimal_unified_request("claude", "hi");
        request.temperature = Some(0.5);
        request.sampling = Some(SamplingParams {
            temperature: Some(0.9),
            top_k: Some(40),
            ..Default::default()
        });
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(out["temperature"], 0.5, "flat 优先，sampling 不覆写");
        assert_eq!(out["top_k"], 40);
    }

    /// M2：Anthropic 无原生 effort——出站不写顶层 effort（first-party
    /// 严格校验会 400）；effort 仅经 vendor_extensions 显式携带时可达上游
    ///（denylist 不含 effort = opt-in，400 风险自担）。
    #[tokio::test]
    async fn test_anthropic_effort_option_via_vendor_extensions() {
        let anthropic = AnthropicConverter::new();

        // IR 类型化 effort 字段不物化顶层键（无条件写入已删除）
        let mut request = minimal_unified_request("claude", "hi");
        request.effort = Some(EffortLevel::High);
        let out = anthropic.request_to_external(request).expect("to external");
        assert!(
            out.get("effort").is_none(),
            "IR effort 不得无条件物化为顶层 effort 键"
        );

        // 入站 effort → vendor_extensions 吸收，出站显式透传（opt-in）
        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": "hi"}],
                "effort": "high"
            }))
            .expect("to_unified");
        assert!(unified.effort.is_none(), "anthropic 入站不解析 effort");
        assert_eq!(
            unified.vendor_extensions.get("effort"),
            Some(&json!("high")),
            "effort 落扩展表（known_keys 不含 effort）"
        );
        let out = anthropic.request_to_external(unified).expect("to external");
        assert_eq!(out["effort"], "high", "显式 opt-in 透传");

        // denylist 仍拦截协议关键键覆写
        let mut request = minimal_unified_request("claude", "hi");
        request
            .vendor_extensions
            .insert("x_gateway".into(), json!({"flag": true}));
        request
            .vendor_extensions
            .insert("model".into(), json!("hijacked"));
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(out["x_gateway"]["flag"], true);
        assert_eq!(out["model"], "claude", "扩展键不得覆写协议关键键");
    }

    // ---- Anthropic：StreamChunk::ToolCall ----

    /// 入站：content_block_start(tool_use) 与 input_json_delta → ToolCall
    /// 片段（并行工具 = 多 index 交错）。
    #[tokio::test]
    async fn test_anthropic_stream_tool_call_frames_parse_to_ir() {
        let anthropic = AnthropicConverter::new();
        let chunk = anthropic
            .parse_stream_frame(
                "{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"get_weather\",\"input\":{}}}",
            )
            .expect("tool_use start 应解析");
        match chunk {
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, 1);
                assert_eq!(id, "toolu_9");
                assert_eq!(name.as_deref(), Some("get_weather"));
                assert_eq!(arguments, None);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        let chunk = anthropic
            .parse_stream_frame(
                "{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\"}}",
            )
            .expect("input_json_delta 应解析");
        match chunk {
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, 1);
                assert_eq!(id, "", "续片段无 id");
                assert_eq!(name, None);
                assert_eq!(arguments.as_deref(), Some("{\"city\":"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // 文本块生命周期事件仍不映射 IR
        assert!(anthropic
            .parse_stream_frame(
                "{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}"
            )
            .is_none());
    }

    /// 出站：ToolCall 首片段 → content_block_start(tool_use)；续片段 →
    /// content_block_delta(input_json_delta)。
    #[tokio::test]
    async fn test_anthropic_stream_tool_call_chunk_to_external() {
        let anthropic = AnthropicConverter::new();
        let out = anthropic
            .stream_chunk_to_external(StreamChunk::ToolCall {
                index: 2,
                id: "toolu_9".into(),
                name: Some("get_weather".into()),
                arguments: None,
            })
            .expect("tool_use start");
        assert_eq!(out["type"], "content_block_start");
        assert_eq!(out["index"], 2);
        assert_eq!(out["content_block"]["type"], "tool_use");
        assert_eq!(out["content_block"]["id"], "toolu_9");
        assert_eq!(out["content_block"]["name"], "get_weather");

        let out = anthropic
            .stream_chunk_to_external(StreamChunk::ToolCall {
                index: 2,
                id: String::new(),
                name: None,
                arguments: Some(":5}".into()),
            })
            .expect("input_json_delta");
        assert_eq!(out["type"], "content_block_delta");
        assert_eq!(out["index"], 2);
        assert_eq!(out["delta"]["type"], "input_json_delta");
        assert_eq!(out["delta"]["partial_json"], ":5}");
    }

    // ---- Anthropic：ImageSource::Url ----

    /// 入站：source type url → Url 形态；出站 Url → {"type":"url","url"}。
    #[tokio::test]
    async fn test_anthropic_image_url_source_roundtrip() {
        let anthropic = AnthropicConverter::new();
        let unified = anthropic
            .to_unified(json!({
                "model": "claude",
                "messages": [{"role": "user", "content": [
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/cat.png"}},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGk="}}
                ]}]
            }))
            .expect("to_unified");
        match &unified.messages[0].content {
            MessageContent::MultiModal(blocks) => {
                assert!(
                    matches!(&blocks[0], ContentBlock::Image { source: ImageSource::Url { url }, .. } if url == "https://example.com/cat.png"),
                );
                assert!(matches!(
                    &blocks[1],
                    ContentBlock::Image {
                        source: ImageSource::Base64 { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected MultiModal, got {other:?}"),
        }

        let mut request = minimal_unified_request("claude", "");
        request.messages[0].content = MessageContent::MultiModal(vec![ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/cat.png".into(),
            },
            detail: None,
        }]);
        let out = anthropic.request_to_external(request).expect("to external");
        assert_eq!(
            out["messages"][0]["content"][0]["source"],
            json!({"type": "url", "url": "https://example.com/cat.png"})
        );
    }

    // ==================== T004：流式工具链 golden（表示统一） ====================

    use std::collections::BTreeMap;

    /// 聚合后的完整工具调用（golden 断言用）。
    #[derive(Default, Debug, Clone, PartialEq)]
    struct AggregatedCall {
        id: String,
        name: Option<String>,
        arguments: String,
    }

    /// T004 golden 聚合器：显式状态跟踪 index → {id, name, arguments 缓冲}
    ///（审查 F3 收口——不依赖 name 存在性判定），双侧入站与交叉往返共用。
    struct ToolCallAggregator {
        slots: BTreeMap<u32, AggregatedCall>,
    }

    impl ToolCallAggregator {
        fn new() -> Self {
            Self {
                slots: BTreeMap::new(),
            }
        }

        fn absorb(&mut self, chunk: &StreamChunk) {
            if let StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } = chunk
            {
                let slot = self.slots.entry(*index).or_default();
                if !id.is_empty() {
                    slot.id = id.clone();
                }
                if name.is_some() {
                    slot.name = name.clone();
                }
                if let Some(shard) = arguments {
                    slot.arguments.push_str(shard);
                }
            }
        }

        fn call(&self, index: u32) -> &AggregatedCall {
            &self.slots[&index]
        }
    }

    /// golden：openai 入站并行工具——多 id/index 交错片段流经显式聚合
    /// 状态还原完整调用（arguments 分片拼接），终止帧 finish_reason 不失。
    #[tokio::test]
    async fn test_openai_stream_parallel_tools_golden() {
        let openai = OpenAIConverter::new();
        let frames = [
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"get_time"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"tz\":"}}]}}]}"#,
            // 交错：tool 0 的参数分片在 tool 1 之后到达
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Beijing\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"Asia/CST\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];

        let mut aggregator = ToolCallAggregator::new();
        let mut finish_reason = None;
        for frame in frames {
            let chunk = openai
                .parse_stream_frame(frame)
                .unwrap_or_else(|| panic!("帧应解析: {frame}"));
            match &chunk {
                StreamChunk::Content {
                    finish_reason: fr, ..
                } => {
                    finish_reason = fr.clone();
                }
                _ => aggregator.absorb(&chunk),
            }
        }

        assert_eq!(
            finish_reason.as_deref(),
            Some("tool_calls"),
            "终止帧 finish_reason 不得丢失"
        );
        assert_eq!(
            aggregator.call(0),
            &AggregatedCall {
                id: "call_a".into(),
                name: Some("get_weather".into()),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
            "tool 0 分片拼接应还原完整调用"
        );
        assert_eq!(
            aggregator.call(1),
            &AggregatedCall {
                id: "call_b".into(),
                name: Some("get_time".into()),
                arguments: r#"{"tz":"Asia/CST"}"#.into(),
            },
            "tool 1 交错片段应还原完整调用"
        );
    }

    /// golden：anthropic 入站并行工具——相邻顺序块（tool_use 块无法并发
    /// open）经显式聚合状态还原完整调用；content_block_stop 为生命周期
    /// 事件不映射 IR，message_delta 尾帧 stop_reason 进 Usage.finish_reason。
    #[tokio::test]
    async fn test_anthropic_stream_parallel_tools_golden() {
        let anthropic = AnthropicConverter::new();
        let events = [
            r#"{"type":"message_start","message":{}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"checking"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_a","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"Beijing\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_b","name":"get_time","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"tz\":\"CST\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            r#"{"type":"message_stop"}"#,
        ];

        let mut aggregator = ToolCallAggregator::new();
        let mut stream_stop_reason = None;
        for event in events {
            match anthropic.parse_stream_frame(event) {
                Some(chunk) => match &chunk {
                    StreamChunk::Usage { finish_reason, .. } => {
                        stream_stop_reason = finish_reason.clone();
                    }
                    _ => aggregator.absorb(&chunk),
                },
                None => continue, // 生命周期事件（含 content_block_stop 收口）
            }
        }

        assert!(
            anthropic
                .parse_stream_frame(r#"{"type":"content_block_stop","index":1}"#)
                .is_none(),
            "content_block_stop 为生命周期事件不映射 IR"
        );
        assert_eq!(
            stream_stop_reason.as_deref(),
            Some("tool_calls"),
            "message_delta stop_reason 应映射进 Usage.finish_reason"
        );
        // 文本块与两个工具块并存
        assert_eq!(
            aggregator.call(1),
            &AggregatedCall {
                id: "toolu_a".into(),
                name: Some("get_weather".into()),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
        );
        assert_eq!(
            aggregator.call(2),
            &AggregatedCall {
                id: "toolu_b".into(),
                name: Some("get_time".into()),
                arguments: r#"{"tz":"CST"}"#.into(),
            },
        );
    }

    /// golden 交叉：openai SSE → IR 片段流 → AnthropicStreamEncoder →
    /// 合法事件序列（message_start 首位、工具块 start/delta*/stop 顺序块、
    /// message_delta 携带 stop_reason、message_stop 收尾），事件回读经
    /// anthropic 解析聚合后还原原始调用。
    #[tokio::test]
    async fn test_openai_to_anthropic_tool_stream_golden() {
        let openai = OpenAIConverter::new();
        let anthropic = AnthropicConverter::new();
        let frames = [
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"get_time"}}]}}]}"#,
            // 交错：tool 1 参数 → tool 0 参数（编码器按槽位序串行化）
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"tz\":"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":\"Beijing\"}"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"u","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":9,"total_tokens":14}}"#,
        ];

        let mut encoder = AnthropicStreamEncoder::new();
        let mut all_events: Vec<String> = Vec::new();
        for frame in frames {
            let chunk = openai
                .parse_stream_frame(frame)
                .unwrap_or_else(|| panic!("帧应解析: {frame}"));
            all_events.extend(encoder.encode_chunk(chunk));
        }
        all_events.extend(encoder.finish());

        let parsed: Vec<serde_json::Value> = all_events
            .iter()
            .map(|e| serde_json::from_str(e).expect("事件必须是合法 JSON"))
            .collect();
        let types: Vec<&str> = parsed
            .iter()
            .map(|v| v["type"].as_str().expect("事件必须带 type"))
            .collect();

        // 事件序合法性：message_start 首位、message_stop 收尾、无交错 open 块
        assert_eq!(types[0], "message_start", "types={types:?}");
        assert_eq!(*types.last().unwrap(), "message_stop", "types={types:?}");
        assert_eq!(
            types,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ],
            "交错片段必须按槽位序串行化为合法顺序块: {types:?}"
        );
        // 工具块序号由编码器内部计数器分配（0/1 顺序）
        assert_eq!(parsed[1]["index"], 0);
        assert_eq!(parsed[4]["index"], 1);
        assert_eq!(parsed[1]["content_block"]["type"], "tool_use");
        assert_eq!(parsed[1]["content_block"]["id"], "call_a");
        assert_eq!(parsed[4]["content_block"]["id"], "call_b");
        // 分片逐片还原（刷写按 IR 工具槽位序：slot 0 先于 slot 1）
        assert_eq!(parsed[2]["delta"]["partial_json"], r#"{"city":"Beijing"}"#);
        assert_eq!(parsed[5]["delta"]["partial_json"], r#"{"tz":"#);
        // stop_reason 流式透传闭环：finish_reason("tool_calls") → message_delta
        assert_eq!(parsed[7]["delta"]["stop_reason"], "tool_use");
        assert_eq!(parsed[7]["usage"]["output_tokens"], 9);

        // 事件回读：anthropic 解析聚合 → 原调用还原
        let mut aggregator = ToolCallAggregator::new();
        for event in &all_events {
            if let Some(chunk) = anthropic.parse_stream_frame(event) {
                aggregator.absorb(&chunk);
            }
        }
        assert_eq!(
            aggregator.call(0),
            &AggregatedCall {
                id: "call_a".into(),
                name: Some("get_weather".into()),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
        );
        assert_eq!(
            aggregator.call(1),
            &AggregatedCall {
                id: "call_b".into(),
                name: Some("get_time".into()),
                arguments: r#"{"tz":"#.into(),
            },
        );
    }

    /// golden 交叉：anthropic SSE → IR 片段流 → openai chunk → 回读解析
    /// 聚合，还原原始调用（分片语义与 index 经 openai delta.tool_calls 保留）。
    #[tokio::test]
    async fn test_anthropic_to_openai_tool_stream_golden() {
        let anthropic = AnthropicConverter::new();
        let openai = OpenAIConverter::new();
        let events = [
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_a","name":"get_weather","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"Beijing\"}"}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_b","name":"get_time","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"tz\":\"CST\"}"}}"#,
        ];

        let mut aggregator = ToolCallAggregator::new();
        for event in events {
            let chunk = anthropic
                .parse_stream_frame(event)
                .unwrap_or_else(|| panic!("事件应解析: {event}"));
            let wire = openai
                .stream_chunk_to_external(chunk)
                .expect("IR 片段应编码为 openai chunk");
            // 回读：openai chunk data 负载 → IR → 聚合
            let chunk = openai
                .parse_stream_frame(&wire.to_string())
                .unwrap_or_else(|| panic!("chunk 应回读: {wire}"));
            aggregator.absorb(&chunk);
        }

        assert_eq!(
            aggregator.call(1),
            &AggregatedCall {
                id: "toolu_a".into(),
                name: Some("get_weather".into()),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
            "分片语义与 index 经 openai delta.tool_calls 往返保留"
        );
        assert_eq!(
            aggregator.call(2),
            &AggregatedCall {
                id: "toolu_b".into(),
                name: Some("get_time".into()),
                arguments: r#"{"tz":"CST"}"#.into(),
            },
        );
    }

    /// stop_reason 流式透传专项：anthropic message_delta → Usage.finish_reason
    /// → 编码器回注 message_delta.delta.stop_reason（闭环）；openai 侧
    /// finish_reason 帧 → Content → chunk 透传。
    #[tokio::test]
    async fn test_stop_reason_stream_passthrough_roundtrip() {
        let anthropic = AnthropicConverter::new();

        // 上游：message_delta 尾帧 stop_reason
        let usage_chunk = anthropic
            .parse_stream_frame(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            )
            .expect("message_delta 应解析");
        match &usage_chunk {
            StreamChunk::Usage { finish_reason, .. } => {
                assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            }
            other => panic!("expected Usage, got {other:?}"),
        }

        // 编码器回注：Usage.finish_reason → message_delta.delta.stop_reason
        let mut encoder = AnthropicStreamEncoder::new();
        let events = encoder.encode_chunk(usage_chunk);
        let message_delta = events
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(e).unwrap())
            .find(|v| v["type"] == "message_delta")
            .expect("Usage 应产出 message_delta");
        assert_eq!(
            message_delta["delta"]["stop_reason"], "tool_use",
            "stop_reason 必须经编码器回注到 message_delta"
        );

        // Content 携带 finish_reason 的回注路径
        let mut encoder = AnthropicStreamEncoder::new();
        let mut events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("done".into()),
            finish_reason: Some("max_tokens".into()),
        });
        events.extend(encoder.finish());
        let stop_reason = events
            .iter()
            .filter_map(|e| serde_json::from_str::<serde_json::Value>(e).ok())
            .find_map(|v| (v["type"] == "content_block_stop").then_some(v["index"].clone()));
        assert!(
            stop_reason.is_some(),
            "finish_reason 应闭合文本块: {events:?}"
        );

        // openai 侧：finish_reason 帧 → Content → chunk 透传
        let openai = OpenAIConverter::new();
        let chunk = openai
            .parse_stream_frame(
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            )
            .expect("finish_reason 帧应解析");
        match &chunk {
            StreamChunk::Content { finish_reason, .. } => {
                assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            }
            other => panic!("expected Content, got {other:?}"),
        }
        let wire = openai.stream_chunk_to_external(chunk).expect("编码");
        assert_eq!(wire["choices"][0]["finish_reason"], "tool_calls");
    }

    /// 非规范形状无损回退：单帧多片段 → Content{ToolCalls} 旧聚合表示
    ///（trait 单 chunk 交付约束下的无损路径）；片段与 finish_reason 同帧
    /// 同样回退（ToolCall 片段无终止槽）。
    #[tokio::test]
    async fn test_openai_stream_multi_fragment_frame_falls_back() {
        let openai = OpenAIConverter::new();

        // 单帧多片段：两个并行工具首片段合并
        let chunk = openai
            .parse_stream_frame(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"call_a","type":"function","function":{"name":"get_weather"}},
                    {"index":1,"id":"call_b","type":"function","function":{"name":"get_time"}}
                ]}}]}"#,
            )
            .expect("多片段帧应解析");
        match chunk {
            StreamChunk::Content {
                delta,
                finish_reason,
                ..
            } => {
                match delta {
                    MessageContent::ToolCalls(calls) => {
                        assert_eq!(calls.len(), 2, "多片段必须无损保留");
                        assert_eq!(calls[0].id, "call_a");
                        assert_eq!(calls[1].id, "call_b");
                    }
                    other => panic!("expected ToolCalls fallback, got {other:?}"),
                }
                assert_eq!(finish_reason, None);
            }
            other => panic!("expected Content chunk, got {other:?}"),
        }

        // 片段与 finish_reason 同帧：回退并保留终止原因
        let chunk = openai
            .parse_stream_frame(
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":"{}"}}
                ]},"finish_reason":"tool_calls"}]}"#,
            )
            .expect("同帧应解析");
        match chunk {
            StreamChunk::Content {
                delta,
                finish_reason,
                ..
            } => {
                assert!(matches!(
                    delta,
                    MessageContent::ToolCalls(calls) if calls.len() == 1
                ));
                assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            }
            other => panic!("expected Content chunk, got {other:?}"),
        }
    }

    /// MEDIUM 修复：非规范回退形状 Content{ToolCalls}（openai 多片段帧
    /// 回退产物）经 anthropic 出站合成 tool_use 块还原调用，不再静默丢弃；
    /// 事件序保持合法（工具块 + message_delta stop_reason + message_stop），
    /// 且回读聚合跨表示还原原调用。
    #[tokio::test]
    async fn test_anthropic_outbound_fallback_toolcalls_synthesized() {
        let mut encoder = AnthropicStreamEncoder::new();
        let mut events = encoder.encode_chunk(StreamChunk::Content {
            index: 0,
            delta: MessageContent::ToolCalls(vec![
                ToolCall {
                    id: "call_a".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "get_weather".into(),
                        arguments: r#"{"city":"Beijing"}"#.into(),
                    },
                },
                ToolCall {
                    id: "call_b".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "get_time".into(),
                        arguments: r#"{"tz":"CST"}"#.into(),
                    },
                },
            ]),
            finish_reason: Some("tool_calls".into()),
        });
        events.extend(encoder.encode_chunk(StreamChunk::Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: Some("tool_calls".into()),
        }));
        events.extend(encoder.finish());

        let parsed: Vec<serde_json::Value> = events
            .iter()
            .map(|e| serde_json::from_str(e).expect("事件必须是合法 JSON"))
            .collect();
        let types: Vec<&str> = parsed
            .iter()
            .map(|v| v["type"].as_str().expect("事件必须带 type"))
            .collect();
        assert_eq!(
            types,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ],
            "合成片段必须走既有刷写路径产出合法顺序块: {types:?}"
        );
        assert_eq!(parsed[1]["content_block"]["type"], "tool_use");
        assert_eq!(parsed[1]["content_block"]["id"], "call_a");
        assert_eq!(parsed[1]["content_block"]["name"], "get_weather");
        assert_eq!(parsed[2]["delta"]["partial_json"], r#"{"city":"Beijing"}"#);
        assert_eq!(parsed[4]["content_block"]["id"], "call_b");
        assert_eq!(parsed[5]["delta"]["partial_json"], r#"{"tz":"CST"}"#);
        assert_eq!(parsed[7]["delta"]["stop_reason"], "tool_use");

        // 回读：合成事件经 anthropic 解析聚合 → 跨表示还原原调用
        let anthropic = AnthropicConverter::new();
        let mut aggregator = ToolCallAggregator::new();
        for event in &events {
            if let Some(chunk) = anthropic.parse_stream_frame(event) {
                aggregator.absorb(&chunk);
            }
        }
        assert_eq!(
            aggregator.call(0),
            &AggregatedCall {
                id: "call_a".into(),
                name: Some("get_weather".into()),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
        );
        assert_eq!(
            aggregator.call(1),
            &AggregatedCall {
                id: "call_b".into(),
                name: Some("get_time".into()),
                arguments: r#"{"tz":"CST"}"#.into(),
            },
        );
    }

    /// LOW-4：id 与 name 均空的退化槽（畸形输入）刷写时跳过，不产出
    /// tool_use 块（tracing::warn 显性化），事件序保持合法；Some("") 与
    /// 空串同判。
    #[tokio::test]
    async fn test_anthropic_encoder_degenerate_tool_slot_skipped() {
        let mut encoder = AnthropicStreamEncoder::new();
        encoder.encode_chunk(StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("{}".into()),
        });
        let events = encoder.encode_chunk(StreamChunk::Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: Some("tool_calls".into()),
        });
        let types: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let v: serde_json::Value = serde_json::from_str(e).ok()?;
                v["type"].as_str().map(|s| s.to_string())
            })
            .collect();
        assert_eq!(
            types,
            vec!["message_start".to_string(), "message_delta".to_string()],
            "退化槽不得产出 tool_use 块: {types:?}"
        );

        // Some("") 与空串同判：不产出、不 panic
        let mut encoder = AnthropicStreamEncoder::new();
        encoder.encode_chunk(StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: Some(String::new()),
            arguments: None,
        });
        let events = encoder.finish();
        let types: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let v: serde_json::Value = serde_json::from_str(e).ok()?;
                v["type"].as_str().map(|s| s.to_string())
            })
            .collect();
        assert_eq!(
            types,
            vec!["message_start".to_string(), "message_stop".to_string()]
        );
    }
}

mod embedding {
    // recId=15③：embedding_response_from_openai 死代码已删除（零生产调用，
    // embeddings 端点走治理透传），其两个用例随之退役；
    // embedding_request_to_openai 仍被 api/openai.rs 使用，用例保留。
    use llm_gate::protocol::embedding_request_to_openai;
    use serde_json::json;

    #[test]
    fn test_embedding_request_to_openai_string_input() {
        let body = json!({
            "model": "text-embedding-ada-002",
            "input": "Hello world"
        });

        let result = embedding_request_to_openai(body);
        assert!(result.is_ok());
        let openai = result.unwrap();

        assert_eq!(openai["model"], "text-embedding-ada-002");
        assert_eq!(openai["input"], json!(["Hello world"]));
    }

    #[test]
    fn test_embedding_request_to_openai_array_input() {
        let body = json!({
            "model": "text-embedding-3-small",
            "input": ["hello", "world", "test"]
        });

        let result = embedding_request_to_openai(body);
        assert!(result.is_ok());
        let openai = result.unwrap();

        assert_eq!(openai["model"], "text-embedding-3-small");
        assert_eq!(openai["input"], json!(["hello", "world", "test"]));
    }

    #[test]
    fn test_embedding_request_to_openai_with_options() {
        let body = json!({
            "model": "text-embedding-3-large",
            "input": "Test input",
            "encoding_format": "float",
            "dimensions": 1024,
            "user": "user-123"
        });

        let result = embedding_request_to_openai(body);
        assert!(result.is_ok());
        let openai = result.unwrap();

        assert_eq!(openai["encoding_format"], "float");
        assert_eq!(openai["dimensions"], 1024);
        assert_eq!(openai["user"], "user-123");
    }

    #[test]
    fn test_embedding_request_to_openai_missing_model() {
        let body = json!({
            "input": "Test input"
        });

        let result = embedding_request_to_openai(body);
        assert!(result.is_err());
    }
}

mod unified {
    use llm_gate::protocol::*;
    use serde_json::json;

    // ==================== Role Tests ====================

    #[test]
    fn test_role_serialize() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn test_role_deserialize() {
        assert_eq!(
            serde_json::from_str::<Role>("\"system\"").unwrap(),
            Role::System
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"user\"").unwrap(),
            Role::User
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"assistant\"").unwrap(),
            Role::Assistant
        );
        assert_eq!(
            serde_json::from_str::<Role>("\"tool\"").unwrap(),
            Role::Tool
        );
    }

    #[test]
    fn test_role_equality() {
        assert_eq!(Role::System, Role::System);
        assert_ne!(Role::System, Role::User);
        assert_ne!(Role::Assistant, Role::Tool);
    }

    // ==================== MessageContent Tests ====================

    #[test]
    fn test_message_content_text_default() {
        let default = MessageContent::default();
        match default {
            MessageContent::Text(s) => assert!(s.is_empty()),
            _ => panic!("Default should be Text(String::new())"),
        }
    }

    #[test]
    fn test_message_content_text_roundtrip() {
        let content = MessageContent::Text("hello world".to_string());
        let json_str = serde_json::to_string(&content).unwrap();
        assert_eq!(json_str, "\"hello world\"");
        let deserialized: MessageContent = serde_json::from_str(&json_str).unwrap();
        match deserialized {
            MessageContent::Text(s) => assert_eq!(s, "hello world"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn test_message_content_tool_calls_serialize() {
        let content = MessageContent::ToolCalls(vec![ToolCall {
            id: "call_1".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "search".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let json = serde_json::to_value(&content).unwrap();
        // untagged: serializes as bare array
        assert!(json.is_array());
        assert_eq!(json[0]["id"], "call_1");
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "search");
    }

    #[test]
    fn test_message_content_tool_result_roundtrip() {
        let result = ToolResult {
            tool_call_id: "call_123".to_string(),
            function_name: "get_weather".to_string(),
            content: "Sunny, 25C".to_string(),
            is_error: false,
        };
        let content = MessageContent::ToolResult(result);
        let json_str = serde_json::to_string(&content).unwrap();
        let deserialized: MessageContent = serde_json::from_str(&json_str).unwrap();
        match deserialized {
            MessageContent::ToolResult(r) => {
                assert_eq!(r.tool_call_id, "call_123");
                assert_eq!(r.function_name, "get_weather");
                assert_eq!(r.content, "Sunny, 25C");
                assert!(!r.is_error);
            }
            _ => panic!("Expected ToolResult variant"),
        }
    }

    #[test]
    fn test_message_content_text_with_tool_calls_serialize() {
        let content = MessageContent::TextWithToolCalls {
            text: "Let me search".to_string(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: "search".to_string(),
                    arguments: "{}".to_string(),
                },
            }],
        };
        let json = serde_json::to_value(&content).unwrap();
        // camelCase: toolCalls (not tool_calls)
        assert_eq!(json["text"], "Let me search");
        assert!(json.get("toolCalls").is_some());
        assert!(json.get("tool_calls").is_none());
        assert_eq!(json["toolCalls"][0]["id"], "call_1");
    }

    // ==================== Message Tests ====================

    #[test]
    fn test_message_basic_serialize() {
        let msg = Message {
            role: Role::User,
            content: MessageContent::Text("Hello".to_string()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn test_message_roundtrip() {
        let msg = Message {
            role: Role::Assistant,
            content: MessageContent::Text("Hi there".to_string()),
        };
        let json_str = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.role, Role::Assistant);
        match deserialized.content {
            MessageContent::Text(s) => assert_eq!(s, "Hi there"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn test_message_with_tool_role() {
        let msg = Message {
            role: Role::Tool,
            content: MessageContent::ToolResult(ToolResult {
                tool_call_id: "call_123".to_string(),
                function_name: "get_weather".to_string(),
                content: "Sunny, 25C".to_string(),
                is_error: false,
            }),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"]["tool_call_id"], "call_123");
        assert_eq!(json["content"]["content"], "Sunny, 25C");
    }

    // ==================== UnifiedRequest Tests ====================

    #[test]
    fn test_unified_request_minimal() {
        let json = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let req: UnifiedRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.messages.len(), 1);
        assert!(req.temperature.is_none());
        assert!(req.max_tokens.is_none());
        assert!(!req.stream);
        assert!(req.stop.is_empty());
        assert!(req.tools.is_empty());
        assert!(req.tool_choice.is_none());
        // T003 新字段 serde default
        assert!(req.system_prompt.is_none());
        assert!(req.sampling.is_none());
        assert!(req.thinking_config.is_none());
        assert!(req.effort.is_none());
        assert!(req.turn_index.is_none());
        assert!(req.vendor_extensions.is_empty());
    }

    #[test]
    fn test_unified_request_full() {
        let json = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "temperature": 0.7,
            "max_tokens": 100,
            "stream": true,
            "stop": ["END", "STOP"],
            "top_p": 0.9,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.3,
            "logit_bias": {"123": 1.0},
            "user": "user-123",
            "tools": [],
            "tool_choice": "auto"
        });
        let req: UnifiedRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(100));
        assert!(req.stream);
        assert_eq!(req.stop.len(), 2);
        assert_eq!(req.stop[0], "END");
        assert_eq!(req.stop[1], "STOP");
        assert_eq!(req.top_p, Some(0.9));
        assert_eq!(req.frequency_penalty, Some(0.5));
        assert_eq!(req.presence_penalty, Some(0.3));
        assert_eq!(req.user, Some("user-123".to_string()));
        assert!(req.logit_bias.contains_key("123"));
    }

    #[test]
    fn test_unified_request_skip_serializing_if_none() {
        let req = UnifiedRequest {
            model: "gpt-4".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: MessageContent::Text("Hi".to_string()),
            }],
            ..Default::default()
        };
        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();
        // None Option fields should be skipped
        assert!(!obj.contains_key("temperature"));
        assert!(!obj.contains_key("max_tokens"));
        assert!(!obj.contains_key("top_p"));
        assert!(!obj.contains_key("system_prompt"));
        assert!(!obj.contains_key("sampling"));
        assert!(!obj.contains_key("thinking_config"));
        assert!(!obj.contains_key("effort"));
        assert!(!obj.contains_key("turn_index"));
        assert!(!obj.contains_key("vendor_extensions"));
        assert!(!obj.contains_key("frequency_penalty"));
        assert!(!obj.contains_key("presence_penalty"));
        assert!(!obj.contains_key("user"));
        assert!(!obj.contains_key("tool_choice"));
        // Empty collections should be skipped
        assert!(!obj.contains_key("stop"));
        assert!(!obj.contains_key("logit_bias"));
        assert!(!obj.contains_key("tools"));
        // Required + non-skipped fields should be present
        assert!(obj.contains_key("model"));
        assert!(obj.contains_key("messages"));
        assert!(obj.contains_key("stream"));
    }

    #[test]
    fn test_unified_request_default_fields() {
        let json = json!({
            "model": "gpt-4",
            "messages": []
        });
        let req: UnifiedRequest = serde_json::from_value(json).unwrap();
        assert!(!req.stream);
        assert!(req.stop.is_empty());
        assert!(req.logit_bias.is_empty());
        assert!(req.tools.is_empty());
        assert!(req.tool_choice.is_none());
        assert!(req.temperature.is_none());
        assert!(req.user.is_none());
    }

    // ==================== UnifiedResponse Tests ====================

    #[test]
    fn test_unified_response_basic() {
        let json = json!({
            "id": "chatcmpl-123",
            "created": 1677858242,
            "model": "gpt-4",
            "choices": [],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        });
        let resp: UnifiedResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.id, "chatcmpl-123");
        assert_eq!(resp.created, 1677858242);
        assert_eq!(resp.model, "gpt-4");
        assert!(resp.choices.is_empty());
        assert_eq!(resp.usage.prompt_tokens, 5);
        assert_eq!(resp.object, ""); // default empty string
    }

    #[test]
    fn test_unified_response_roundtrip() {
        let resp = UnifiedResponse {
            id: "resp-1".to_string(),
            object: "chat.completion".to_string(),
            created: 1000,
            model: "gpt-4".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content: MessageContent::Text("Hello".to_string()),
                }),
                finish_reason: Some("stop".to_string()),
                finish: Some(FinishReason::Stop),
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
        };
        let json_str = serde_json::to_string(&resp).unwrap();
        let deserialized: UnifiedResponse = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.id, "resp-1");
        assert_eq!(deserialized.object, "chat.completion");
        assert_eq!(deserialized.model, "gpt-4");
        assert_eq!(deserialized.choices.len(), 1);
        assert_eq!(deserialized.usage.total_tokens, 15);
        assert_eq!(deserialized.choices[0].finish, Some(FinishReason::Stop));
    }

    #[test]
    fn test_unified_response_default_object() {
        let json = json!({
            "id": "r1",
            "created": 0,
            "model": "m",
            "choices": [],
            "usage": {}
        });
        let resp: UnifiedResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.object, "");
        assert_eq!(resp.usage.prompt_tokens, 0);
        assert_eq!(resp.usage.completion_tokens, 0);
        assert_eq!(resp.usage.total_tokens, 0);
    }

    // ==================== TokenUsage Tests ====================

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_token_usage_serialize() {
        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_read_tokens: 8,
            cache_creation_tokens: 3,
            ..Default::default()
        };
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["prompt_tokens"], 100);
        assert_eq!(json["completion_tokens"], 50);
        assert_eq!(json["total_tokens"], 150);
        assert_eq!(json["cache_read_tokens"], 8);
        assert_eq!(json["cache_creation_tokens"], 3);
    }

    // ==================== Tool / ToolFunction / ToolParameters Tests ====================

    #[test]
    fn test_tool_default() {
        let tool = Tool::default();
        assert_eq!(tool.tool_type, "function");
        assert_eq!(tool.function.name, "");
        // A3 保真改造：parameters 为原样 Value，未携带即 Null
        assert!(tool.function.parameters.is_null());
    }

    #[test]
    fn test_tool_function_default() {
        let func = ToolFunction::default();
        assert_eq!(func.name, "");
        assert_eq!(func.description, "");
        assert!(func.parameters.is_null());
    }

    #[test]
    fn test_tool_full_roundtrip() {
        // A3 保真改造：parameters 原样 Value 往返（嵌套/枚举/required 全保真）
        let schema = json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name",
                    "enum": ["Beijing", "Shanghai"],
                },
            },
            "required": ["city"],
        });
        let tool = Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                parameters: schema.clone(),
            },
        };
        let json_str = serde_json::to_string(&tool).unwrap();
        let deserialized: Tool = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.function.name, "get_weather");
        assert_eq!(deserialized.function.parameters, schema, "原样往返保真");
        // Verify "type" rename (not "tool_type")
        let json_val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json_val["type"], "function");
        assert!(json_val.get("tool_type").is_none());
        assert_eq!(json_val["function"]["name"], "get_weather");
        assert_eq!(json_val["function"]["parameters"]["type"], "object");
    }

    /// A3 保真验收（T008 M1 闭环）：嵌套 schema / anyOf / $ref / 缺 type
    /// 属性——经公共转换面（to_unified 入站解析 → request_to_external 出站
    /// 编码）原样幸存；anthropic 面 input_schema 同样原样 + 缺 schema 补
    /// 协议最小形状。
    #[test]
    fn test_tool_parameters_fidelity_nested_anyof_ref_missing_type() {
        let schema = json!({
            "type": "object",
            "properties": {
                "target": { "anyOf": [{ "$ref": "#/$defs/city" }, { "type": "null" }] },
                "nested": {
                    "type": "object",
                    "properties": { "deep": { "type": "array", "items": { "type": "integer" } } },
                },
                "legacy": { "description": "缺 type 属性（A3 前被静默丢弃）" },
            },
            "$defs": { "city": { "type": "string" } },
            "required": ["target"],
            "additionalProperties": false,
        });
        let wire_tool = json!({
            "type": "function",
            "function": {
                "name": "fidelity",
                "description": "schema 保真",
                "parameters": schema.clone(),
            },
        });

        // openai 面往返
        let converter = OpenAIConverter::new();
        let unified = converter
            .to_unified(json!({
                "model": "gpt-4",
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [wire_tool.clone()],
            }))
            .expect("入站解析");
        assert_eq!(unified.tools.len(), 1);
        assert_eq!(unified.tools[0].function.parameters, schema, "入站原样幸存");

        let encoded = converter.request_to_external(unified).expect("出站编码");
        assert_eq!(
            encoded["tools"][0]["function"]["parameters"], schema,
            "出站无损"
        );

        // anthropic 面往返（input_schema）
        let anthropic = AnthropicConverter::new();
        let a_unified = anthropic
            .to_unified(json!({
                "model": "claude-3",
                "max_tokens": 64,
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{
                    "name": "fidelity",
                    "description": "schema 保真",
                    "input_schema": schema.clone(),
                }],
            }))
            .expect("anthropic 入站解析");
        assert_eq!(a_unified.tools[0].function.parameters, schema);

        let a_encoded = anthropic
            .request_to_external(a_unified)
            .expect("anthropic 出站");
        assert_eq!(
            a_encoded["tools"][0]["input_schema"], schema,
            "anthropic 出站无损"
        );

        // 缺 input_schema → 协议最小形状补齐
        let a_bare = anthropic
            .to_unified(json!({
                "model": "claude-3",
                "max_tokens": 64,
                "messages": [{ "role": "user", "content": "hi" }],
                "tools": [{ "name": "bare", "description": "无 schema" }],
            }))
            .expect("anthropic 无 schema 入站");
        let a_bare_encoded = anthropic.request_to_external(a_bare).expect("出站");
        assert_eq!(
            a_bare_encoded["tools"][0]["input_schema"],
            json!({ "type": "object" }),
            "anthropic 必填 input_schema 缺省补最小形状"
        );
    }

    // ==================== ToolChoice Tests ====================

    #[test]
    fn test_tool_choice_strategy() {
        let choice = ToolChoice::Strategy("auto".to_string());
        let json = serde_json::to_value(&choice).unwrap();
        // untagged: serializes as bare string
        assert_eq!(json, json!("auto"));
        // Roundtrip
        let deserialized: ToolChoice = serde_json::from_str("\"auto\"").unwrap();
        match deserialized {
            ToolChoice::Strategy(s) => assert_eq!(s, "auto"),
            _ => panic!("Expected Strategy variant"),
        }
    }

    #[test]
    fn test_tool_choice_specific() {
        let choice = ToolChoice::Specific {
            choice_type: "function".to_string(),
            function: ToolChoiceFunction {
                name: "get_weather".to_string(),
            },
        };
        let json = serde_json::to_value(&choice).unwrap();
        // "type" rename (not "choice_type")
        assert_eq!(json["type"], "function");
        assert!(json.get("choice_type").is_none());
        assert_eq!(json["function"]["name"], "get_weather");
        // Roundtrip
        let json_str = serde_json::to_string(&choice).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json_str).unwrap();
        match deserialized {
            ToolChoice::Specific {
                choice_type,
                function,
            } => {
                assert_eq!(choice_type, "function");
                assert_eq!(function.name, "get_weather");
            }
            _ => panic!("Expected Specific variant"),
        }
    }

    // ==================== ToolCall / ToolCallFunction / ToolResult Tests ====================

    #[test]
    fn test_tool_call_roundtrip() {
        let call = ToolCall {
            id: "call_abc".to_string(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: "search".to_string(),
                arguments: "{\"q\": \"test\"}".to_string(),
            },
        };
        let json_str = serde_json::to_string(&call).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.id, "call_abc");
        assert_eq!(deserialized.call_type, "function");
        assert_eq!(deserialized.function.name, "search");
        assert_eq!(deserialized.function.arguments, "{\"q\": \"test\"}");
    }

    #[test]
    fn test_tool_call_default_type() {
        // When "type" is missing, should default to "function"
        let json = json!({
            "id": "call_1",
            "function": {"name": "fn", "arguments": "{}"}
        });
        let call: ToolCall = serde_json::from_value(json).unwrap();
        assert_eq!(call.call_type, "function");
        assert_eq!(call.id, "call_1");
    }

    #[test]
    fn test_tool_result_roundtrip() {
        let result = ToolResult {
            tool_call_id: "call_123".to_string(),
            function_name: "get_weather".to_string(),
            content: "Sunny, 25C".to_string(),
            is_error: false,
        };
        let json_str = serde_json::to_string(&result).unwrap();
        let deserialized: ToolResult = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.tool_call_id, "call_123");
        assert_eq!(deserialized.function_name, "get_weather");
        assert_eq!(deserialized.content, "Sunny, 25C");
        assert!(!deserialized.is_error);
    }

    // ==================== StreamChunk Tests ====================

    #[test]
    fn test_stream_chunk_content() {
        let chunk = StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("Hello".to_string()),
            finish_reason: Some("stop".to_string()),
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["type"], "content");
        assert_eq!(json["index"], 0);
        assert_eq!(json["delta"], "Hello"); // untagged MessageContent::Text → bare string
        assert_eq!(json["finish_reason"], "stop");
    }

    #[test]
    fn test_stream_chunk_usage() {
        let chunk = StreamChunk::Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            finish_reason: None,
        };
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["type"], "usage");
        assert_eq!(json["prompt_tokens"], 10);
        assert_eq!(json["completion_tokens"], 5);
        assert_eq!(json["total_tokens"], 15);
        // Roundtrip
        let json_str = serde_json::to_string(&chunk).unwrap();
        let deserialized: StreamChunk = serde_json::from_str(&json_str).unwrap();
        match deserialized {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => {
                assert_eq!(prompt_tokens, 10);
                assert_eq!(completion_tokens, 5);
                assert_eq!(total_tokens, 15);
            }
            _ => panic!("Expected Usage variant"),
        }
    }

    #[test]
    fn test_stream_chunk_heartbeat() {
        let chunk = StreamChunk::Heartbeat;
        let json = serde_json::to_value(&chunk).unwrap();
        assert_eq!(json["type"], "heartbeat");
        assert_eq!(json.as_object().unwrap().len(), 1);
        // Roundtrip
        let json_str = serde_json::to_string(&chunk).unwrap();
        let deserialized: StreamChunk = serde_json::from_str(&json_str).unwrap();
        assert!(matches!(deserialized, StreamChunk::Heartbeat));
    }

    // ==================== Embedding Tests ====================

    #[test]
    fn test_embedding_response_roundtrip() {
        let resp = UnifiedEmbeddingResponse {
            id: "emb-1".to_string(),
            object: "list".to_string(),
            created: 1000,
            model: "text-embedding-ada-002".to_string(),
            data: vec![EmbeddingData {
                index: 0,
                object_: "embedding".to_string(),
                embedding: vec![0.1, 0.2, 0.3],
            }],
            usage: EmbeddingUsage {
                prompt_tokens: 5,
                total_tokens: 5,
            },
        };
        let json_str = serde_json::to_string(&resp).unwrap();
        let deserialized: UnifiedEmbeddingResponse = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.id, "emb-1");
        assert_eq!(deserialized.model, "text-embedding-ada-002");
        assert_eq!(deserialized.data.len(), 1);
        assert_eq!(deserialized.data[0].index, 0);
        assert_eq!(deserialized.data[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn test_embedding_data_object_rename() {
        let data = EmbeddingData {
            index: 0,
            object_: "embedding".to_string(),
            embedding: vec![0.5],
        };
        let json = serde_json::to_value(&data).unwrap();
        // "object_" serializes as "object" (serde rename)
        assert_eq!(json["object"], "embedding");
        assert!(json.get("object_").is_none());
        assert_eq!(json["index"], 0);
        assert_eq!(json["embedding"][0], 0.5);
    }

    // ==================== ImageSource / ToolParameterProperty Tests ====================

    #[test]
    fn test_image_source_serialize() {
        let src = ImageSource::Base64 {
            media_type: "image/png".to_string(),
            data: "iVBOR".to_string(),
        };
        let json = serde_json::to_value(&src).unwrap();
        // tag = "type"（snake_case 变体名），与旧 IR 结构体形状兼容
        assert_eq!(json["type"], "base64");
        assert!(json.get("type_").is_none());
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["data"], "iVBOR");
    }

    // A3 保真改造：ToolParameterProperty 结构体已删除（parameters 原样
    // Value 透传，属性面保真由 schema 本身承载）——序列化测试随之退役。
}
