#![cfg(feature = "database")]

// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::database::{Database, LogFilter, Repository, RequestLog};
    use std::sync::Arc;

    async fn create_test_pool() -> DbPool {
        let config = DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        };
        DbPool::with_config(config).await.unwrap()
    }

    #[tokio::test]
    async fn test_repository_new() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool);
        repository.initialize().await.unwrap();
        repository.initialize().await.unwrap();
        // Just verify it doesn't panic
    }

    #[tokio::test]
    async fn test_repository_log_request() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool);
        repository.initialize().await.unwrap();
        repository.initialize().await.unwrap();

        let log = RequestLog {
            id: Some(1),
            request_id: "test-001".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: r#"[{"role": "user", "content": "Hello"}]"#.to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: Some(r#"{"id": "test"}"#.to_string()),
            error_message: None,
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            duration_ms: Some(150),
            cost_usd: None,
        };

        let result = repository.log_request(&log).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_repository_query_logs_by_vendor() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool.clone());
        repository.initialize().await.unwrap();

        let now = chrono::Utc::now().timestamp();

        // Log some requests
        for i in 0..3 {
            let log = RequestLog {
                id: None,
                request_id: format!("test-{:03}", i),
                timestamp: now,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
                actual_vendor: "openai_main".to_string(),
                actual_model: "gpt-4".to_string(),
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };
            repository.log_request(&log).await.unwrap();
        }

        // Filter by vendor
        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: Some("openai_main".to_string()),
            model: None,
            limit: 10,
        };

        let result = repository.query_logs(&filter).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        assert_eq!(logs.len(), 3);
    }

    #[tokio::test]
    async fn test_repository_query_logs_by_time_range() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool.clone());
        repository.initialize().await.unwrap();

        let now = chrono::Utc::now().timestamp();

        // Log requests at different times
        for i in 0..5 {
            let log = RequestLog {
                id: None,
                request_id: format!("time-test-{:03}", i),
                timestamp: now - (i as i64 * 60), // 1 minute apart
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
                actual_vendor: "openai_main".to_string(),
                actual_model: "gpt-4".to_string(),
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };
            repository.log_request(&log).await.unwrap();
        }

        // Filter by time range (last 3 minutes)
        let filter = LogFilter {
            start_time: Some(now - 180),
            end_time: Some(now),
            vendor: None,
            model: None,
            limit: 10,
        };

        let result = repository.query_logs(&filter).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        // Should get requests within the time range
        assert!(logs.len() >= 3);
    }

    #[tokio::test]
    async fn test_repository_search_logs() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool.clone());
        repository.initialize().await.unwrap();

        let now = chrono::Utc::now().timestamp();

        // Insert logs with specific content
        for i in 0..3 {
            let log = RequestLog {
                id: None,
                request_id: format!("search-test-{:03}", i),
                timestamp: now,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: format!(
                    r#"{{"role": "user", "content": "Search for {}", "index": {}}}"#,
                    "keyword", i
                ),
                actual_vendor: "openai_main".to_string(),
                actual_model: "gpt-4".to_string(),
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };
            repository.log_request(&log).await.unwrap();
        }

        let result = repository.search_logs("keyword", 10).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        assert_eq!(logs.len(), 3);
    }

    #[tokio::test]
    async fn test_repository_get_logs_limit() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool.clone());
        repository.initialize().await.unwrap();

        let now = chrono::Utc::now().timestamp();

        // Insert 20 logs
        for i in 0..20 {
            let log = RequestLog {
                id: None,
                request_id: format!("limit-test-{:03}", i),
                timestamp: now,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
                actual_vendor: "openai_main".to_string(),
                actual_model: "gpt-4".to_string(),
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };
            repository.log_request(&log).await.unwrap();
        }

        // Query with limit 5
        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: None,
            model: None,
            limit: 5,
        };

        let result = repository.query_logs(&filter).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        assert_eq!(logs.len(), 5); // Should be limited to 5
    }

    #[tokio::test]
    async fn test_repository_get_logs_empty_result() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool);
        repository.initialize().await.unwrap();

        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: Some("nonexistent".to_string()),
            model: None,
            limit: 10,
        };

        let result = repository.query_logs(&filter).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        assert!(logs.is_empty()); // No logs for nonexistent vendor
    }

    #[tokio::test]
    async fn test_database_new() {
        let pool = create_test_pool().await;
        let _database = Database {
            pool: pool.clone(),
            repository: Arc::new(Repository::new(pool)),
        };
        // Just verify it doesn't panic
    }

    #[tokio::test]
    async fn test_database_close() {
        let pool = create_test_pool().await;
        let database = Database {
            pool: pool.clone(),
            repository: Arc::new(Repository::new(pool)),
        };

        // Close should not panic
        database.close().await;
    }

    #[tokio::test]
    async fn test_database_clone() {
        let pool = create_test_pool().await;
        let database = Database {
            pool: pool.clone(),
            repository: Arc::new(Repository::new(pool)),
        };

        let _cloned = database.clone();
        // Just verify clone works
    }

    #[tokio::test]
    async fn test_repository_batch_insert() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool.clone());
        repository.initialize().await.unwrap();
        let now = chrono::Utc::now().timestamp();

        // Batch insert
        let mut handles = vec![];
        for i in 0..10 {
            let repo = repository.clone();
            let log = RequestLog {
                id: None,
                request_id: format!("batch-{:03}", i),
                timestamp: now,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
                actual_vendor: "openai_main".to_string(),
                actual_model: "gpt-4".to_string(),
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };

            handles.push(tokio::spawn(async move { repo.log_request(&log).await }));
        }

        // Wait for all inserts
        let results: Vec<Result<(), dbnexus::DbError>> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // All should succeed
        assert_eq!(results.len(), 10);
        assert!(results.into_iter().all(|r| r.is_ok()));

        // Verify all were inserted
        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: None,
            model: None,
            limit: 100,
        };
        let logs = repository.query_logs(&filter).await.unwrap();
        assert_eq!(logs.len(), 10);
    }

    #[tokio::test]
    async fn test_request_log_struct() {
        let log = RequestLog {
            id: Some(1),
            request_id: "test-id".to_string(),
            timestamp: 1234567890,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
            actual_vendor: "test-vendor".to_string(),
            actual_model: "test-model".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: Some(r#"{"id": "test"}"#.to_string()),
            error_message: Some("test error".to_string()),
            prompt_tokens: Some(100),
            completion_tokens: Some(200),
            total_tokens: Some(300),
            duration_ms: Some(100),
            cost_usd: None,
        };

        assert_eq!(log.request_id, "test-id");
        assert_eq!(log.total_tokens, Some(300));
        assert_eq!(log.error_message, Some("test error".to_string()));
        assert_eq!(log.actual_vendor, "test-vendor");
    }

    #[tokio::test]
    async fn test_log_filter_empty() {
        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: None,
            model: None,
            limit: 0,
        };

        assert!(filter.start_time.is_none());
        assert!(filter.vendor.is_none());
        assert_eq!(filter.limit, 0);
    }

    #[tokio::test]
    async fn test_log_filter_with_values() {
        let now = chrono::Utc::now().timestamp();
        let filter = LogFilter {
            start_time: Some(now - 3600),
            end_time: Some(now),
            vendor: Some("anthropic".to_string()),
            model: Some("claude-3.5-sonnet".to_string()),
            limit: 50,
        };

        assert!(filter.start_time.is_some());
        assert!(filter.vendor.is_some());
        assert_eq!(filter.vendor.unwrap(), "anthropic");
        assert_eq!(filter.limit, 50);
    }

    #[tokio::test]
    async fn test_request_log_error_scenario() {
        let log = RequestLog {
            id: Some(1),
            request_id: "error-test".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(429),
            response: None,
            error_message: Some("Rate limited".to_string()),
            prompt_tokens: Some(0),
            completion_tokens: Some(0),
            total_tokens: Some(0),
            duration_ms: Some(5000),
            cost_usd: None,
        };

        assert_eq!(log.status_code, Some(429));
        assert!(log.error_message.is_some());
        assert!(log.duration_ms.unwrap() > 1000);
    }

    #[tokio::test]
    async fn test_repository_search_no_results() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool);
        repository.initialize().await.unwrap();

        let result = repository.search_logs("nonexistent_keyword_xyz", 10).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        assert!(logs.is_empty());
    }

    #[tokio::test]
    async fn test_repository_query_logs_by_model() {
        let pool = create_test_pool().await;
        let repository = Repository::new(pool);
        repository.initialize().await.unwrap();

        let now = chrono::Utc::now().timestamp();

        // Insert logs for different models
        for i in 0..5 {
            let log = RequestLog {
                id: None,
                request_id: format!("model-test-by-model-{:03}", i), // Unique request_id
                timestamp: now,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("gpt-4".to_string()),
                messages: "[{\"role\": \"user\", \"content\": \"Hello\"}]".to_string(),
                actual_vendor: "openai_main".to_string(),
                actual_model: if i < 3 {
                    "gpt-4".to_string()
                } else {
                    "gpt-3.5-turbo".to_string()
                },
                routing_strategy: "direct".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(100),
                completion_tokens: Some(50),
                total_tokens: Some(150),
                duration_ms: Some(150),
                cost_usd: None,
            };

            // Log each one individually
            let result = repository.log_request(&log).await;
            if result.is_err() {
                println!("Error inserting log {}: {:?}", i, result);
            }
        }

        // First verify logs were inserted
        let all_filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: None,
            model: None,
            limit: 10,
        };
        let all_logs = repository.query_logs(&all_filter).await.unwrap();
        println!("Total logs inserted: {}", all_logs.len());

        // Check what models were actually stored
        println!("All logs:");
        for log in &all_logs {
            println!(
                "  - id={:?}, request_id={}, actual_model={}",
                log.id, log.request_id, log.actual_model
            );
        }

        // Filter by actual_model = "gpt-4"
        let filter = LogFilter {
            start_time: None,
            end_time: None,
            vendor: None,
            model: Some("gpt-4".to_string()),
            limit: 10,
        };

        let result = repository.query_logs(&filter).await;
        assert!(result.is_ok());
        let logs = result.unwrap();
        println!("Logs matching model 'gpt-4': {}", logs.len());
        assert_eq!(logs.len(), 3); // Only first 3 have actual_model = "gpt-4"
    }
}

// ============ recId=9：单事务批量写入（Repository::log_requests_batch） ============

#[cfg(test)]
mod batch_tx {
    use dbnexus::{DbConfig, DbPool};
    use nexusflow::database::{Database, LogFilter, Repository, RequestLog};
    use std::sync::Arc;

    async fn create_test_pool() -> DbPool {
        let config = DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        };
        DbPool::with_config(config).await.unwrap()
    }

    fn sample_log(i: usize) -> RequestLog {
        RequestLog {
            id: None,
            request_id: format!("tx-batch-{i:03}"),
            timestamp: chrono::Utc::now().timestamp(),
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: format!(r#"{{"i":{i}}}"#),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "Abstract".to_string(),
            status_code: Some(200),
            response: Some(r#"{"ok":true}"#.to_string()),
            error_message: None,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            duration_ms: Some(20),
            cost_usd: None,
        }
    }

    /// recId=9 tests ①：批量 100 条单事务写入 → query_logs 恰 100 条
    ///（N 次 INSERT 一次 commit：不丢条、不留半批）。
    #[tokio::test]
    async fn test_log_requests_batch_atomic_100() {
        let pool = create_test_pool().await;
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.unwrap();

        let logs: Vec<RequestLog> = (0..100).map(sample_log).collect();
        repository
            .log_requests_batch(&logs)
            .await
            .expect("batch ok");

        let all = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(all.len(), 100, "事务批量必须整批落库");
    }

    /// 空批 no-op（writer_loop 的空缓冲快路径）。
    #[tokio::test]
    async fn test_log_requests_batch_empty_is_noop() {
        let pool = create_test_pool().await;
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.unwrap();
        repository
            .log_requests_batch(&[])
            .await
            .expect("empty batch ok");
        let all = repository.query_logs(&LogFilter::default()).await.unwrap();
        assert!(all.is_empty());
    }

    /// 全新文件路径自举建库（mode=rwc 回归：sqlx 默认不创建不存在的库文件，
    /// 此前仅 :memory: 测试覆盖不到该路径，实机首次启动在全新路径上失败）
    #[tokio::test]
    async fn test_database_new_creates_missing_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("nx-db-create-{}-{nanos}", std::process::id()));
        let db_path = dir.join("fresh").join("llm_proxy.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        assert!(!db_path.exists(), "前置：库文件尚不存在");

        let db = Database::new(db_path.to_str().unwrap(), 1).await;

        assert!(db.is_ok(), "全新路径应能自举建库: {:?}", db.err());
        assert!(db_path.exists(), "Database::new 之后库文件应已创建");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
