// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use serde_json::json;
    use nexusflow::protocol::embedding_converter::{
        embedding_request_to_openai, embedding_response_from_openai,
    };

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
