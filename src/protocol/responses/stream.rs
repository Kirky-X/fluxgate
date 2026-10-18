// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Responses 语义化 SSE：IR → 事件编码器（入站）与上游事件 → IR 解析器
//!（出站）。
//!
//! 事件序（每个 item 先 added 后 done，终止事件恰好一次）：
//! `response.created` → `response.in_progress` →
//! `response.output_item.added` → `response.content_part.added` →
//! `response.output_text.delta`* → `response.output_text.done` →
//! `response.content_part.done` → `response.output_item.done` →
//! `response.completed` / `response.incomplete` / `response.failed`。
//!
//! `sequence_number` 从 0 单调递增；`event:` 名与 `data.type` 同值。

use crate::protocol::responses::types::{
    IncompleteDetails, InputTokensDetails, OutputContent, OutputItem, ResponseError,
    ResponsesResponse, ResponsesUsage, StreamEvent,
};
use crate::protocol::sse_frame::encode_sse_event;
use crate::protocol::unified::{FinishReason, MessageContent, StreamChunk};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

/// 打开中的输出 item（message 或 function_call）。
enum OpenItem {
    Message {
        id: String,
        text: String,
    },
    Call {
        item_id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
}

/// IR 流 → Responses 语义事件编码器。
pub struct ResponsesStreamEncoder {
    template: ResponsesResponse,
    sequence: u64,
    next_output_index: u32,
    call_indices: BTreeMap<u32, u32>,
    items: BTreeMap<u32, OpenItem>,
    completed: BTreeMap<u32, OutputItem>,
    usage: ResponsesUsage,
    finish_reason: Option<String>,
    started: bool,
    terminated: bool,
    /// 终止时的完整响应快照（流式落库用）
    final_response: Option<ResponsesResponse>,
}

impl ResponsesStreamEncoder {
    /// `template` 提供 id/model/回显字段；状态与 output/usage 由编码器填充。
    pub fn new(template: ResponsesResponse) -> Self {
        Self {
            template,
            sequence: 0,
            next_output_index: 0,
            call_indices: BTreeMap::new(),
            items: BTreeMap::new(),
            completed: BTreeMap::new(),
            usage: ResponsesUsage::default(),
            finish_reason: None,
            started: false,
            terminated: false,
            final_response: None,
        }
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// 终止快照（completed/incomplete/failed 后为 Some）。
    pub fn final_response(&self) -> Option<&ResponsesResponse> {
        self.final_response.as_ref()
    }

    /// 起始事件（幂等）。
    pub fn start(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let mut response = self.template.clone();
        response.status = "in_progress".to_string();
        response.output = Vec::new();
        response.usage = ResponsesUsage::default();
        let events = vec![
            self.emit("response.created", fields_with_response(&response)),
            self.emit("response.in_progress", fields_with_response(&response)),
        ];
        events
    }

    /// 消费一个 IR chunk，产出零到多个 SSE 帧。
    pub fn on_chunk(&mut self, chunk: &StreamChunk) -> Vec<String> {
        if self.terminated {
            return Vec::new();
        }
        match chunk {
            StreamChunk::Content {
                delta,
                finish_reason,
                ..
            } => {
                let mut events = Vec::new();
                if let MessageContent::Text(text) = delta {
                    if !text.is_empty() {
                        let index = self.ensure_message(&mut events);
                        if let Some(OpenItem::Message { id, text: buffer }) =
                            self.items.get_mut(&index)
                        {
                            buffer.push_str(text);
                            let id = id.clone();
                            events.push(
                                self.emit(
                                    "response.output_text.delta",
                                    json!({
                                        "item_id": id,
                                        "output_index": index,
                                        "content_index": 0,
                                        "delta": text,
                                    })
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default(),
                                ),
                            );
                        }
                    }
                }
                if let Some(reason) = finish_reason {
                    self.finish_reason = Some(reason.clone());
                }
                events
            }
            StreamChunk::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                let mut events = Vec::new();
                let output_index = self.ensure_call(*index, id, name, &mut events);
                if let Some(OpenItem::Call {
                    item_id,
                    arguments: buffer,
                    ..
                }) = self.items.get_mut(&output_index)
                {
                    if let Some(arguments) = arguments {
                        buffer.push_str(arguments);
                        let item_id = item_id.clone();
                        events.push(
                            self.emit(
                                "response.function_call_arguments.delta",
                                json!({
                                    "item_id": item_id,
                                    "output_index": output_index,
                                    "delta": arguments,
                                })
                                .as_object()
                                .cloned()
                                .unwrap_or_default(),
                            ),
                        );
                    }
                }
                events
            }
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cache_read_tokens,
                finish_reason,
                ..
            } => {
                self.usage = ResponsesUsage {
                    input_tokens: *prompt_tokens,
                    output_tokens: *completion_tokens,
                    total_tokens: *total_tokens,
                    input_tokens_details: (*cache_read_tokens > 0).then_some(InputTokensDetails {
                        cached_tokens: *cache_read_tokens,
                    }),
                    output_tokens_details: None,
                };
                if let Some(reason) = finish_reason {
                    self.finish_reason = Some(reason.clone());
                }
                Vec::new()
            }
            StreamChunk::Heartbeat => Vec::new(),
        }
    }

    /// 收流：补齐所有 item 的 done 事件并发出终止事件（恰好一次）。
    pub fn finish(&mut self) -> Vec<String> {
        if self.terminated {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.close_all(&mut events);
        let (status, incomplete) = match self.finish_reason.as_deref() {
            Some("length") => (
                "incomplete",
                Some(IncompleteDetails {
                    reason: "max_output_tokens".to_string(),
                }),
            ),
            Some("content_filter") => (
                "incomplete",
                Some(IncompleteDetails {
                    reason: "content_filter".to_string(),
                }),
            ),
            _ => ("completed", None),
        };
        let mut response = self.build_final_response(status, incomplete);
        response.error = None;
        self.final_response = Some(response.clone());
        let event_type = if status == "incomplete" {
            "response.incomplete"
        } else {
            "response.completed"
        };
        events.push(self.emit(event_type, fields_with_response(&response)));
        self.terminated = true;
        events
    }

    /// 失败终止：补齐已打开 item 后发 `response.failed`（message 为安全文案）。
    pub fn fail(&mut self, message: &str) -> Vec<String> {
        if self.terminated {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.close_all(&mut events);
        let mut response = self.build_final_response("failed", None);
        response.error = Some(ResponseError {
            code: Some("upstream_error".to_string()),
            message: message.to_string(),
        });
        self.final_response = Some(response.clone());
        events.push(self.emit("response.failed", fields_with_response(&response)));
        self.terminated = true;
        events
    }

    fn build_final_response(
        &self,
        status: &str,
        incomplete: Option<IncompleteDetails>,
    ) -> ResponsesResponse {
        let mut response = self.template.clone();
        response.status = status.to_string();
        response.output = self.completed.values().cloned().collect();
        response.usage = self.usage.clone();
        response.incomplete_details = incomplete;
        response
    }

    fn ensure_message(&mut self, events: &mut Vec<String>) -> u32 {
        if let Some((index, _)) = self
            .items
            .iter()
            .find(|(_, item)| matches!(item, OpenItem::Message { .. }))
        {
            return *index;
        }
        let index = self.next_output_index;
        self.next_output_index += 1;
        let id = format!("msg_{}", Uuid::new_v4());
        self.items.insert(
            index,
            OpenItem::Message {
                id: id.clone(),
                text: String::new(),
            },
        );
        events.push(
            self.emit(
                "response.output_item.added",
                json!({
                    "output_index": index,
                    "item": {
                        "type": "message",
                        "id": id,
                        "status": "in_progress",
                        "role": "assistant",
                        "content": [],
                    },
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        events.push(
            self.emit(
                "response.content_part.added",
                json!({
                    "item_id": id,
                    "output_index": index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        index
    }

    fn ensure_call(
        &mut self,
        call_index: u32,
        id: &str,
        name: &Option<String>,
        events: &mut Vec<String>,
    ) -> u32 {
        if let Some(output_index) = self.call_indices.get(&call_index) {
            return *output_index;
        }
        // 新工具调用前先关闭已打开的 message（文本段结束）
        if let Some((message_index, _)) = self
            .items
            .iter()
            .find(|(_, item)| matches!(item, OpenItem::Message { .. }))
        {
            let message_index = *message_index;
            self.close_message(message_index, events);
        }
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        self.call_indices.insert(call_index, output_index);
        let item_id = format!("fc_{}", Uuid::new_v4());
        let call_id = if id.is_empty() {
            format!("call_{}", Uuid::new_v4())
        } else {
            id.to_string()
        };
        let name = name.clone().unwrap_or_default();
        self.items.insert(
            output_index,
            OpenItem::Call {
                item_id: item_id.clone(),
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: String::new(),
            },
        );
        events.push(
            self.emit(
                "response.output_item.added",
                json!({
                    "output_index": output_index,
                    "item": {
                        "type": "function_call",
                        "id": item_id,
                        "call_id": call_id,
                        "name": name,
                        "arguments": "",
                        "status": "in_progress",
                    },
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        output_index
    }

    fn close_all(&mut self, events: &mut Vec<String>) {
        let indices: Vec<u32> = self.items.keys().copied().collect();
        for index in indices {
            match self.items.get(&index) {
                Some(OpenItem::Message { .. }) => self.close_message(index, events),
                Some(OpenItem::Call { .. }) => self.close_call(index, events),
                None => {}
            }
        }
    }

    fn close_message(&mut self, index: u32, events: &mut Vec<String>) {
        let Some(OpenItem::Message { id, text }) = self.items.remove(&index) else {
            return;
        };
        let part = json!({"type": "output_text", "text": text, "annotations": []});
        events.push(
            self.emit(
                "response.output_text.done",
                json!({
                    "item_id": id,
                    "output_index": index,
                    "content_index": 0,
                    "text": text,
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        events.push(
            self.emit(
                "response.content_part.done",
                json!({
                    "item_id": id,
                    "output_index": index,
                    "content_index": 0,
                    "part": part,
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        let item = OutputItem::Message {
            id,
            status: "completed".to_string(),
            role: "assistant".to_string(),
            content: vec![OutputContent::OutputText {
                text,
                annotations: Vec::new(),
            }],
        };
        events.push(
            self.emit(
                "response.output_item.done",
                json!({"output_index": index, "item": item})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        self.completed.insert(index, item);
    }

    fn close_call(&mut self, index: u32, events: &mut Vec<String>) {
        let Some(OpenItem::Call {
            item_id,
            call_id,
            name,
            arguments,
        }) = self.items.remove(&index)
        else {
            return;
        };
        events.push(
            self.emit(
                "response.function_call_arguments.done",
                json!({
                    "item_id": item_id,
                    "output_index": index,
                    "arguments": arguments,
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ),
        );
        let item = OutputItem::FunctionCall {
            id: item_id,
            call_id,
            name,
            arguments,
            status: "completed".to_string(),
        };
        events.push(
            self.emit(
                "response.output_item.done",
                json!({"output_index": index, "item": item})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            ),
        );
        self.completed.insert(index, item);
    }

    fn emit(&mut self, event_type: &str, mut payload: Map<String, Value>) -> String {
        payload.insert("type".to_string(), Value::String(event_type.to_string()));
        payload.insert(
            "sequence_number".to_string(),
            Value::Number(self.sequence.into()),
        );
        self.sequence += 1;
        let data = Value::Object(payload).to_string();
        encode_sse_event(&data, Some(event_type))
    }
}

fn fields_with_response(response: &ResponsesResponse) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert(
        "response".to_string(),
        serde_json::to_value(response).unwrap_or(Value::Null),
    );
    fields
}

/// 上游 Responses 事件 → IR chunk（无状态：工具首帧由
/// `response.output_item.added` 携带 id/name）。
pub fn parse_stream_frame(data: &str) -> Option<StreamChunk> {
    let event = StreamEvent::from_data(data)?;
    match event.event_type.as_str() {
        "response.output_item.added" => {
            let item = event.item.as_ref()?;
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return None;
            }
            let call_id = item.get("call_id").and_then(Value::as_str)?.to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(StreamChunk::ToolCall {
                index: event.output_index.unwrap_or(0),
                id: call_id,
                name: Some(name),
                arguments: None,
            })
        }
        "response.function_call_arguments.delta" => Some(StreamChunk::ToolCall {
            index: event.output_index.unwrap_or(0),
            id: String::new(),
            name: None,
            arguments: event.delta.clone(),
        }),
        "response.output_text.delta" => Some(StreamChunk::Content {
            index: event.output_index.unwrap_or(0),
            delta: MessageContent::Text(event.delta.unwrap_or_default()),
            finish_reason: None,
        }),
        "response.completed" | "response.incomplete" => {
            let response = event.response?;
            let finish_reason = infer_finish_reason(&response);
            Some(StreamChunk::Usage {
                prompt_tokens: response.usage.input_tokens,
                completion_tokens: response.usage.output_tokens,
                total_tokens: response.usage.total_tokens,
                cache_read_tokens: response
                    .usage
                    .input_tokens_details
                    .as_ref()
                    .map(|details| details.cached_tokens)
                    .unwrap_or(0),
                cache_creation_tokens: 0,
                finish_reason: Some(finish_reason.to_string()),
            })
        }
        // `response.failed` / `error` 由 SseFrame::is_error 终止流
        _ => None,
    }
}

fn infer_finish_reason(response: &ResponsesResponse) -> &'static str {
    match response
        .incomplete_details
        .as_ref()
        .map(|details| details.reason.as_str())
    {
        Some("max_output_tokens") => FinishReason::Length.to_wire(),
        Some("content_filter") => FinishReason::ContentFilter.to_wire(),
        _ => {
            if response
                .output
                .iter()
                .any(|item| matches!(item, OutputItem::FunctionCall { .. }))
            {
                FinishReason::ToolCalls.to_wire()
            } else {
                FinishReason::Stop.to_wire()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> ResponsesResponse {
        ResponsesResponse {
            id: "resp_test".into(),
            object: "response".into(),
            created_at: 1,
            status: "in_progress".into(),
            error: None,
            incomplete_details: None,
            instructions: None,
            max_output_tokens: Some(16),
            model: "m".into(),
            output: Vec::new(),
            parallel_tool_calls: None,
            previous_response_id: None,
            reasoning: None,
            store: true,
            temperature: None,
            text: None,
            tool_choice: None,
            tools: None,
            top_p: None,
            truncation: None,
            usage: ResponsesUsage::default(),
            user: None,
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn event_type(frame: &str) -> String {
        let line = frame
            .lines()
            .find(|line| line.starts_with("event: "))
            .expect("event 行");
        line["event: ".len()..].to_string()
    }

    fn payload(frame: &str) -> Value {
        let data = frame
            .lines()
            .find(|line| line.starts_with("data: "))
            .expect("data 行");
        serde_json::from_str(&data["data: ".len()..]).expect("JSON 负载")
    }

    #[test]
    fn text_stream_emits_full_lifecycle() {
        let mut encoder = ResponsesStreamEncoder::new(template());
        let mut events = encoder.start();
        events.extend(encoder.on_chunk(&StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("hel".into()),
            finish_reason: None,
        }));
        events.extend(encoder.on_chunk(&StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("lo".into()),
            finish_reason: Some("stop".into()),
        }));
        events.extend(encoder.on_chunk(&StreamChunk::Usage {
            prompt_tokens: 5,
            completion_tokens: 2,
            total_tokens: 7,
            cache_read_tokens: 1,
            cache_creation_tokens: 0,
            finish_reason: None,
        }));
        events.extend(encoder.finish());

        let types: Vec<String> = events.iter().map(|frame| event_type(frame)).collect();
        assert_eq!(
            types,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        for (index, frame) in events.iter().enumerate() {
            assert_eq!(
                payload(frame)["sequence_number"],
                json!(index as u64),
                "sequence_number 必须从 0 单调递增"
            );
        }
        let completed = payload(events.last().unwrap());
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["output"][0]["type"], "message");
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "hello"
        );
        assert_eq!(completed["response"]["usage"]["input_tokens"], 5);
        assert_eq!(
            completed["response"]["usage"]["input_tokens_details"]["cached_tokens"],
            1
        );
    }

    #[test]
    fn tool_call_stream_accumulates_arguments() {
        let mut encoder = ResponsesStreamEncoder::new(template());
        let mut events = encoder.start();
        events.extend(encoder.on_chunk(&StreamChunk::ToolCall {
            index: 0,
            id: "call_1".into(),
            name: Some("get_weather".into()),
            arguments: None,
        }));
        events.extend(encoder.on_chunk(&StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("{\"city\":".into()),
        }));
        events.extend(encoder.on_chunk(&StreamChunk::ToolCall {
            index: 0,
            id: String::new(),
            name: None,
            arguments: Some("\"bj\"}".into()),
        }));
        events.extend(encoder.finish());

        let types: Vec<String> = events.iter().map(|frame| event_type(frame)).collect();
        assert!(types.contains(&"response.function_call_arguments.delta".to_string()));
        assert!(types.contains(&"response.function_call_arguments.done".to_string()));
        assert_eq!(types.last().unwrap(), "response.completed");
        let completed = payload(events.last().unwrap());
        let item = &completed["response"]["output"][0];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["call_id"], "call_1");
        assert_eq!(item["name"], "get_weather");
        assert_eq!(item["arguments"], "{\"city\":\"bj\"}");
        assert_eq!(completed["response"]["status"], "completed");
    }

    #[test]
    fn length_finish_emits_incomplete() {
        let mut encoder = ResponsesStreamEncoder::new(template());
        let mut events = encoder.start();
        events.extend(encoder.on_chunk(&StreamChunk::Content {
            index: 0,
            delta: MessageContent::Text("x".into()),
            finish_reason: Some("length".into()),
        }));
        events.extend(encoder.finish());
        assert_eq!(
            event_type(events.last().unwrap()),
            "response.incomplete",
            "长度截断必须发 incomplete"
        );
        let terminal = payload(events.last().unwrap());
        assert_eq!(terminal["response"]["status"], "incomplete");
        assert_eq!(
            terminal["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn fail_emits_failed_once_with_safe_message() {
        let mut encoder = ResponsesStreamEncoder::new(template());
        let _ = encoder.start();
        let events = encoder.fail("upstream interrupted");
        assert_eq!(events.len(), 1);
        assert_eq!(event_type(&events[0]), "response.failed");
        let failed = payload(&events[0]);
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(
            failed["response"]["error"]["message"],
            "upstream interrupted"
        );
        assert!(encoder.is_terminated());
        assert!(encoder.finish().is_empty(), "终止后 finish 必须无输出");
    }

    #[test]
    fn parse_upstream_frames_to_ir() {
        let text = parse_stream_frame(
            r#"{"type":"response.output_text.delta","output_index":0,"delta":"hi"}"#,
        )
        .expect("文本增量");
        assert!(matches!(
            text,
            StreamChunk::Content { delta: MessageContent::Text(ref t), .. } if t == "hi"
        ));

        let added = parse_stream_frame(
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call_9","name":"f"}}"#,
        )
        .expect("工具首帧");
        assert!(matches!(
            added,
            StreamChunk::ToolCall { id, name: Some(ref n), .. } if id == "call_9" && n == "f"
        ));

        let delta = parse_stream_frame(
            r#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{}"}"#,
        )
        .expect("参数增量");
        assert!(matches!(
            delta,
            StreamChunk::ToolCall { arguments: Some(ref a), .. } if a == "{}"
        ));

        let completed = parse_stream_frame(
            r#"{"type":"response.completed","response":{"id":"r","object":"response","created_at":1,"status":"completed","model":"m","output":[{"type":"function_call","id":"fc","call_id":"c","name":"f","arguments":"{}","status":"completed"}],"usage":{"input_tokens":3,"output_tokens":4,"total_tokens":7,"input_tokens_details":{"cached_tokens":1}}}}"#,
        )
        .expect("完成帧");
        match completed {
            StreamChunk::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                finish_reason,
                ..
            } => {
                assert_eq!(prompt_tokens, 3);
                assert_eq!(completion_tokens, 4);
                assert_eq!(cache_read_tokens, 1);
                assert_eq!(finish_reason.as_deref(), Some("tool_calls"));
            }
            other => panic!("期望 Usage: {other:?}"),
        }

        assert!(parse_stream_frame(r#"{"type":"response.failed","response":{}}"#).is_none());
        assert!(parse_stream_frame(r#"{"type":"unknown.event"}"#).is_none());
    }
}
