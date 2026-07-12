// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::error::NexusFlowError;
use crate::protocol::unified::*;
use crate::protocol::{Protocol, ProtocolConverter};
use serde_json::{json, Value};

#[allow(dead_code)]
pub struct OpenAIEmbeddingConverter;

impl OpenAIEmbeddingConverter {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ProtocolConverter for OpenAIEmbeddingConverter {
    fn input_protocol(&self) -> Protocol {
        Protocol::OpenAI
    }

    fn output_protocol(&self) -> Protocol {
        Protocol::OpenAI
    }

    fn to_unified(&self, _body: Value) -> Result<UnifiedRequest, NexusFlowError> {
        // Embeddings don't map to UnifiedRequest directly in this context,
        // as they are handled separately.
        Err(NexusFlowError::Protocol {
            source: "Embeddings cannot be converted to UnifiedRequest".into(),
            inner: None,
        })
    }

    fn request_to_external(&self, _request: UnifiedRequest) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "UnifiedRequest cannot be converted to Embedding request".into(),
            inner: None,
        })
    }

    fn from_unified(&self, _response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "UnifiedResponse cannot be converted to Embedding response".into(),
            inner: None,
        })
    }

    fn parse_response(&self, _response: Value) -> Result<UnifiedResponse, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Embedding response cannot be parsed to UnifiedResponse".into(),
            inner: None,
        })
    }

    #[allow(dead_code)]
    fn stream_chunk_to_external(&self, _chunk: StreamChunk) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Embedding does not support streaming".into(),
            inner: None,
        })
    }
}

#[allow(dead_code)]
pub struct AnthropicEmbeddingConverter;

impl AnthropicEmbeddingConverter {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl ProtocolConverter for AnthropicEmbeddingConverter {
    fn input_protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    fn output_protocol(&self) -> Protocol {
        Protocol::Anthropic
    }

    fn to_unified(&self, _body: Value) -> Result<UnifiedRequest, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Anthropic doesn't support embeddings".into(),
            inner: None,
        })
    }

    fn request_to_external(&self, _request: UnifiedRequest) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Anthropic doesn't support embeddings".into(),
            inner: None,
        })
    }

    fn from_unified(&self, _response: UnifiedResponse) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Anthropic doesn't support embeddings".into(),
            inner: None,
        })
    }

    fn parse_response(&self, _response: Value) -> Result<UnifiedResponse, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Anthropic doesn't support embeddings".into(),
            inner: None,
        })
    }

    #[allow(dead_code)]
    fn stream_chunk_to_external(&self, _chunk: StreamChunk) -> Result<Value, NexusFlowError> {
        Err(NexusFlowError::Protocol {
            source: "Anthropic doesn't support embeddings".into(),
            inner: None,
        })
    }
}

/// OpenAI Embedding 请求转换
pub fn embedding_request_to_openai(body: Value) -> Result<Value, NexusFlowError> {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or(NexusFlowError::Validation {
            field: "model".into(),
        })?;

    let input = body
        .get("input")
        .map(|v| {
            if v.is_string() {
                vec![v.as_str().unwrap_or("").to_string()]
            } else if v.is_array() {
                v.as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|s| s.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .unwrap_or_default();

    let mut result = json!({
        "model": model,
        "input": input,
    });

    if let Some(encoding_format) = body.get("encoding_format").and_then(|v| v.as_str()) {
        result["encoding_format"] = json!(encoding_format);
    }

    if let Some(dimensions) = body.get("dimensions").and_then(|v| v.as_u64()) {
        result["dimensions"] = json!(dimensions);
    }

    if let Some(user) = body.get("user").and_then(|v| v.as_str()) {
        result["user"] = json!(user);
    }

    Ok(result)
}

/// OpenAI Embedding 响应转换
#[allow(dead_code)]
pub fn embedding_response_from_openai(
    response: Value,
) -> Result<UnifiedEmbeddingResponse, NexusFlowError> {
    let id = response
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("embedding-unknown")
        .to_string();

    let model = response
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let created = response
        .get("created")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let data: Vec<EmbeddingData> = response
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let embedding = item
                .get("embedding")?
                .as_array()?
                .iter()
                .filter_map(|e| e.as_f64().map(|f| f as f32))
                .collect();
            Some(EmbeddingData {
                index: i as u32,
                object_: item.get("object")?.as_str()?.to_string(),
                embedding,
            })
        })
        .collect();

    let usage = response
        .get("usage")
        .and_then(|v| v.as_object())
        .map(|u| EmbeddingUsage {
            prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        })
        .unwrap_or_default();

    Ok(UnifiedEmbeddingResponse {
        id,
        object: "list".to_string(),
        created,
        model,
        data,
        usage,
    })
}
