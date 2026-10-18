// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! IR 响应 → Responses 响应对象（纯函数）。
//!
//! 只消费第一个 choice（Responses 单输出语义）；`finish_reason` 映射
//! `status`/`incomplete_details`，usage 映射到 Responses 词汇。

use crate::protocol::responses::types::{
    IncompleteDetails, InputTokensDetails, OutputContent, OutputItem, OutputTokensDetails,
    ResponsesRequest, ResponsesResponse, ResponsesUsage,
};
use crate::protocol::unified::{FinishReason, MessageContent, UnifiedResponse};
use uuid::Uuid;

/// 构造 Responses 响应对象。
///
/// `id` 由调用方生成（IR 路径 `resp_<uuid>`；原生直通沿用上游 id）。
/// `store` 为生效值（`[responses].store_enabled=false` 时已降级）。
pub fn to_responses_response(
    id: &str,
    response: &UnifiedResponse,
    request: &ResponsesRequest,
    store: bool,
) -> ResponsesResponse {
    let choice = response.choices.first();
    let mut output: Vec<OutputItem> = Vec::new();
    if let Some(choice) = choice {
        if let Some(message) = choice.message.as_ref() {
            append_message_output(&mut output, &message.content);
        }
    }

    let finish = choice.and_then(|choice| choice.finish).or_else(|| {
        choice
            .and_then(|choice| choice.finish_reason.as_deref())
            .and_then(FinishReason::from_wire)
    });
    let (status, incomplete_details) = match finish {
        Some(FinishReason::Length) => (
            "incomplete",
            Some(IncompleteDetails {
                reason: "max_output_tokens".to_string(),
            }),
        ),
        Some(FinishReason::ContentFilter) => (
            "incomplete",
            Some(IncompleteDetails {
                reason: "content_filter".to_string(),
            }),
        ),
        _ => ("completed", None),
    };

    ResponsesResponse {
        id: id.to_string(),
        object: "response".to_string(),
        created_at: response.created,
        status: status.to_string(),
        error: None,
        incomplete_details,
        instructions: request.instructions.clone(),
        max_output_tokens: request.max_output_tokens,
        model: response.model.clone(),
        output,
        parallel_tool_calls: request.parallel_tool_calls,
        previous_response_id: request.previous_response_id.clone(),
        reasoning: None,
        store,
        temperature: request.temperature,
        text: request
            .text
            .as_ref()
            .and_then(|text| serde_json::to_value(text).ok()),
        tool_choice: request.tool_choice.clone(),
        tools: request.tools.clone(),
        top_p: request.top_p,
        truncation: request.truncation.clone(),
        usage: map_usage(&response.usage),
        user: request.user.clone(),
        metadata: request.metadata.clone().unwrap_or_default(),
        extra: serde_json::Map::new(),
    }
}

/// assistant 内容 → output items（文本 message + 逐个 function_call）。
fn append_message_output(output: &mut Vec<OutputItem>, content: &MessageContent) {
    match content {
        MessageContent::Text(text) => push_text_message(output, text),
        MessageContent::MultiModal(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    crate::protocol::unified::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            push_text_message(output, &text);
        }
        MessageContent::ToolCalls(calls) => {
            for call in calls {
                output.push(function_call_item(
                    &call.id,
                    &call.function.name,
                    &call.function.arguments,
                ));
            }
        }
        MessageContent::TextWithToolCalls {
            text, tool_calls, ..
        } => {
            push_text_message(output, text);
            for call in tool_calls {
                output.push(function_call_item(
                    &call.id,
                    &call.function.name,
                    &call.function.arguments,
                ));
            }
        }
        MessageContent::ToolResult(_) => {}
        // Responses 无 reasoning item 建模：丢弃并 debug 显性化
        MessageContent::Reasoning { .. } => {
            crate::protocol::drop_unsupported(
                crate::protocol::Protocol::Responses,
                "message.reasoning",
                "no responses reasoning item in scope",
            );
        }
    }
}

fn push_text_message(output: &mut Vec<OutputItem>, text: &str) {
    output.push(OutputItem::Message {
        id: format!("msg_{}", Uuid::new_v4()),
        status: "completed".to_string(),
        role: "assistant".to_string(),
        content: vec![OutputContent::OutputText {
            text: text.to_string(),
            annotations: Vec::new(),
        }],
    });
}

fn function_call_item(call_id: &str, name: &str, arguments: &str) -> OutputItem {
    OutputItem::FunctionCall {
        id: format!("fc_{}", Uuid::new_v4()),
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: arguments.to_string(),
        status: "completed".to_string(),
    }
}

fn map_usage(usage: &crate::protocol::unified::TokenUsage) -> ResponsesUsage {
    // details 明细优先回写；缺省时 legacy cache_read_tokens 并入
    // cached_tokens（与 OpenAI 面策略一致）；reasoning_tokens 从 IR
    // completion_tokens_details 回写（D4）
    let input_tokens_details = match usage
        .prompt_tokens_details
        .as_ref()
        .map(|details| details.cached_tokens)
    {
        Some(cached) => Some(InputTokensDetails {
            cached_tokens: cached.min(u32::MAX as u64) as u32,
        }),
        None if usage.cache_read_tokens > 0 => Some(InputTokensDetails {
            cached_tokens: usage.cache_read_tokens.min(u32::MAX as u64) as u32,
        }),
        None => None,
    };
    let output_tokens_details =
        usage
            .completion_tokens_details
            .as_ref()
            .map(|details| OutputTokensDetails {
                reasoning_tokens: details.reasoning_tokens.min(u32::MAX as u64) as u32,
            });
    ResponsesUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        input_tokens_details,
        output_tokens_details,
    }
}

/// 从请求构造 `queued` 响应（后台任务入队即返回）。
pub fn queued_response(id: &str, request: &ResponsesRequest) -> ResponsesResponse {
    ResponsesResponse {
        id: id.to_string(),
        object: "response".to_string(),
        created_at: now_secs(),
        status: "queued".to_string(),
        error: None,
        incomplete_details: None,
        instructions: request.instructions.clone(),
        max_output_tokens: request.max_output_tokens,
        model: request.model.clone(),
        output: Vec::new(),
        parallel_tool_calls: request.parallel_tool_calls,
        previous_response_id: request.previous_response_id.clone(),
        reasoning: None,
        store: true,
        temperature: request.temperature,
        text: request
            .text
            .as_ref()
            .and_then(|text| serde_json::to_value(text).ok()),
        tool_choice: request.tool_choice.clone(),
        tools: request.tools.clone(),
        top_p: request.top_p,
        truncation: request.truncation.clone(),
        usage: ResponsesUsage::default(),
        user: request.user.clone(),
        metadata: request.metadata.clone().unwrap_or_default(),
        extra: serde_json::Map::new(),
    }
}

/// 秒级时间戳（与 chat 响应 `created` 同口径）。
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::unified::{Choice, Message, Role, TokenUsage, ToolCall, ToolCallFunction};
    use serde_json::json;

    fn response_with(content: MessageContent, finish: FinishReason) -> UnifiedResponse {
        UnifiedResponse {
            id: "chatcmpl_1".into(),
            object: "chat.completion".into(),
            created: 1_700_000_000,
            model: "gpt-4.1".into(),
            choices: vec![Choice {
                index: 0,
                message: Some(Message {
                    role: Role::Assistant,
                    content,
                }),
                finish_reason: Some(finish.to_wire().to_string()),
                finish: Some(finish),
                logprobs: None,
            }],
            usage: TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                cache_read_tokens: 3,
                ..Default::default()
            },
            stop_sequence: None,
        }
    }

    fn request() -> ResponsesRequest {
        ResponsesRequest {
            model: "gpt-4.1".into(),
            input: json!("hi"),
            store: Some(true),
            metadata: Some([("trace".to_string(), "t1".to_string())].into()),
            ..Default::default()
        }
    }

    #[test]
    fn text_response_maps_to_message_item_and_usage() {
        let unified = response_with(MessageContent::Text("hello".into()), FinishReason::Stop);
        let response = to_responses_response("resp_1", &unified, &request(), true);
        assert_eq!(response.object, "response");
        assert_eq!(response.status, "completed");
        assert_eq!(response.output.len(), 1);
        match &response.output[0] {
            OutputItem::Message { role, content, .. } => {
                assert_eq!(role, "assistant");
                match &content[0] {
                    OutputContent::OutputText { text, annotations } => {
                        assert_eq!(text, "hello");
                        assert!(annotations.is_empty());
                    }
                }
            }
            other => panic!("期望 message item: {other:?}"),
        }
        assert_eq!(response.usage.input_tokens, 11);
        assert_eq!(response.usage.output_tokens, 7);
        assert_eq!(
            response
                .usage
                .input_tokens_details
                .as_ref()
                .unwrap()
                .cached_tokens,
            3
        );
        assert!(response.store);
        assert_eq!(response.metadata["trace"], "t1");
    }

    #[test]
    fn usage_details_roundtrip_with_reasoning_tokens() {
        use crate::protocol::unified::{CompletionTokensDetails, PromptTokensDetails};
        let mut unified = response_with(MessageContent::Text("hi".into()), FinishReason::Stop);
        unified.usage.prompt_tokens_details = Some(PromptTokensDetails {
            audio_tokens: 1,
            cached_tokens: 5,
        });
        unified.usage.completion_tokens_details = Some(CompletionTokensDetails {
            reasoning_tokens: 4,
            ..Default::default()
        });
        let response = to_responses_response("resp_3", &unified, &request(), true);
        assert_eq!(
            response
                .usage
                .input_tokens_details
                .as_ref()
                .expect("input details")
                .cached_tokens,
            5,
            "details 优先于 legacy cache_read_tokens"
        );
        assert_eq!(
            response
                .usage
                .output_tokens_details
                .as_ref()
                .expect("output details")
                .reasoning_tokens,
            4,
            "completion_tokens_details.reasoning_tokens 回写"
        );

        // 无 details 且无 legacy cached → 不写 details 键
        let mut unified = response_with(MessageContent::Text("hi".into()), FinishReason::Stop);
        unified.usage.cache_read_tokens = 0;
        let response = to_responses_response("resp_4", &unified, &request(), true);
        assert!(response.usage.input_tokens_details.is_none());
        assert!(response.usage.output_tokens_details.is_none());
    }

    #[test]
    fn tool_calls_map_to_function_call_items() {
        let unified = response_with(
            MessageContent::ToolCalls(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "get_weather".into(),
                    arguments: "{\"city\":\"bj\"}".into(),
                },
            }]),
            FinishReason::ToolCalls,
        );
        let response = to_responses_response("resp_2", &unified, &request(), true);
        assert_eq!(response.status, "completed");
        match &response.output[0] {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, "{\"city\":\"bj\"}");
            }
            other => panic!("期望 function_call item: {other:?}"),
        }
    }

    #[test]
    fn length_and_content_filter_map_to_incomplete() {
        let length = response_with(MessageContent::Text("x".into()), FinishReason::Length);
        let response = to_responses_response("resp_3", &length, &request(), true);
        assert_eq!(response.status, "incomplete");
        assert_eq!(
            response.incomplete_details.as_ref().unwrap().reason,
            "max_output_tokens"
        );

        let filtered = response_with(
            MessageContent::Text("x".into()),
            FinishReason::ContentFilter,
        );
        let response = to_responses_response("resp_4", &filtered, &request(), true);
        assert_eq!(response.status, "incomplete");
        assert_eq!(
            response.incomplete_details.as_ref().unwrap().reason,
            "content_filter"
        );
    }

    #[test]
    fn queued_response_has_empty_output() {
        let response = queued_response("resp_5", &request());
        assert_eq!(response.status, "queued");
        assert!(response.output.is_empty());
        assert!(response.store);
    }
}
