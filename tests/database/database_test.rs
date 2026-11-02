#![cfg(feature = "database")]

// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use fluxgate::config::DatabaseConfig;
use fluxgate::database::resolve_database_url;

/// 测试路径 → 解析后 URL（与 bootstrap 同一解析出口，sqlite 回退分支）。
/// 文件作用域：batch_tx / database_gap 等兄弟模块经 `crate::resolve_sqlite_url` 复用。
fn resolve_sqlite_url(path: &str) -> String {
    resolve_database_url(&DatabaseConfig {
        path: path.to_string(),
        pool_size: 1,
        url: String::new(),
    })
    .expect("resolve sqlite url")
}

#[cfg(test)]
mod tests {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{Database, LogFilter, Repository, RequestLog};
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
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
            };
            repository.log_request(&log).await.unwrap();
        }

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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
            };

            handles.push(tokio::spawn(async move { repo.log_request(&log).await }));
        }

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
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
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
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
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
                session_key: None,
                context_window_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                upstream_key_fingerprint: None,
            };

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

// 单事务批量写入（Repository::log_requests_batch）

#[cfg(test)]
mod batch_tx {
    use crate::resolve_sqlite_url;
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{Database, LogFilter, Repository, RequestLog};
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
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        }
    }

    /// tests ①：批量 100 条单事务写入 → query_logs 恰 100 条
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

    /// 全新文件路径自举建库（mode=rwc：sqlx 默认不创建不存在的库文件）。
    #[tokio::test]
    async fn test_database_new_creates_missing_file() {
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().to_path_buf();
        let db_path = dir.join("fresh").join("llm_proxy.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        assert!(!db_path.exists(), "前置：库文件尚不存在");

        let db = Database::new(&resolve_sqlite_url(db_path.to_str().unwrap()), 1).await;

        assert!(db.is_ok(), "全新路径应能自举建库: {:?}", db.err());
        assert!(db_path.exists(), "Database::new 之后库文件应已创建");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// 覆盖补缺：Token 预聚合器（record / flush / flush worker / handle）

#[cfg(test)]
mod preagg {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{Repository, TokenPreAggregator, TokenPreAggregatorHandle};
    use fluxgate::protocol::TokenUsage;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

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

    async fn initialized_repository() -> Arc<Repository> {
        let pool = create_test_pool().await;
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.expect("init schema");
        repository
    }

    fn usage(prompt: u32, completion: u32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            ..Default::default()
        }
    }

    /// record 聚合语义：同 (vendor, model) 计数累加，flush 后按行落库；
    /// 空缓冲的二次 flush 是 no-op；UPSERT 键不产生重复行。
    #[tokio::test]
    async fn record_accumulates_and_flush_persists_token_usage() {
        let repo = initialized_repository().await;
        let agg = TokenPreAggregator::new(repo.clone(), Duration::from_secs(60));

        agg.record("openai_main", "gpt-4", 0.5, &usage(100, 50))
            .await;
        agg.record("openai_main", "gpt-4", 0.5, &usage(10, 5)).await;
        agg.record("anthropic_main", "claude", 0.25, &usage(7, 3))
            .await;

        agg.flush().await.expect("flush ok");
        // 空缓冲 flush：返回 Ok 且不改数据
        agg.flush().await.expect("empty flush ok");

        let mut stats = repo.get_token_stats(None).await.expect("query ok");
        stats.sort_by(|a, b| a.vendor.cmp(&b.vendor));
        assert_eq!(stats.len(), 2, "两个 (vendor, model) 键 → 两行");

        let openai = stats.iter().find(|s| s.vendor == "openai_main").unwrap();
        assert_eq!(openai.model, "gpt-4");
        assert_eq!(openai.request_count, 2);
        assert_eq!(openai.prompt_tokens, 110);
        assert_eq!(openai.completion_tokens, 55);
        assert_eq!(openai.total_tokens, 165);
        assert!((openai.cost_usd_sum - 1.0).abs() < 1e-9, "成本须累加");

        let anthropic = stats.iter().find(|s| s.vendor == "anthropic_main").unwrap();
        assert_eq!(anthropic.request_count, 1);
        assert_eq!(anthropic.total_tokens, 10);

        // 同键再记一笔再 flush → ON CONFLICT 续加（既不覆盖也不新增行）
        agg.record("openai_main", "gpt-4", 0.1, &usage(1, 1)).await;
        agg.flush().await.expect("second flush ok");
        let stats = repo
            .get_token_stats(Some("openai_main"))
            .await
            .expect("query ok");
        assert_eq!(stats.len(), 1, "UPSERT 必须落同一行");
        assert_eq!(stats[0].request_count, 3);
        assert_eq!(stats[0].total_tokens, 167);
        assert!((stats[0].cost_usd_sum - 1.1).abs() < 1e-9);
    }

    /// 未初始化 schema（token_usage 表缺失）→ flush 显式报错，不吞错。
    #[tokio::test]
    async fn flush_without_schema_returns_error() {
        let pool = create_test_pool().await;
        let repo = Arc::new(Repository::new(pool));
        let agg = TokenPreAggregator::new(repo, Duration::from_secs(60));
        agg.record_raw("v", "m", 1, 2, 3, 0.01).await;
        assert!(agg.flush().await.is_err(), "无表必须报错（错误显性化）");
    }

    /// 停机补 flush：shutdown 预置 true → 首个 tick 立即触发，缓冲数据
    /// 落库后 worker 退出。
    #[tokio::test]
    async fn flush_worker_final_flush_on_immediate_shutdown() {
        let repo = initialized_repository().await;
        let agg = Arc::new(TokenPreAggregator::new(
            repo.clone(),
            Duration::from_secs(60),
        ));
        agg.record("v", "m", 0.0, &usage(5, 5)).await;

        let shutdown = Arc::new(AtomicBool::new(true));
        agg.clone().start_flush_worker(shutdown).await;

        let stats = repo.get_token_stats(None).await.expect("query ok");
        assert_eq!(stats.len(), 1, "停机补 flush 必须把缓冲落库");
        assert_eq!(stats[0].total_tokens, 10);
    }

    /// 周期 flush：间隔 20ms，缓冲在停机前已落库；置位后 worker 在
    /// 一个周期内退出。
    #[tokio::test]
    async fn flush_worker_periodic_flush_then_shutdown_exit() {
        let repo = initialized_repository().await;
        let agg = Arc::new(TokenPreAggregator::new(
            repo.clone(),
            Duration::from_millis(20),
        ));
        agg.record("v", "m", 0.0, &usage(5, 5)).await;

        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_flag = shutdown.clone();
        let worker_agg = agg.clone();
        let worker = tokio::spawn(async move { worker_agg.start_flush_worker(worker_flag).await });

        tokio::time::sleep(Duration::from_millis(150)).await;
        let stats = repo.get_token_stats(None).await.expect("query ok");
        assert_eq!(stats.len(), 1, "周期 flush 必须在停机前落库");

        shutdown.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("置位后 worker 必须退出")
            .expect("worker 不 panic");
    }

    /// flush 失败不中止 worker：无 schema 下周期 flush 报错只记日志，
    /// 置位停机后补一次（仍失败）并正常退出——观测不得拖垮执行路径。
    #[tokio::test]
    async fn flush_worker_survives_flush_failure_until_shutdown() {
        let pool = create_test_pool().await;
        let repo = Arc::new(Repository::new(pool));
        let agg = Arc::new(TokenPreAggregator::new(repo, Duration::from_millis(20)));
        agg.record("v", "m", 0.0, &usage(1, 1)).await;

        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_flag = shutdown.clone();
        let worker_agg = agg.clone();
        let worker = tokio::spawn(async move { worker_agg.start_flush_worker(worker_flag).await });

        tokio::time::sleep(Duration::from_millis(120)).await;
        shutdown.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("flush 失败不得中止 worker，置位后必须退出")
            .expect("worker 不 panic");
    }

    /// 句柄委托：record / record_raw / start_flush_worker（停机置位 →
    /// 补最终 flush）三路径与直连聚合器行为一致。
    #[tokio::test]
    async fn handle_delegates_record_and_flush_worker() {
        let repo = initialized_repository().await;
        let agg = Arc::new(TokenPreAggregator::new(
            repo.clone(),
            Duration::from_secs(60),
        ));
        let handle = TokenPreAggregatorHandle::new(agg);

        handle.record("v", "m", 0.5, &usage(10, 5)).await;
        handle.record_raw("v", "m", 1, 1, 2, 0.5).await;

        let shutdown = Arc::new(AtomicBool::new(true));
        handle.start_flush_worker(shutdown);
        tokio::time::sleep(Duration::from_millis(100)).await;

        let stats = repo.get_token_stats(None).await.expect("query ok");
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].request_count, 2, "record 与 record_raw 同键累加");
        assert_eq!(stats[0].prompt_tokens, 11);
        assert_eq!(stats[0].completion_tokens, 6);
        assert_eq!(stats[0].total_tokens, 17);
        assert!((stats[0].cost_usd_sum - 1.0).abs() < 1e-9);
    }
}

// 覆盖补缺：Repository 迁移 / 批量回滚 / 厂商状态读取

#[cfg(test)]
mod repository_gap {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{LogFilter, Repository, RequestLog};
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

    async fn initialized_repository() -> Arc<Repository> {
        let pool = create_test_pool().await;
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.expect("init schema");
        repository
    }

    fn log_with_request_id(request_id: &str) -> RequestLog {
        RequestLog {
            id: None,
            request_id: request_id.to_string(),
            timestamp: 1_700_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "{}".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            total_tokens: Some(2),
            duration_ms: Some(5),
            cost_usd: Some(0.5),
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        }
    }

    /// V0.1 旧库迁移：缺 cost_usd / cost_usd_sum 列时 initialize 补列，
    /// 旧行取列默认值且可正常读回。
    #[tokio::test]
    async fn initialize_adds_missing_cost_columns_to_legacy_schema() {
        let pool = create_test_pool().await;
        {
            let session = pool.get_session("admin").await.unwrap();
            session
                .execute_raw_ddl(
                    "CREATE TABLE request_logs (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        request_id TEXT UNIQUE NOT NULL,
                        timestamp INTEGER NOT NULL,
                        endpoint TEXT NOT NULL,
                        input_format TEXT NOT NULL,
                        abstract_model TEXT,
                        messages TEXT NOT NULL,
                        actual_vendor TEXT NOT NULL,
                        actual_model TEXT NOT NULL,
                        routing_strategy TEXT NOT NULL,
                        status_code INTEGER,
                        response TEXT,
                        error_message TEXT,
                        prompt_tokens INTEGER,
                        completion_tokens INTEGER,
                        total_tokens INTEGER,
                        duration_ms INTEGER
                    )",
                )
                .await
                .unwrap();
            session
                .execute_raw(
                    "INSERT INTO request_logs (
                        request_id, timestamp, endpoint, input_format, messages,
                        actual_vendor, actual_model, routing_strategy, status_code
                     ) VALUES ('legacy-1', 1700000000, '/v1/chat/completions', 'openai',
                               '{}', 'openai_main', 'gpt-4', 'direct', 200)",
                )
                .await
                .unwrap();
        }

        let repository = Repository::new(pool);
        repository.initialize().await.expect("旧库迁移必须成功");

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].request_id, "legacy-1");
        assert_eq!(
            logs[0].cost_usd,
            Some(0.0),
            "迁移补列后旧行必须取列默认值 0.0"
        );
    }

    /// 批量写入的 UNIQUE 冲突 → 整批回滚：显式报错且不留下半批。
    #[tokio::test]
    async fn batch_rollback_on_constraint_violation_leaves_no_partial_batch() {
        let repo = initialized_repository().await;
        let logs = vec![
            log_with_request_id("dup"),
            log_with_request_id("dup"),
            log_with_request_id("other-3"),
        ];

        let result = repo.log_requests_batch(&logs).await;
        assert!(result.is_err(), "UNIQUE 冲突必须显式报错");

        let all = repo
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert!(all.is_empty(), "事务回滚不得留下半批: {all:?}");
    }

    /// vendor_status 读取映射：window_reset_at 的 NULL / 有值两形态，
    /// 数值字段逐一核对。
    #[tokio::test]
    async fn vendor_status_rows_are_mapped_with_optional_reset_at() {
        let repo = initialized_repository().await;
        {
            let session = repo.pool.get_session("admin").await.unwrap();
            session
                .execute_raw(
                    "INSERT INTO vendor_status (vendor, status, last_updated, window_used_tokens, window_reset_at, error_count)
                     VALUES ('vendor_a', 'healthy', 1700000000, 500, NULL, 2)",
                )
                .await
                .unwrap();
            session
                .execute_raw(
                    "INSERT INTO vendor_status (vendor, status, last_updated, window_used_tokens, window_reset_at, error_count)
                     VALUES ('vendor_b', 'rate_limited', 1700000060, 9000, 1700000120, 5)",
                )
                .await
                .unwrap();
        }

        let mut rows = repo.get_all_vendor_status().await.expect("query ok");
        rows.sort_by(|a, b| a.vendor.cmp(&b.vendor));
        assert_eq!(rows.len(), 2);

        assert_eq!(rows[0].vendor, "vendor_a");
        assert_eq!(rows[0].status, "healthy");
        assert_eq!(rows[0].last_updated, 1_700_000_000);
        assert_eq!(rows[0].window_used_tokens, 500);
        assert_eq!(rows[0].window_reset_at, None, "NULL 须映射为 None");
        assert_eq!(rows[0].error_count, 2);

        assert_eq!(rows[1].vendor, "vendor_b");
        assert_eq!(rows[1].window_reset_at, Some(1_700_000_120));
        assert_eq!(rows[1].error_count, 5);
    }
}

// 覆盖补缺：AsyncLogger 关闭/背压/缓冲阈值/落库失败路径

#[cfg(test)]
mod async_logger_gap {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{AsyncLogger, AsyncLoggerHandle, LogFilter, Repository, RequestLog};
    use fluxgate::error::FluxgateError;
    use fluxgate::i18n::t;
    use std::sync::Arc;
    use std::time::Duration;

    async fn initialized_repository() -> Arc<Repository> {
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
        let pool = DbPool::with_config(config).await.unwrap();
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.expect("init schema");
        repository
    }

    fn sample_log(request_id: &str) -> RequestLog {
        RequestLog {
            id: None,
            request_id: request_id.to_string(),
            timestamp: 1_718_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "Hello".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "weighted".to_string(),
            status_code: Some(200),
            response: Some("Hi".to_string()),
            error_message: None,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            duration_ms: Some(120),
            cost_usd: Some(0.001),
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        }
    }

    /// shutdown 后 log 拒绝写入：Database 错误 + shutdown 语义消息。
    #[tokio::test]
    async fn log_after_shutdown_is_rejected_with_database_error() {
        let repo = initialized_repository().await;
        let logger = AsyncLogger::new(repo, 10, Duration::from_secs(60));
        logger.shutdown().await.expect("首次 shutdown 成功");

        let err = logger.log(sample_log("after-shutdown")).unwrap_err();
        match err {
            FluxgateError::Database { message } => {
                assert_eq!(message, t("database-logger-shutdown"));
            }
            other => panic!("Expected Database error, got {other:?}"),
        }
    }

    /// 背压：容量 1 连发 3 条（同步调用无让出点）→ 1 条入队、恰 2 条丢弃
    /// 计数、全部立即返回 Ok（绝不阻塞响应路径）。
    #[tokio::test]
    async fn full_channel_drops_and_counts_without_blocking() {
        let repo = initialized_repository().await;
        let logger = Arc::new(AsyncLogger::new(repo, 1, Duration::from_secs(60)));

        let start = std::time::Instant::now();
        let r1 = logger.log(sample_log("a"));
        let r2 = logger.log(sample_log("b"));
        let r3 = logger.log(sample_log("c"));
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(100),
            "log() 不得阻塞: {elapsed:?}"
        );
        assert!(r1.is_ok() && r2.is_ok() && r3.is_ok(), "丢弃不是错误");
        assert_eq!(
            logger.dropped_logs(),
            2,
            "单线程同步连发：恰 1 条在 channel、2 条丢弃计数"
        );

        logger.shutdown().await.expect("shutdown ok");
    }

    /// 二次 shutdown：writer 已退出（receiver drop）→ send 失败显式返回
    /// channel-closed 错误，调用方以 Ok/Err 判断首闭。
    #[tokio::test]
    async fn second_shutdown_returns_channel_closed_error() {
        let repo = initialized_repository().await;
        let logger = AsyncLogger::new(repo, 10, Duration::from_secs(60));

        logger.shutdown().await.expect("首次 shutdown 成功");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let err = logger.shutdown().await.unwrap_err();
        match err {
            FluxgateError::Database { message } => {
                assert_eq!(message, t("database-logger-channel-closed"));
            }
            other => panic!("Expected Database error, got {other:?}"),
        }
    }

    /// 缓冲达到 100 条阈值立即落库（不等周期 tick）：101 条全部可查回。
    #[tokio::test]
    async fn buffer_full_triggers_immediate_flush() {
        let repo = initialized_repository().await;
        let logger = AsyncLogger::new(repo.clone(), 200, Duration::from_secs(60));

        for i in 0..101 {
            logger
                .log(sample_log(&format!("thresh-{i:03}")))
                .expect("log ok");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        logger.shutdown().await.expect("shutdown ok");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repo
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 101, "阈值 flush + 收尾 flush 后 101 条全在库");
    }

    /// 落库失败（无 request_logs 表）只记日志不得拖垮 writer：shutdown
    /// 流程照常走完，且关闭后写入被显式拒绝。
    #[tokio::test]
    async fn flush_failure_does_not_kill_writer() {
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
        let pool = DbPool::with_config(config).await.unwrap();
        // 故意不 initialize：request_logs 表不存在 → 批量 flush 必失败
        let repo = Arc::new(Repository::new(pool));
        let logger = AsyncLogger::new(repo, 10, Duration::from_secs(60));

        logger.log(sample_log("f1")).expect("入队 ok");
        logger.log(sample_log("f2")).expect("入队 ok");
        logger.shutdown().await.expect("落库失败不阻断 shutdown");
        assert!(logger.log(sample_log("f3")).is_err(), "关闭后写入被拒绝");
    }

    /// 句柄端到端：log → dropped_logs 读数 → shutdown → 拒绝写入，
    /// 数据落库可查。
    #[tokio::test]
    async fn handle_wraps_logger_end_to_end() {
        let repo = initialized_repository().await;
        let handle =
            AsyncLoggerHandle::new(AsyncLogger::new(repo.clone(), 10, Duration::from_secs(60)));

        handle.log(sample_log("h-1")).expect("log ok");
        assert_eq!(handle.dropped_logs(), 0, "未满槽无丢弃");
        handle.shutdown().await.expect("shutdown ok");
        assert!(handle.log(sample_log("h-2")).is_err(), "关闭后写入被拒绝");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let logs = repo
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].request_id, "h-1");
    }
}

// 覆盖补缺：Database::new 自举与父路径非法

#[cfg(test)]
mod database_gap {
    use crate::resolve_sqlite_url;
    use fluxgate::database::{Database, LogFilter, RequestLog};
    use fluxgate::error::FluxgateError;

    fn sample_log() -> RequestLog {
        RequestLog {
            id: None,
            request_id: "db-gap-1".to_string(),
            timestamp: 1_700_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: None,
            messages: "{}".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            duration_ms: None,
            cost_usd: None,
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        }
    }

    /// 内存库自举：Database::new 建连 + 建表，repository 可直接读写，
    /// close 幂等无副作用。
    #[tokio::test]
    async fn database_new_memory_bootstrap_and_repository_accessor() {
        let db = Database::new(&resolve_sqlite_url(":memory:"), 1)
            .await
            .expect("内存库自举成功");
        let repo = db.repository();

        repo.log_request(&sample_log()).await.expect("落库 ok");
        let logs = repo
            .query_logs(&LogFilter::default())
            .await
            .expect("query ok");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].request_id, "db-gap-1");

        db.close().await;
    }

    /// 父路径是普通文件时 create_dir_all 失败 → Database 错误显性化，
    /// 不得 panic 或默认成功。
    #[tokio::test]
    async fn database_new_fails_when_parent_path_is_a_file() {
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let file = tmpdir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();

        let db_path = file.join("sub").join("db.sqlite");
        let result =
            Database::new(&resolve_sqlite_url(db_path.to_string_lossy().as_ref()), 1).await;

        match result {
            Err(FluxgateError::Database { .. }) => {}
            Err(other) => panic!("Expected Database error, got {other:?}"),
            Ok(_) => panic!("父路径是文件时必须失败"),
        }

        let _ = std::fs::remove_file(&file);
    }

    /// 库路径本身是已存在目录时 SQLite 无法打开 → 连接失败映射为
    /// Database 错误显性化（mode=rwc 也建不出目录形态的库文件）。
    #[tokio::test]
    async fn database_new_fails_when_db_path_is_a_directory() {
        let tmpdir = tempfile::tempdir().expect("tempdir creatable");
        let dir = tmpdir.path().join("as-dir");
        std::fs::create_dir_all(&dir).unwrap();

        let result = Database::new(&resolve_sqlite_url(dir.to_str().unwrap()), 1).await;
        match result {
            Err(FluxgateError::Database { .. }) => {}
            Err(other) => panic!("Expected Database error, got {other:?}"),
            Ok(_) => panic!("库路径是目录时必须失败"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// 成本分账落库（V2 分账聚合 + /admin/cost 查询面）

#[cfg(test)]
mod cost_ledger {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{CostGroupBy, KeyedUsage, Repository, TokenPreAggregator};
    use std::sync::Arc;
    use std::time::Duration;

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

    async fn initialized_repository() -> Arc<Repository> {
        let pool = create_test_pool().await;
        let repository = Arc::new(Repository::new(pool));
        repository.initialize().await.expect("init schema");
        repository
    }

    /// 分账行构造（token 与成本成固定比例，断言聚焦聚合语义）。
    fn keyed(vendor: &str, model: &str, hash: &str, prompt: u32, completion: u32) -> KeyedUsage {
        KeyedUsage {
            vendor: vendor.into(),
            model: model.into(),
            api_key_hash: hash.into(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            input_cost: prompt as f64 * 0.01,
            output_cost: completion as f64 * 0.02,
            cache_read_cost: 0.0,
            cache_creation_cost: 0.0,
        }
    }

    /// 全字段分账行构造（cache 类单独覆盖）。
    fn keyed_full(mut usage: KeyedUsage, cache_read: u32, cache_creation: u32) -> KeyedUsage {
        usage.cache_read_tokens = cache_read;
        usage.cache_creation_tokens = cache_creation;
        usage
    }

    async fn seed() -> Arc<Repository> {
        let repo = initialized_repository().await;
        let agg = TokenPreAggregator::new(repo.clone(), Duration::from_secs(60));
        // vendor-a/m1/hash-h1: 10+5(+2 cache_read)
        agg.record_keyed(&keyed_full(keyed("vendor-a", "m1", "h1", 10, 5), 2, 0))
            .await;
        agg.record_keyed(&keyed("vendor-a", "m1", "h1", 4, 1)).await;
        // vendor-a/m2/hash-h1：聚合键不同（model 不同）
        agg.record_keyed(&keyed("vendor-a", "m2", "h1", 7, 3)).await;
        // vendor-b/m1/hash-h2：hash 不同即分行
        agg.record_keyed(&keyed("vendor-b", "m1", "h2", 100, 50))
            .await;
        agg.flush().await.expect("flush ok");
        repo
    }

    /// 分账聚合：同 (vendor, model, hash) 同小时合并；四类 token 与分项
    /// 成本各自累加；维度分组求和正确。
    #[tokio::test]
    async fn cost_ledger_aggregates_by_key_and_dimension() {
        let repo = seed().await;

        let by_vendor = repo
            .query_cost_usage(None, None, CostGroupBy::Vendor, None)
            .await
            .expect("query ok");
        assert_eq!(by_vendor.len(), 2, "两 vendor → 两行");
        let a = by_vendor
            .iter()
            .find(|r| r.vendor.as_deref() == Some("vendor-a"))
            .unwrap();
        assert_eq!(a.model, None);
        assert_eq!(a.request_count, 3);
        assert_eq!(a.input_tokens, 10 + 4 + 7);
        assert_eq!(a.output_tokens, 5 + 1 + 3);
        assert_eq!(a.cache_read_tokens, 2);
        assert_eq!(a.cache_creation_tokens, 0);
        assert!((a.input_cost - 0.21).abs() < 1e-9);
        assert!((a.output_cost - 0.18).abs() < 1e-9);
        assert!((a.total_cost - 0.39).abs() < 1e-9);

        let by_model = repo
            .query_cost_usage(None, None, CostGroupBy::Model, None)
            .await
            .expect("query ok");
        assert_eq!(by_model.len(), 2, "m1/m2 → 两行");
        let m1 = by_model
            .iter()
            .find(|r| r.model.as_deref() == Some("m1"))
            .unwrap();
        assert_eq!(m1.vendor, None);
        assert_eq!(m1.request_count, 3);
        assert_eq!(m1.input_tokens, 10 + 4 + 100);
    }

    /// hash 过滤与日期窗口：h1 仅见自有行；全集外窗口返回空数组。
    #[tokio::test]
    async fn cost_ledger_filters_by_hash_and_window() {
        let repo = seed().await;

        let h1 = repo
            .query_cost_usage(None, None, CostGroupBy::Vendor, Some("h1"))
            .await
            .expect("query ok");
        assert_eq!(h1.len(), 1, "h1 只关联 vendor-a");
        assert_eq!(h1[0].vendor.as_deref(), Some("vendor-a"));

        let wide = repo
            .query_cost_usage(
                Some("2000-01-01"),
                Some("2999-12-31"),
                CostGroupBy::Vendor,
                None,
            )
            .await
            .expect("query ok");
        assert_eq!(wide.len(), 2, "全集窗口全见");

        let empty = repo
            .query_cost_usage(
                Some("2000-01-01"),
                Some("2000-01-02"),
                CostGroupBy::Vendor,
                None,
            )
            .await
            .expect("query ok");
        assert!(empty.is_empty(), "空窗口返回空数组而非错误");

        let no_hash = repo
            .query_cost_usage(None, None, CostGroupBy::Model, Some("ghost"))
            .await
            .expect("query ok");
        assert!(no_hash.is_empty(), "无匹配 hash 返回空数组");
    }

    /// 兼容面：V2 行经 get_token_stats 可读，旧字段为四类映射
    /// （prompt=input、completion=output、total=四类和、cost=四项和）。
    #[tokio::test]
    async fn token_stats_maps_v2_rows_to_legacy_fields() {
        let repo = seed().await;
        let mut stats = repo.get_token_stats(None).await.expect("query ok");
        stats.sort_by(|a, b| {
            (&a.vendor, &a.model, &a.api_key_hash).cmp(&(&b.vendor, &b.model, &b.api_key_hash))
        });
        assert_eq!(stats.len(), 3, "三聚合键 → 三行");
        let row = stats
            .iter()
            .find(|r| r.vendor == "vendor-a" && r.model == "m1")
            .unwrap();
        assert_eq!(row.api_key_hash, "h1");
        assert_eq!(row.prompt_tokens, 14);
        assert_eq!(row.completion_tokens, 6);
        assert_eq!(row.total_tokens, 14 + 6 + 2);
        assert!((row.cost_usd_sum - 0.26).abs() < 1e-9);
        assert_eq!(row.cache_read_tokens, 2);
    }
}

/// 会话/上下文四列落库面（V5 列 + 写入路径）。
#[cfg(test)]
mod usage_observability_columns {
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::database::{Repository, RequestLog};
    use fluxgate::storage::LogFilter;

    async fn pool() -> DbPool {
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
        DbPool::with_config(config).await.expect("pool")
    }

    /// 会话聚合：3 会话 7 请求，成本/token/首末时间聚合正确，
    /// 标题取首条用户消息并截断 80 字符；NULL 会话不参与。
    #[tokio::test]
    async fn sessions_group_by_with_cost_and_title() {
        let pool = pool().await;
        let repo = Repository::new(pool);
        repo.initialize().await.expect("schema");

        let long_title = "x".repeat(200);
        let rows: Vec<(&str, i64, &str, i64, i64)> = vec![
            // (session, ts, title, prompt, cache_read)
            ("s-a", 100, "first a", 100, 10),
            ("s-a", 101, "later a", 200, 20),
            ("s-a", 102, "later a2", 300, 30),
            ("s-b", 200, long_title.as_str(), 400, 40),
            ("s-b", 201, "later b", 500, 50),
            ("s-c", 300, "first c", 600, 60),
            ("s-c", 301, "later c", 700, 70),
        ];
        for (session, ts, title, prompt, cache_read) in rows {
            let log = RequestLog {
                id: None,
                request_id: format!("{session}-{ts}"),
                timestamp: ts,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("m".to_string()),
                messages: format!(r#"[{{"role":"user","content":"{title}"}}]"#),
                actual_vendor: "v".to_string(),
                actual_model: "m".to_string(),
                routing_strategy: "weighted".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                prompt_tokens: Some(prompt as i32),
                completion_tokens: Some(1),
                total_tokens: Some(prompt as i32 + 1),
                duration_ms: Some(1),
                cost_usd: Some(0.5),
                session_key: Some(session.to_string()),
                context_window_tokens: Some(1000),
                cache_read_tokens: Some(cache_read),
                cache_write_tokens: Some(1),
                upstream_key_fingerprint: None,
            };
            repo.log_request(&log).await.expect("write");
        }
        // NULL 会话：不参与聚合
        let mut orphan = RequestLog {
            id: None,
            request_id: "orphan".to_string(),
            timestamp: 999,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("m".to_string()),
            messages: "[]".to_string(),
            actual_vendor: "v".to_string(),
            actual_model: "m".to_string(),
            routing_strategy: "weighted".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            total_tokens: Some(2),
            duration_ms: Some(1),
            cost_usd: Some(1.0),
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        };
        orphan.session_key = None;
        repo.log_request(&orphan).await.expect("write orphan");

        let sessions = repo.query_sessions(0, 50).await.expect("sessions");
        assert_eq!(sessions.len(), 3, "三个会话，NULL 会话排除");

        let a = sessions.iter().find(|s| s.session_key == "s-a").unwrap();
        assert_eq!(a.request_count, 3);
        assert_eq!(a.prompt_tokens, 600);
        assert_eq!(a.cache_read_tokens, 60);
        assert!((a.total_cost - 1.5).abs() < 1e-9);
        assert_eq!(a.first_at, 100, "首条时间");
        assert_eq!(a.last_at, 102, "末条时间");
        assert_eq!(a.title, "first a", "标题取首条用户消息");

        let b = sessions.iter().find(|s| s.session_key == "s-b").unwrap();
        assert_eq!(b.request_count, 2);
        assert_eq!(
            b.title.chars().count(),
            80,
            "超长标题截断 80 字符，实际 {}",
            b.title.chars().count()
        );

        // limit 生效：上限取前 1 条（按 last_at DESC）
        let one = repo.query_sessions(0, 1).await.expect("limit");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].session_key, "s-c", "last_at 最新优先");
    }

    /// 上下文分桶：占用率 10/40/60/90/120% 五行 → 按 20/40/60/80% 边界
    /// 落桶（b0<20%、b1 20-40%、b2 40-60%、b3 60-80%、b4>=80%）。
    /// 分子口径为 `prompt + cache_write`（不含 cache_read，见
    /// `INPUT_TOKENS_EXPR` 注释），故构造时把目标入窗量全放 prompt。
    #[tokio::test]
    async fn context_stats_buckets_and_cache_share() {
        let pool = pool().await;
        let repo = Repository::new(pool);
        repo.initialize().await.expect("schema");

        // window=1000：input=100 → 10%，400 → 40%，600 → 60%，900 → 90%，1200 → 120%
        for (i, input) in [100i64, 400, 600, 900, 1200].into_iter().enumerate() {
            let log = RequestLog {
                id: None,
                request_id: format!("ctx-{i}"),
                timestamp: 100 + i as i64,
                endpoint: "/v1/chat/completions".to_string(),
                input_format: "openai".to_string(),
                abstract_model: Some("ctx-m".to_string()),
                messages: "[]".to_string(),
                actual_vendor: "v".to_string(),
                actual_model: "ctx-m".to_string(),
                routing_strategy: "weighted".to_string(),
                status_code: Some(200),
                response: None,
                error_message: None,
                // 分子 = prompt + cache_write；把目标入窗量全放 prompt，
                // cache_read 单独给值以验证它不参与分子。
                prompt_tokens: Some(input as i32),
                completion_tokens: Some(1),
                total_tokens: Some((input + 1) as i32),
                duration_ms: Some(1),
                cost_usd: Some(0.0),
                session_key: Some(format!("s-{i}")),
                context_window_tokens: Some(1000),
                cache_read_tokens: Some(50),
                cache_write_tokens: Some(0),
                upstream_key_fingerprint: None,
            };
            repo.log_request(&log).await.expect("write");
        }

        let stats = repo.query_context_stats(0).await.expect("stats");
        assert_eq!(stats.len(), 1, "单模型一行");
        let row = &stats[0];
        assert_eq!(row.model, "ctx-m");
        assert_eq!(row.request_count, 5);
        assert_eq!(
            row.bucket_counts,
            [1, 0, 1, 1, 2],
            "按 20/40/60/80% 边界：10%→b0、40%→b2、60%→b3、90%+120%→b4，实际 {:?}",
            row.bucket_counts
        );
        assert!(
            (row.avg_occupancy - 0.64).abs() < 1e-9,
            "平均占用率 (100+400+600+900+1200)/5/1000 = 0.64，实际 {}",
            row.avg_occupancy
        );
        // 缓存份额 = cache_read / input_tokens；input 总量 100+400+600+900+1200 = 3200
        let expected_share = 250.0 / 3200.0;
        assert!(
            (row.cache_read_share - expected_share).abs() < 1e-9,
            "缓存读份额 = 5×50/3200，实际 {}",
            row.cache_read_share
        );
    }

    /// 会话与上下文四列：有值原样落库、无值为 NULL；查询回读一致。
    #[tokio::test]
    async fn request_log_session_and_context_columns_roundtrip() {
        let pool = pool().await;
        let repo = Repository::new(pool);
        repo.initialize().await.expect("schema");

        let build = |request_id: &str, ts: i64| RequestLog {
            id: None,
            request_id: request_id.to_string(),
            timestamp: ts,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "{}".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            duration_ms: Some(20),
            cost_usd: None,
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: None,
        };

        let mut with_session = build("req-session", 1_700_000_000);
        with_session.session_key = Some("sess-abc".to_string());
        with_session.context_window_tokens = Some(200_000);
        with_session.cache_read_tokens = Some(1024);
        with_session.cache_write_tokens = Some(256);
        repo.log_request(&with_session).await.expect("写有值行");

        let without = build("req-null", 1_700_000_001);
        repo.log_request(&without).await.expect("写无值行");

        let rows = repo
            .query_logs(&LogFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("query");
        let hit = rows
            .iter()
            .find(|r| r.request_id == "req-session")
            .expect("有值行可读");
        assert_eq!(hit.session_key.as_deref(), Some("sess-abc"));
        assert_eq!(hit.context_window_tokens, Some(200_000));
        assert_eq!(hit.cache_read_tokens, Some(1024));
        assert_eq!(hit.cache_write_tokens, Some(256));

        let miss = rows
            .iter()
            .find(|r| r.request_id == "req-null")
            .expect("无值行可读");
        assert!(miss.session_key.is_none(), "无会话键必须 NULL");
        assert!(miss.context_window_tokens.is_none(), "未登记目录必须 NULL");
        assert!(miss.cache_read_tokens.is_none());
        assert!(miss.cache_write_tokens.is_none());
    }
}

/// 会话粘性落盘（cache-economy-affinity V6）：建表幂等 + UPSERT 覆盖 +
/// 未过期读取上限与租户隔离 + 过期清理。
#[cfg(test)]
mod affinity_sticks {
    use fluxgate::storage::AffinityStick;

    async fn pool() -> dbnexus::DbPool {
        dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    fn stick(tenant: &str, session: &str, vendor: &str, expires_at: i64) -> AffinityStick {
        AffinityStick {
            tenant: tenant.into(),
            session_key: session.into(),
            vendor_id: vendor.into(),
            worth_tokens: 2_048,
            expires_at,
        }
    }

    /// 迁移建表 + 重跑幂等（V6 记账、affinity_sticks 可写可读）。
    #[tokio::test]
    async fn v6_migration_creates_affinity_sticks_idempotently() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("首次迁移");
        repo.initialize().await.expect("重跑幂等");

        repo.upsert_affinity_sticks(&[stick("", "s1", "v-a", 1_700_000_000)])
            .await
            .expect("UPSERT");
        let rows = repo.read_affinity_sticks(0, 10).await.expect("read");
        assert_eq!(rows.len(), 1, "V6 表必须可写可读");
        assert_eq!(rows[0].vendor_id, "v-a");
    }

    /// UPSERT 覆盖：同 (tenant, session) 保留最后写入的 vendor；
    /// 租户键与 legacy 键互不可见（复合主键）。
    #[tokio::test]
    async fn upsert_overwrites_and_tenant_scopes() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("init");

        repo.upsert_affinity_sticks(&[
            stick("", "s1", "v-a", 100),
            stick("", "s1", "v-b", 200),
            stick("tenant-a", "s1", "v-c", 300),
        ])
        .await
        .expect("UPSERT");

        let rows = repo.read_affinity_sticks(0, 10).await.expect("read");
        assert_eq!(rows.len(), 2, "legacy 覆盖为单行 + 租户行独立");
        let legacy = rows
            .iter()
            .find(|r| r.tenant.is_empty())
            .expect("legacy 行");
        assert_eq!(legacy.vendor_id, "v-b", "保留最后写入");
        assert_eq!(legacy.expires_at, 200);
        let tenant_row = rows
            .iter()
            .find(|r| r.tenant == "tenant-a")
            .expect("租户行");
        assert_eq!(tenant_row.vendor_id, "v-c");
    }

    /// 未过期读取：过期行排除、LIMIT 下推、按 expires_at 降序（最新优先）。
    #[tokio::test]
    async fn read_unexpired_orders_desc_and_limits() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("init");

        let rows: Vec<AffinityStick> = (0..5)
            .map(|i| stick("", &format!("s{i}"), "v-a", 1_700_000_000 + i))
            .chain(std::iter::once(stick("", "expired", "v-b", 50)))
            .collect();
        repo.upsert_affinity_sticks(&rows).await.expect("UPSERT");

        let fresh = repo.read_affinity_sticks(100, 10).await.expect("read");
        assert_eq!(fresh.len(), 5, "过期行不入恢复集");
        assert_eq!(fresh[0].expires_at, 1_700_000_004, "降序：最新优先");
        assert_eq!(fresh.last().expect("非空").expires_at, 1_700_000_000);

        let limited = repo.read_affinity_sticks(100, 2).await.expect("read");
        assert_eq!(limited.len(), 2, "LIMIT 下推（top 512 恢复上限的机制）");
        assert_eq!(limited[0].expires_at, 1_700_000_004);
    }

    /// 过期清理：删除 expires_at <= now 的行并返回删除数。
    #[tokio::test]
    async fn purge_expired_removes_only_stale_rows() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("init");

        repo.upsert_affinity_sticks(&[
            stick("", "old", "v-a", 100),
            stick("", "fresh", "v-b", 1_700_000_000),
        ])
        .await
        .expect("UPSERT");

        let purged = repo
            .purge_expired_affinity_sticks(1_000)
            .await
            .expect("purge");
        assert_eq!(purged, 1, "只删过期行");
        let rows = repo.read_affinity_sticks(0, 10).await.expect("read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_key, "fresh");
    }

    /// 空批 UPSERT 短路（无 SQL 往返）——write-behind flush_buffer 的空批路径。
    #[tokio::test]
    async fn empty_upsert_is_noop() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("init");
        repo.upsert_affinity_sticks(&[]).await.expect("空批必须 Ok");
        let rows = repo.read_affinity_sticks(0, 10).await.expect("read");
        assert!(rows.is_empty());
    }
}

/// V6 建表与 UPSERT 的**服务端方言**运行时验证（postgres / mysql）。
///
/// 上面 `affinity_sticks` 模块全走 `sqlite::memory:`；三方言建表与
/// `ON CONFLICT` / `ON DUPLICATE KEY UPDATE` 此前只有纯函数字符串断言。
/// 本用例要求环境变量 `FLUXGATE_TEST_DB_URL` 指向被测实例（无则 `#[ignore]`
/// 跳过；CI 的 `server-backend` 作业注入该变量后以 `-- --ignored` 运行）。
///
/// 单函数串行完成全部断言：服务端实例是共享的，多测试并行会互相看到对方的行。
#[cfg(test)]
mod affinity_sticks_server {
    use fluxgate::storage::AffinityStick;

    fn server_url() -> String {
        std::env::var("FLUXGATE_TEST_DB_URL")
            .expect("本用例需要 FLUXGATE_TEST_DB_URL 指向 postgres/mysql 实例")
    }

    fn stick(tenant: &str, session: &str, vendor: &str, expires_at: i64) -> AffinityStick {
        AffinityStick {
            tenant: tenant.into(),
            session_key: session.into(),
            vendor_id: vendor.into(),
            worth_tokens: 2_048,
            expires_at,
        }
    }

    #[tokio::test]
    #[ignore = "需要 FLUXGATE_TEST_DB_URL 指向 postgres/mysql 实例（CI server-backend 作业注入）"]
    async fn v6_and_upsert_run_on_server_backend() {
        let url = server_url();
        assert!(
            url.starts_with("postgres://")
                || url.starts_with("postgresql://")
                || url.starts_with("mysql://"),
            "FLUXGATE_TEST_DB_URL 必须指向 postgres:// 或 mysql://（实际 {url}）"
        );

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: url.clone(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("连接被测服务端实例");
        let repo = fluxgate::database::Repository::new(pool);

        // ① 建表迁移在真实方言上可重入（V6 DDL + 索引创建）
        repo.initialize().await.expect("首次迁移");
        repo.initialize().await.expect("重跑幂等");

        // ② 清场：服务端实例是共享的，先删掉本模块可能遗留的行
        repo.purge_expired_affinity_sticks(i64::MAX)
            .await
            .expect("清场");

        // ③ UPSERT 冲突路径：同 (tenant, session) 覆盖，租户行独立
        repo.upsert_affinity_sticks(&[
            stick("", "s1", "v-a", 100),
            stick("", "s1", "v-b", 200),
            stick("tenant-a", "s1", "v-c", 300),
        ])
        .await
        .expect("服务端方言 UPSERT");
        let rows = repo.read_affinity_sticks(0, 10).await.expect("read");
        assert_eq!(rows.len(), 2, "legacy 覆盖为单行 + 租户行独立");
        let legacy = rows
            .iter()
            .find(|r| r.tenant.is_empty())
            .expect("legacy 行");
        assert_eq!(
            legacy.vendor_id, "v-b",
            "ON CONFLICT / ON DUPLICATE KEY 必须覆盖"
        );
        assert_eq!(legacy.expires_at, 200);
        assert_eq!(
            rows.iter()
                .find(|r| r.tenant == "tenant-a")
                .expect("租户行")
                .vendor_id,
            "v-c"
        );

        // ④ 未过期读取：过期排除 + 降序 + LIMIT 下推
        //（先清场：③ 的遗留行 expires_at=100/200/300 会落进本步的读取集）
        repo.purge_expired_affinity_sticks(i64::MAX)
            .await
            .expect("清场");
        let batch: Vec<AffinityStick> = (0..5)
            .map(|i| stick("", &format!("s{i}"), "v-a", 1_700_000_000 + i))
            .chain(std::iter::once(stick("", "expired", "v-b", 50)))
            .collect();
        repo.upsert_affinity_sticks(&batch).await.expect("UPSERT");
        let fresh = repo.read_affinity_sticks(100, 10).await.expect("read");
        assert_eq!(fresh.len(), 5, "过期行不入恢复集");
        assert_eq!(fresh[0].expires_at, 1_700_000_004, "降序：最新优先");
        let limited = repo.read_affinity_sticks(100, 2).await.expect("read");
        assert_eq!(limited.len(), 2, "LIMIT 下推");

        // ⑤ 过期清理
        let purged = repo
            .purge_expired_affinity_sticks(1_000)
            .await
            .expect("purge");
        assert!(purged >= 1, "过期行必须被删除（实际 {purged}）");
    }
}

/// quota_ledger（quota-aware-routing V7）：建表幂等 + UPSERT 覆盖（PK 三列
/// 去重）+ 全量读取（启动恢复读侧）。
#[cfg(test)]
mod quota_ledger {
    use fluxgate::database::QuotaLedgerRow;

    async fn pool() -> dbnexus::DbPool {
        dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    fn row(key: &str, window: &str, start: i64, tokens: i64, cost: i64) -> QuotaLedgerRow {
        QuotaLedgerRow {
            api_key_hash: key.into(),
            window_id: window.into(),
            window_start: start,
            tokens_used: tokens,
            cost_used_millicents: cost,
            updated_at: 1_770_000_000,
        }
    }

    /// 迁移建表 + 重跑幂等（V7 表可写可读，三列复合主键 UPSERT 覆盖）。
    #[tokio::test]
    async fn v7_migration_creates_quota_ledger_idempotently() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("首次迁移");
        repo.initialize().await.expect("重跑幂等");

        // 同 (key, window, window_start) 二次写入覆盖不重复；不同 window_start
        // 各占一行（每周期实例一行）。
        repo.upsert_quota_ledger(&[row("k1", "daily", 1_770_000_000, 100, 40)])
            .await
            .expect("首次 UPSERT");
        repo.upsert_quota_ledger(&[row("k1", "daily", 1_770_000_000, 250, 90)])
            .await
            .expect("覆盖 UPSERT");
        repo.upsert_quota_ledger(&[
            row("k1", "daily", 1_770_086_400, 10, 0),
            row("__global__", "fleet", 1_770_000_000, 7, 3),
        ])
        .await
        .expect("跨周期与全局键");

        let mut rows = repo.read_quota_ledger(0).await.expect("read");
        rows.sort_by(|a, b| {
            (a.api_key_hash.as_str(), a.window_start)
                .cmp(&(b.api_key_hash.as_str(), b.window_start))
        });
        assert_eq!(rows.len(), 3, "PK 三列去重：2 周期实例 + 1 全局行");
        let overwritten = rows
            .iter()
            .find(|r| r.api_key_hash == "k1" && r.window_start == 1_770_000_000)
            .expect("k1 当期行");
        assert_eq!(overwritten.tokens_used, 250, "同 PK 覆盖为最后写入");
        assert_eq!(overwritten.cost_used_millicents, 90);
        let next_period = rows
            .iter()
            .find(|r| r.api_key_hash == "k1" && r.window_start == 1_770_086_400)
            .expect("k1 下周期行");
        assert_eq!(next_period.tokens_used, 10, "不同周期实例独立行");
        let global = rows
            .iter()
            .find(|r| r.api_key_hash == "__global__")
            .expect("全局行");
        assert_eq!(global.window_id, "fleet", "global 作用域行同表同路");
    }

    /// 空批 UPSERT 短路（无 SQL 往返）。
    #[tokio::test]
    async fn empty_upsert_is_noop() {
        let pool = pool().await;
        let repo = fluxgate::database::Repository::new(pool);
        repo.initialize().await.expect("init");
        repo.upsert_quota_ledger(&[]).await.expect("空批必须 Ok");
        assert!(
            repo.read_quota_ledger(0).await.expect("read").is_empty(),
            "空批不产行"
        );
    }
}

/// V7 建表与 UPSERT 的**服务端方言**运行时验证（postgres / mysql）——与
/// affinity_sticks_server 同约定：`FLUXGATE_TEST_DB_URL` 指向被测实例，
/// CI server-backend 作业以 `-- --ignored` 运行；单函数串行防并行互染。
#[cfg(test)]
mod quota_ledger_server {
    use fluxgate::database::QuotaLedgerRow;

    #[tokio::test]
    #[ignore = "需要 FLUXGATE_TEST_DB_URL 指向 postgres/mysql 实例（CI server-backend 作业注入）"]
    async fn v7_and_upsert_run_on_server_backend() {
        let url = std::env::var("FLUXGATE_TEST_DB_URL")
            .expect("本用例需要 FLUXGATE_TEST_DB_URL 指向 postgres/mysql 实例");
        assert!(
            url.starts_with("postgres://")
                || url.starts_with("postgresql://")
                || url.starts_with("mysql://"),
            "FLUXGATE_TEST_DB_URL 必须指向 postgres:// 或 mysql://（实际 {url}）"
        );

        let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
            url: url.clone(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("连接被测服务端实例");
        let repo = fluxgate::database::Repository::new(pool);
        // ① 建表迁移在真实方言上可重入（V7 DDL）
        repo.initialize().await.expect("首次迁移");
        repo.initialize().await.expect("重跑幂等");

        // ② 三列复合主键 UPSERT 在真实方言上覆盖去重（ON CONFLICT /
        //    ON DUPLICATE KEY 两路 SQL 由 upsert_quota_ledger_sql 按后端
        //    选择）；同批喂跨周期双行走 begin → N×execute → commit 事务
        //    模式——生产 flush 即批量写，与 sqlite 用例批量形态对齐
        let mk = |start: i64, tokens: i64| QuotaLedgerRow {
            api_key_hash: "server-dialect".into(),
            window_id: "daily".into(),
            window_start: start,
            tokens_used: tokens,
            cost_used_millicents: 0,
            updated_at: 1_770_000_000,
        };
        repo.upsert_quota_ledger(&[mk(1_770_000_000, 100)])
            .await
            .expect("UPSERT");
        repo.upsert_quota_ledger(&[mk(1_770_000_000, 220), mk(1_770_086_400, 10)])
            .await
            .expect("覆盖 UPSERT + 跨周期批量");
        let rows = repo.read_quota_ledger(0).await.expect("read");
        let mine: Vec<&QuotaLedgerRow> = rows
            .iter()
            .filter(|r| r.api_key_hash == "server-dialect")
            .collect();
        assert_eq!(mine.len(), 2, "同 PK 覆盖为单行 + 下周期独立行");
        let current = mine
            .iter()
            .find(|r| r.window_start == 1_770_000_000)
            .expect("当期行");
        assert_eq!(current.tokens_used, 220, "覆盖为最后写入");
        let next = mine
            .iter()
            .find(|r| r.window_start == 1_770_086_400)
            .expect("下周期行");
        assert_eq!(next.tokens_used, 10, "不同周期实例独立行");

        // ③ 清场（共享实例不留测试行）：purge 路径真删而非 0 值覆盖残留，
        //    同时让 purge_expired_quota_ledger 的方言 DELETE 进 CI 实跑面。
        //    范围假设：purge 按过期阈值清**全表**（不只本模块行）——依赖
        //    共享实例上 quota_ledger 无第二写入方（CI --test-threads=1
        //    串行，另一 ignore 用例 affinity_sticks_server 只写 affinity
        //    表）；`purged >= 2` 的下界断言顺带容忍历史残留行。未来新增
        //    写 quota_ledger 的用例须与本模块同样自约串行 + 事后清场。
        let purged = repo
            .purge_expired_quota_ledger(i64::MAX)
            .await
            .expect("清场");
        assert!(purged >= 2, "本模块写入的行必须被真删（实际 {purged}）");
        assert!(
            repo.read_quota_ledger(0)
                .await
                .expect("read")
                .iter()
                .all(|r| r.api_key_hash != "server-dialect"),
            "清场后本模块测试行无残留"
        );
    }
}

// V9 上游键归因列（magpie-absorb-r1 R-usage-001）：列存在（迁移链落
// 地）+ Some/None 两态往返无损。
#[cfg(test)]
mod v9_key_attribution {
    use dbnexus::{DbConfig, DbPool, PoolConfig};
    use fluxgate::database::{LogFilter, Repository, RequestLog};
    use std::sync::Arc;

    async fn repo() -> Arc<Repository> {
        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let repo = Arc::new(Repository::new(pool));
        repo.initialize().await.expect("init schema (V1..=V9)");
        repo
    }

    fn log(id: &str, fp: Option<&str>) -> RequestLog {
        RequestLog {
            id: None,
            request_id: id.to_string(),
            timestamp: 1_700_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "[]".to_string(),
            actual_vendor: "v".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "direct".to_string(),
            status_code: Some(200),
            response: None,
            error_message: None,
            prompt_tokens: Some(1),
            completion_tokens: Some(1),
            total_tokens: Some(2),
            duration_ms: Some(3),
            cost_usd: None,
            session_key: None,
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            upstream_key_fingerprint: fp.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn upstream_key_fingerprint_roundtrip_some_and_null() {
        let repo = repo().await;
        repo.log_request(&log("v9-fp-some", Some("aabb001122334455")))
            .await
            .expect("insert with fp");
        repo.log_request(&log("v9-fp-null", None))
            .await
            .expect("insert null fp");

        let rows = repo
            .query_logs(&LogFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("query");
        let some = rows.iter().find(|r| r.request_id == "v9-fp-some").unwrap();
        let null = rows.iter().find(|r| r.request_id == "v9-fp-null").unwrap();
        assert_eq!(
            some.upstream_key_fingerprint.as_deref(),
            Some("aabb001122334455"),
            "Some 指纹往返无损（V9 列存在且经 INSERT/SELECT 两侧）"
        );
        assert!(
            null.upstream_key_fingerprint.is_none(),
            "NULL 语义保持（历史行/未走键池）"
        );
    }
}
