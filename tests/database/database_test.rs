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

// ============ 单事务批量写入（Repository::log_requests_batch） ============

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

// ============ 覆盖补缺：Token 预聚合器（record / flush / flush worker / handle） ============

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

// ============ 覆盖补缺：Repository 迁移 / 批量回滚 / 厂商状态读取 ============

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

// ============ 覆盖补缺：AsyncLogger 关闭/背压/缓冲阈值/落库失败路径 ============

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
        }
    }

    /// shutdown 后 log() 拒绝写入：Database 错误 + shutdown 语义消息。
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

// ============ 覆盖补缺：Database::new 自举与父路径非法 ============

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
        }
    }

    /// 内存库自举：Database::new 建连 + 建表，repository() 可直接读写，
    /// close() 幂等无副作用。
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

// ============ 成本分账落库（V2 分账聚合 + /admin/cost 查询面） ============

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
