#[cfg(test)]
mod tests {
    use smart_llm_proxy::protocol::{OpenAIConverter, AnthropicConverter, Protocol, ProtocolConverter};
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
        assert_eq!(tool_msg.role, smart_llm_proxy::protocol::Role::Tool);
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
            smart_llm_proxy::protocol::MessageContent::ToolCalls(calls) => {
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
            Some(smart_llm_proxy::protocol::ToolChoice::Specific { function, .. }) => {
                assert_eq!(function.name, "get_weather");
            }
            _ => panic!("Expected Specific tool_choice"),
        }
    }
}
