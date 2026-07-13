// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 异步日志写入器
//!
//! 使用专用 channel 实现异步批量日志写入，提高性能。

use crate::database::{Repository, RequestLog};
use crate::error::NexusFlowError;
use async_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// 日志写入命令
#[allow(clippy::large_enum_variant)]
enum LogCommand {
    /// 写入日志
    Write(RequestLog),
    /// 关闭日志写入器
    Shutdown,
}

/// 异步日志写入器
pub struct AsyncLogger {
    sender: Sender<LogCommand>,
}

impl AsyncLogger {
    /// 创建新的异步日志写入器
    pub fn new(
        repository: Arc<Repository>,
        channel_capacity: usize,
        flush_interval: Duration,
    ) -> Self {
        let (sender, receiver) = bounded(channel_capacity);

        // 启动后台写入任务
        tokio::spawn(async move {
            Self::writer_loop(receiver, repository, flush_interval).await;
        });

        Self { sender }
    }

    /// 异步写入日志
    pub async fn log(&self, log: RequestLog) -> Result<(), NexusFlowError> {
        self.sender
            .send(LogCommand::Write(log))
            .await
            .map_err(|_| NexusFlowError::Database {
                message: "Async logger channel closed".into(),
            })
    }

    /// 优雅关闭
    pub async fn shutdown(&self) -> Result<(), NexusFlowError> {
        self.sender
            .send(LogCommand::Shutdown)
            .await
            .map_err(|_| NexusFlowError::Database {
                message: "Async logger channel closed".into(),
            })
    }

    /// 后台写入循环
    async fn writer_loop(
        receiver: Receiver<LogCommand>,
        repository: Arc<Repository>,
        flush_interval: Duration,
    ) {
        let mut buffer: Vec<RequestLog> = Vec::with_capacity(100);
        let mut interval = tokio::time::interval(flush_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // 定时刷新缓冲区
                    Self::flush_buffer(&repository, &mut buffer).await;
                }
                command = receiver.recv() => {
                    match command {
                        Ok(LogCommand::Write(log)) => {
                            buffer.push(log);
                            // 如果缓冲区已满，立即刷新
                            if buffer.len() >= 100 {
                                Self::flush_buffer(&repository, &mut buffer).await;
                            }
                        }
                        Ok(LogCommand::Shutdown) => {
                            // 关闭前刷新剩余日志
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                        Err(_) => {
                            // Channel 已关闭，退出
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 刷新缓冲区到数据库
    async fn flush_buffer(repository: &Arc<Repository>, buffer: &mut Vec<RequestLog>) {
        if buffer.is_empty() {
            return;
        }

        // 批量写入
        for log in buffer.drain(..) {
            if let Err(e) = repository.log_request(&log).await {
                tracing::error!("Failed to write request log: {}", e);
            }
        }
    }
}

/// 异步日志写入器句柄
#[derive(Clone)]
pub struct AsyncLoggerHandle {
    inner: Arc<Mutex<Option<AsyncLogger>>>,
}

impl AsyncLoggerHandle {
    /// 创建新的句柄
    pub fn new(logger: AsyncLogger) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(logger))),
        }
    }

    /// 写入日志
    pub async fn log(&self, log: RequestLog) -> Result<(), NexusFlowError> {
        let guard = self.inner.lock().await;
        if let Some(logger) = guard.as_ref() {
            logger.log(log).await
        } else {
            Err(NexusFlowError::Database {
                message: "Logger already shut down".into(),
            })
        }
    }

    /// 关闭日志写入器
    pub async fn shutdown(&self) -> Result<(), NexusFlowError> {
        let mut guard = self.inner.lock().await;
        if let Some(logger) = guard.take() {
            logger.shutdown().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::LogFilter;
    use dbnexus::{DbConfig, DbPool};
    use std::time::Duration;

    async fn setup_repository() -> Arc<Repository> {
        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            max_connections: 1,
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("Failed to create memory DB pool");

        let repository = Arc::new(Repository::new(pool));
        repository
            .initialize()
            .await
            .expect("Failed to initialize schema");
        repository
    }

    fn sample_log() -> RequestLog {
        RequestLog {
            id: None,
            request_id: "req-test-001".to_string(),
            timestamp: 1_718_000_000,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("gpt-4".to_string()),
            messages: "Hello, world!".to_string(),
            actual_vendor: "openai_main".to_string(),
            actual_model: "gpt-4".to_string(),
            routing_strategy: "weighted".to_string(),
            status_code: Some(200),
            response: Some("Hi!".to_string()),
            error_message: None,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            total_tokens: Some(15),
            duration_ms: Some(120),
        }
    }

    #[tokio::test]
    async fn test_async_logger_writes_log_on_shutdown() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(
            repository.clone(),
            100,
            Duration::from_secs(60),
        );

        logger.log(sample_log()).await.expect("Log write failed");
        logger.shutdown().await.expect("Shutdown failed");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let filter = LogFilter {
            vendor: Some("openai_main".to_string()),
            limit: 10,
            ..Default::default()
        };

        let logs = repository.query_logs(&filter).await.expect("Query failed");
        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(log.request_id, "req-test-001");
        assert_eq!(log.actual_model, "gpt-4");
        assert_eq!(log.status_code, Some(200));
        assert_eq!(log.total_tokens, Some(15));
    }

    #[tokio::test]
    async fn test_async_logger_writes_log_on_buffer_full() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(
            repository.clone(),
            200,
            Duration::from_secs(60),
        );

        for i in 0..101 {
            let mut log = sample_log();
            log.request_id = format!("req-{:03}", i);
            logger.log(log).await.expect("Log write failed");
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
        logger.shutdown().await.expect("Shutdown failed");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert_eq!(logs.len(), 101);
    }

    #[tokio::test]
    async fn test_async_logger_writes_log_on_interval() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(
            repository.clone(),
            100,
            Duration::from_millis(20),
        );

        logger.log(sample_log()).await.expect("Log write failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert!(!logs.is_empty());

        logger.shutdown().await.expect("Shutdown failed");
    }

    #[tokio::test]
    async fn test_async_logger_handle_logs_and_shutdowns() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(
            repository.clone(),
            100,
            Duration::from_secs(60),
        );
        let handle = AsyncLoggerHandle::new(logger);

        handle.log(sample_log()).await.expect("Log write failed");
        handle.shutdown().await.expect("Shutdown failed");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert_eq!(logs.len(), 1);
    }

    #[tokio::test]
    async fn test_async_logger_handle_after_shutdown_returns_error() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(
            repository.clone(),
            100,
            Duration::from_secs(60),
        );
        let handle = AsyncLoggerHandle::new(logger);

        handle.shutdown().await.expect("Shutdown failed");

        let result = handle.log(sample_log()).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            NexusFlowError::Database { message } => {
                assert!(message.contains("shut down"));
            }
            other => panic!("Expected Database error, got {:?}", other),
        }
    }
}
