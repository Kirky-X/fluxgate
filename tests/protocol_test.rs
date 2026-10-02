// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod converter {
    use nexusflow::protocol::{
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
        assert_eq!(tool_msg.role, nexusflow::protocol::Role::Tool);
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
            nexusflow::protocol::MessageContent::ToolCalls(calls) => {
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
            Some(nexusflow::protocol::ToolChoice::Specific { function, .. }) => {
                assert_eq!(function.name, "get_weather");
            }
            _ => panic!("Expected Specific tool_choice"),
        }
    }

    // ==================== 流式转换与 SSE 增量分帧（recId=3） ====================

    use nexusflow::protocol::sse_frame::SseFrameParser;

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
        match chunk {
            StreamChunk::Content { delta, .. } => match delta {
                nexusflow::protocol::MessageContent::ToolCalls(calls) => {
                    assert_eq!(calls.len(), 1);
                    assert_eq!(calls[0].id, "call_1");
                    assert_eq!(calls[0].function.name, "get_weather");
                    assert!(calls[0].function.arguments.contains("\"city\""));
                }
                other => panic!("expected ToolCalls delta, got {other:?}"),
            },
            other => panic!("expected Content chunk, got {other:?}"),
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
                if matches!(delta, nexusflow::protocol::MessageContent::Text(t) if t == "hi")
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
                assert!(matches!(d1, nexusflow::protocol::MessageContent::Text(t) if t == "Hel"));
                assert_eq!(fr2.as_deref(), Some("stop"));
            }
            _ => panic!("expected two Content chunks"),
        }
    }

    // ==================== 协议断点修复（recId=8） ====================

    use nexusflow::protocol::Role;

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
            nexusflow::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
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
            nexusflow::protocol::MessageContent::ToolResult(result) => {
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
            nexusflow::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
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
            nexusflow::protocol::MessageContent::TextWithToolCalls { text, tool_calls } => {
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
            nexusflow::protocol::MessageContent::Text(s) if s == "You are a helpful assistant."
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
        let request = nexusflow::protocol::UnifiedRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            messages: vec![
                nexusflow::protocol::Message {
                    role: Role::System,
                    content: nexusflow::protocol::MessageContent::Text("Be terse.".into()),
                },
                nexusflow::protocol::Message {
                    role: Role::User,
                    content: nexusflow::protocol::MessageContent::Text("Hi".into()),
                },
            ],
            temperature: None,
            max_tokens: Some(10),
            stream: false,
            stop: vec![],
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            logit_bias: Default::default(),
            user: None,
            tools: vec![],
            tool_choice: None,
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
        let make_response = |finish: Option<&str>| nexusflow::protocol::UnifiedResponse {
            id: "m".into(),
            object: "message".into(),
            created: 0,
            model: "claude-x".into(),
            choices: vec![nexusflow::protocol::Choice {
                index: 0,
                message: Some(nexusflow::protocol::Message {
                    role: Role::Assistant,
                    content: nexusflow::protocol::MessageContent::Text("ok".into()),
                }),
                finish_reason: finish.map(|s| s.to_string()),
                logprobs: None,
            }],
            usage: nexusflow::protocol::TokenUsage::default(),
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
}

mod embedding {
    // recId=15③：embedding_response_from_openai 死代码已删除（零生产调用，
    // embeddings 端点走治理透传），其两个用例随之退役；
    // embedding_request_to_openai 仍被 api/openai.rs 使用，用例保留。
    use nexusflow::protocol::embedding_request_to_openai;
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
    use nexusflow::protocol::*;
    use serde_json::json;
    use std::collections::HashMap;

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
            temperature: None,
            max_tokens: None,
            stream: false,
            stop: vec![],
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            logit_bias: HashMap::new(),
            user: None,
            tools: vec![],
            tool_choice: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        let obj = json.as_object().unwrap();
        // None Option fields should be skipped
        assert!(!obj.contains_key("temperature"));
        assert!(!obj.contains_key("max_tokens"));
        assert!(!obj.contains_key("top_p"));
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
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json_str = serde_json::to_string(&resp).unwrap();
        let deserialized: UnifiedResponse = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.id, "resp-1");
        assert_eq!(deserialized.object, "chat.completion");
        assert_eq!(deserialized.model, "gpt-4");
        assert_eq!(deserialized.choices.len(), 1);
        assert_eq!(deserialized.usage.total_tokens, 15);
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
        };
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["prompt_tokens"], 100);
        assert_eq!(json["completion_tokens"], 50);
        assert_eq!(json["total_tokens"], 150);
    }

    // ==================== Tool / ToolFunction / ToolParameters Tests ====================

    #[test]
    fn test_tool_default() {
        let tool = Tool::default();
        assert_eq!(tool.tool_type, "function");
        assert_eq!(tool.function.name, "");
        assert_eq!(tool.function.parameters.param_type, "object");
    }

    #[test]
    fn test_tool_function_default() {
        let func = ToolFunction::default();
        assert_eq!(func.name, "");
        assert_eq!(func.description, "");
        assert_eq!(func.parameters.param_type, "object");
        assert!(func.parameters.properties.is_empty());
        assert!(func.parameters.required.is_empty());
    }

    #[test]
    fn test_tool_parameters_default() {
        let params = ToolParameters::default();
        assert_eq!(params.param_type, "object");
        assert!(params.properties.is_empty());
        assert!(params.required.is_empty());
    }

    #[test]
    fn test_tool_full_roundtrip() {
        let tool = Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "get_weather".to_string(),
                description: "Get weather".to_string(),
                parameters: ToolParameters {
                    param_type: "object".to_string(),
                    properties: {
                        let mut m = HashMap::new();
                        m.insert(
                            "city".to_string(),
                            ToolParameterProperty {
                                param_type: "string".to_string(),
                                description: "City name".to_string(),
                                enum_values: vec![],
                            },
                        );
                        m
                    },
                    required: vec!["city".to_string()],
                },
            },
        };
        let json_str = serde_json::to_string(&tool).unwrap();
        let deserialized: Tool = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.function.name, "get_weather");
        assert_eq!(deserialized.function.parameters.required.len(), 1);
        assert_eq!(deserialized.function.parameters.required[0], "city");
        // Verify "type" rename (not "tool_type")
        let json_val: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(json_val["type"], "function");
        assert!(json_val.get("tool_type").is_none());
        assert_eq!(json_val["function"]["name"], "get_weather");
        assert_eq!(json_val["function"]["parameters"]["type"], "object");
        assert!(json_val["function"]["parameters"]
            .get("param_type")
            .is_none());
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
        let src = ImageSource {
            type_: "base64".to_string(),
            media_type: "image/png".to_string(),
            data: "iVBOR".to_string(),
        };
        let json = serde_json::to_value(&src).unwrap();
        // "type_" serializes as "type" (serde rename)
        assert_eq!(json["type"], "base64");
        assert!(json.get("type_").is_none());
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["data"], "iVBOR");
    }

    #[test]
    fn test_tool_parameter_property_serialize() {
        let prop = ToolParameterProperty {
            param_type: "string".to_string(),
            description: "A city name".to_string(),
            enum_values: vec!["Beijing".to_string(), "Shanghai".to_string()],
        };
        let json = serde_json::to_value(&prop).unwrap();
        // "param_type" serializes as "type" (serde rename)
        assert_eq!(json["type"], "string");
        assert!(json.get("param_type").is_none());
        assert_eq!(json["description"], "A city name");
        assert_eq!(json["enum_values"][0], "Beijing");
        assert_eq!(json["enum_values"][1], "Shanghai");
    }

    #[test]
    fn test_tool_parameter_property_empty_enum_skipped() {
        let prop = ToolParameterProperty {
            param_type: "string".to_string(),
            description: "test".to_string(),
            enum_values: vec![],
        };
        let json = serde_json::to_value(&prop).unwrap();
        // empty enum_values should be skipped via skip_serializing_if
        assert!(json.get("enum_values").is_none());
    }
}
