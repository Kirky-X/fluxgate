// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod converter {
    use nexusflow::protocol::{AnthropicConverter, OpenAIConverter, Protocol, ProtocolConverter};
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
}

mod embedding {
    use nexusflow::protocol::{embedding_request_to_openai, embedding_response_from_openai};
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

    #[test]
    fn test_embedding_response_from_openai() {
        let response = json!({
            "id": "embedding-abc123",
            "object": "list",
            "created": 1677858242,
            "model": "text-embedding-ada-002",
            "data": [
                {
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.1, 0.2, 0.3, 0.4]
                },
                {
                    "object": "embedding",
                    "index": 1,
                    "embedding": [0.5, 0.6, 0.7, 0.8]
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "total_tokens": 10
            }
        });

        let result = embedding_response_from_openai(response);
        assert!(result.is_ok());
        let embedding = result.unwrap();

        assert_eq!(embedding.id, "embedding-abc123");
        assert_eq!(embedding.model, "text-embedding-ada-002");
        assert_eq!(embedding.data.len(), 2);
        assert_eq!(embedding.data[0].index, 0);
        assert_eq!(embedding.data[0].embedding, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(embedding.data[1].index, 1);
        assert_eq!(embedding.usage.prompt_tokens, 10);
    }

    #[test]
    fn test_embedding_response_from_openai_empty() {
        let response = json!({
            "id": "embedding-def456",
            "object": "list",
            "created": 1677858242,
            "model": "text-embedding-3-small",
            "data": [],
            "usage": {
                "prompt_tokens": 0,
                "total_tokens": 0
            }
        });

        let result = embedding_response_from_openai(response);
        assert!(result.is_ok());
        let embedding = result.unwrap();

        assert!(embedding.data.is_empty());
        assert_eq!(embedding.usage.total_tokens, 0);
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
        assert_eq!(serde_json::from_str::<Role>("\"tool\"").unwrap(), Role::Tool);
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
        assert!(
            json_val["function"]["parameters"]
                .get("param_type")
                .is_none()
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
