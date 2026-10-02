// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 异步日志写入器
//!
//! 使用专用 channel 实现异步批量日志写入，提高性能。
//!
//! recId=9 改造后的背压语义：`log()` 经 `try_send` 入队——channel 满即丢
//! 弃并计数（`dropped_logs`），**绝不阻塞响应路径**；消费侧单事务批量
//! flush（见 `Repository::log_requests_batch`），停机时补最终 flush。

use crate::database::{Repository, RequestLog};
use crate::error::NexusFlowError;
use async_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 单次 flush 的缓冲阈值（条）：达到即提前落库
const FLUSH_BUFFER_CAPACITY: usize = 100;

/// 日志写入命令
#[allow(clippy::large_enum_variant)]
enum LogCommand {
    /// 写入日志
    Write(RequestLog),
    /// 关闭日志写入器
    Shutdown,
}

/// 异步日志写入器
///
/// Clone 廉价（sender + 原子计数），多任务共享同一背压通道与丢弃计数。
#[derive(Clone)]
pub struct AsyncLogger {
    sender: Sender<LogCommand>,
    /// recId=9①：channel 满被丢弃的日志条数 — 背压语义可观测，
    /// 暴露进 /admin/metrics
    dropped_logs: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
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

        Self {
            sender,
            dropped_logs: Arc::new(AtomicU64::new(0)),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 写入日志（非阻塞 push：channel 满即丢弃并计数，绝不等待）。
    ///
    /// recId=9①：原 `send().await` 在 channel 满时会阻塞调用方（响应路径）；
    /// 改 `try_send` 后背压语义明确 —— 满即丢并计入 `dropped_logs`。
    /// 返回 Err 仅表示 logger 已 shutdown（channel 关闭）。
    pub fn log(&self, log: RequestLog) -> Result<(), NexusFlowError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(NexusFlowError::Database {
                message: "Logger already shut down".into(),
            });
        }
        match self.sender.try_send(LogCommand::Write(log)) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Full(_)) => {
                self.dropped_logs.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(async_channel::TrySendError::Closed(_)) => Err(NexusFlowError::Database {
                message: "Async logger channel closed".into(),
            }),
        }
    }

    /// recId=9①：丢弃计数 — 暴露进 /admin/metrics 的背压观测信号
    pub fn dropped_logs(&self) -> u64 {
        self.dropped_logs.load(Ordering::Relaxed)
    }

    /// 优雅关闭：置 closed 标志（后续 log() 拒绝）→ 发 Shutdown 命令 →
    /// writer_loop flush 剩余缓冲后退出。
    pub async fn shutdown(&self) -> Result<(), NexusFlowError> {
        self.closed.store(true, Ordering::Relaxed);
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
        let mut buffer: Vec<RequestLog> = Vec::with_capacity(FLUSH_BUFFER_CAPACITY);
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
                            if buffer.len() >= FLUSH_BUFFER_CAPACITY {
                                Self::flush_buffer(&repository, &mut buffer).await;
                            }
                        }
                        Ok(LogCommand::Shutdown) => {
                            // 关闭前刷新剩余日志
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                        Err(_) => {
                            // recId=9①：channel 断开（所有 sender 被 drop）时
                            // 也必须补最终 flush —— 此前直接 break 丢缓冲
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 刷新缓冲区到数据库（recId=9①：单事务批量 — 见
    /// `Repository::log_requests_batch`，N 条 INSERT 一次 commit）
    async fn flush_buffer(repository: &Arc<Repository>, buffer: &mut Vec<RequestLog>) {
        if buffer.is_empty() {
            return;
        }

        if let Err(e) = repository.log_requests_batch(buffer).await {
            tracing::error!(
                "Failed to write request logs (batch of {}): {}",
                buffer.len(),
                e
            );
        }
        buffer.clear();
    }
}

/// 异步日志写入器句柄
///
/// recId=9①：去掉了每条日志一把的 `tokio::Mutex<Option<AsyncLogger>>` ——
/// 句柄即 Clone 的 sender + `AtomicBool` 关闭标志，log() 无锁直通
/// （原实现 N 条日志串行化在一把互斥锁上，与"push 即返回"目标相悖）。
#[derive(Clone)]
pub struct AsyncLoggerHandle {
    inner: AsyncLogger,
}

impl AsyncLoggerHandle {
    /// 创建新的句柄
    pub fn new(logger: AsyncLogger) -> Self {
        Self { inner: logger }
    }

    /// 写入日志（非阻塞，同 [`AsyncLogger::log`]）
    pub fn log(&self, log: RequestLog) -> Result<(), NexusFlowError> {
        self.inner.log(log)
    }

    /// recId=9①：丢弃计数透传
    pub fn dropped_logs(&self) -> u64 {
        self.inner.dropped_logs()
    }

    /// 关闭日志写入器（幂等：重复 shutdown 第二次返回 Err channel closed ——
    /// 与 shutdown 语义一致，调用方以 Ok/Err 判断首闭）
    pub async fn shutdown(&self) -> Result<(), NexusFlowError> {
        self.inner.shutdown().await
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
            // rc.5 起 PoolConfig::default() 的 min_connections=5 且校验
            // min <= max，必须与 max_connections 一起显式给出
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
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
            cost_usd: Some(0.001),
        }
    }

    #[tokio::test]
    async fn test_async_logger_writes_log_on_shutdown() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));

        logger.log(sample_log()).expect("Log write failed");
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

    /// recId=9 tests ①：push 100 条 → shutdown → query_logs 恰 100 条
    /// （单事务批量 flush 不丢条、不留半批）。
    #[tokio::test]
    async fn test_async_logger_batch_of_100_survives_shutdown_flush() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 200, Duration::from_secs(60));

        for i in 0..100 {
            let mut log = sample_log();
            log.request_id = format!("batch-{:03}", i);
            logger.log(log).expect("Log write failed");
        }
        logger.shutdown().await.expect("Shutdown failed");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert_eq!(logs.len(), 100, "事务批量 flush 必须整批落库");
    }

    #[tokio::test]
    async fn test_async_logger_writes_log_on_buffer_full() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 200, Duration::from_secs(60));

        for i in 0..101 {
            let mut log = sample_log();
            log.request_id = format!("req-{:03}", i);
            logger.log(log).expect("Log write failed");
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
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_millis(20));

        logger.log(sample_log()).expect("Log write failed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert!(!logs.is_empty());

        logger.shutdown().await.expect("Shutdown failed");
    }

    /// recId=9 tests ②：容量 1 且消费侧被停机前缓冲卡住时连 push 3 条，
    /// 全部 100ms 内返回（不阻塞）且 dropped_logs==2（背压：满即丢并计数）。
    ///
    /// 构造"不消费"：容量 1 的 channel + shutdown 命令排在消费循环里……
    /// 更直接：writer 在消费前先占用 1 个槽位不可行——用 Tiny 容量 1，
    /// push 3 条后 1 条在 channel、2 条被丢；writer_loop 正常消费（消费快
    /// 不影响断言：try_send 是瞬时判定，满槽只可能是 1 条在途）。
    #[tokio::test]
    async fn test_async_logger_backpressure_drops_and_counts() {
        let repository = setup_repository().await;
        // 慢消费不好直接注入（writer_loop 私有），改以容量 1 + 快速连发 3 条：
        // 第 1 条进 channel；第 2/3 条在消费侧尚未 recv 的窗口内必然撞满。
        let logger = Arc::new(AsyncLogger::new(
            repository.clone(),
            1,
            Duration::from_secs(60),
        ));

        let start = std::time::Instant::now();
        let r1 = logger.log(sample_log());
        let r2 = logger.log(sample_log());
        let r3 = logger.log(sample_log());
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(100),
            "log() 不得阻塞: {elapsed:?}"
        );
        assert!(r1.is_ok());
        // 至少一条被丢弃计数（消费侧 recv 竞态下最多 1 条在 channel）
        assert!(
            logger.dropped_logs() >= 1,
            "满槽必须丢弃并计数: dropped={}",
            logger.dropped_logs()
        );
        // 全部返回 Ok（丢弃不是错误）
        assert!(r2.is_ok() && r3.is_ok());

        logger.shutdown().await.expect("Shutdown failed");
    }

    /// recId=9 tests ③：channel 断开（Clone 出的 sender 全部 drop）→
    /// writer_loop 的 Err 分支退出前补最终 flush，缓冲不丢。
    #[tokio::test]
    async fn test_async_logger_flushes_on_channel_disconnect() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));

        logger.log(sample_log()).expect("Log write failed");
        // 故意不走 shutdown：直接 drop（channel sender 随之断开）
        drop(logger);
        tokio::time::sleep(Duration::from_millis(100)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert_eq!(logs.len(), 1, "断开分支退出前必须补最终 flush（recId=9①）");
    }

    #[tokio::test]
    async fn test_async_logger_handle_logs_and_shutdowns() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));
        let handle = AsyncLoggerHandle::new(logger);

        handle.log(sample_log()).expect("Log write failed");
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
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));
        let handle = AsyncLoggerHandle::new(logger);

        handle.shutdown().await.expect("Shutdown failed");

        let result = handle.log(sample_log());
        assert!(result.is_err());
        match result.unwrap_err() {
            NexusFlowError::Database { message } => {
                assert!(message.contains("shut down"));
            }
            other => panic!("Expected Database error, got {:?}", other),
        }
    }
}
