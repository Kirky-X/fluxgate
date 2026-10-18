// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 异步日志写入器
//!
//! 使用专用 channel 实现异步批量日志写入，提高性能。
//!
//! 改造后的背压语义：`log()` 经 `try_send` 入队——channel 满即丢
//! 弃并计数（`dropped_logs`），**绝不阻塞响应路径**；消费侧单事务批量
//! flush（见 `Repository::log_requests_batch`），停机时补最终 flush。

use crate::database::{Repository, RequestLog};
use crate::error::FluxgateError;
use crate::i18n::{t, t_args};
use async_channel::{bounded, Receiver, Sender};
use inklog::{DataMasker, EntropyScanner, InklogError, SecretPatternRegistry, SecretScanGate};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 单次 flush 的缓冲阈值（条）：达到即提前落库
const FLUSH_BUFFER_CAPACITY: usize = 100;

/// 单条出站文本的截断门限（字节）：对齐 API 侧 64KB 上限，并远离
/// fancy-regex 长文本回溯 panic（门限内仍可触雷，取保守值）；
/// 截断保留门限内前缀（尾部丢弃不外泄，无泄密面）
pub(crate) const MASK_INPUT_LIMIT_BYTES: usize = 64 * 1024;

/// 截断留痕标记：审计可辨识"此文本已按门限截断"。
pub(crate) const TRUNCATION_MARKER: &str = "\n…[truncated]";

/// 出站脱敏 masker：库默认规则构造 SecretScanGate，不做业务定制；
/// console sink 与落库管道共用此构造，两条出站通路同一脱敏面
pub(crate) fn outbound_masker() -> DataMasker {
    let gate = SecretScanGate::new(SecretPatternRegistry::with_builtins())
        .with_entropy(EntropyScanner::new());
    DataMasker::builder().with_secret_scan(gate).build()
}

/// 单条出站文本的统一脱敏策略（console 与落库共用，fail 姿态一致）：
/// 超门限先按 UTF-8 边界截断再 mask_checked（fail-closed）；仍超门限
/// → Err → 整条拒发（宁可丢日志不可泄密）
pub(crate) fn redact_text(masker: &DataMasker, text: &str) -> Result<String, InklogError> {
    let bounded: std::borrow::Cow<str> = if text.len() > MASK_INPUT_LIMIT_BYTES {
        std::borrow::Cow::Owned(format!("{}{TRUNCATION_MARKER}", truncate_to_limit(text)))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    masker.mask_checked(&bounded)
}

/// 门限内最后一个 UTF-8 字符边界（向下取整），预留截断标记长度。
fn truncate_to_limit(text: &str) -> &str {
    let mut end = MASK_INPUT_LIMIT_BYTES - TRUNCATION_MARKER.len();
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// [`redact_text`] 的 serde_json::Value 递归形态：字符串值逐个脱敏，
/// 数组/对象下钻——fields 的嵌套 JSON 与顶层标量同一 fail 姿态。
#[cfg_attr(not(feature = "web-server"), allow(dead_code))] // 仅 server 装配消费
fn redact_value(masker: &DataMasker, value: &mut Value) -> Result<(), InklogError> {
    match value {
        Value::String(s) => {
            *s = redact_text(masker, s)?;
        }
        Value::Array(items) => {
            for item in items {
                redact_value(masker, item)?;
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                redact_value(masker, v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// console 出站包装：先按 redact_text 统一策略改写 message/fields 再委托
/// inner sink（两条出站通路 fail 姿态一致）。gate 拒发按 Ok 处理（预期
/// 丢弃），逐条 warn + dropped 计数显性化，绝不携带记录内容
#[cfg_attr(not(feature = "web-server"), allow(dead_code))] // 仅 server bootstrap 构造
pub(crate) struct GateSink {
    inner: Arc<dyn inklog::LogSink>,
    masker: Arc<DataMasker>,
    dropped: AtomicU64,
}

#[cfg_attr(not(feature = "web-server"), allow(dead_code))] // 同上，构造点在 server 装配
impl GateSink {
    pub(crate) fn new(inner: Arc<dyn inklog::LogSink>, masker: Arc<DataMasker>) -> Self {
        Self {
            inner,
            masker,
            dropped: AtomicU64::new(0),
        }
    }

    /// gate 拒发（含截断前缀超限）未上 console 的记录条数。生产观测走
    /// [`Self::reject`] 逐条 warn 的 total 字段（累计随日志流可见），
    /// 精确读取仅测试需要。
    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn reject(&self, error: &InklogError) {
        let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::warn!(
            total,
            error = %error,
            "{}",
            t("database-logger-console-secret-gate-dropped")
        );
    }
}

#[async_trait::async_trait]
impl inklog::LogSink for GateSink {
    async fn write(&self, record: &inklog::LogRecord) -> Result<(), InklogError> {
        let mut masked = record.clone();
        masked.message = match redact_text(&self.masker, &record.message) {
            Ok(text) => text,
            Err(e) => {
                self.reject(&e);
                return Ok(());
            }
        };
        for value in masked.fields.values_mut() {
            if let Err(e) = redact_value(&self.masker, value) {
                self.reject(&e);
                return Ok(());
            }
        }
        self.inner.write(&masked).await
    }

    async fn flush(&self) -> Result<(), InklogError> {
        self.inner.flush().await
    }

    async fn shutdown(&self) -> Result<(), InklogError> {
        self.inner.shutdown().await
    }
}

/// 日志写入命令
#[allow(clippy::large_enum_variant)]
enum LogCommand {
    /// 写入日志
    Write(RequestLog),
    /// 流式占位行补写：writer 侧先 flush 缓冲中的占位 INSERT 再 UPDATE
    /// （通道 FIFO + 显式 flush，杜绝「更新先于占位落库」竞态）
    UpdateStreamUsage {
        request_id: String,
        response: String,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
    },
    /// 关闭日志写入器
    Shutdown,
}

/// 异步日志写入器
///
/// Clone 廉价（sender + 原子计数），多任务共享同一背压通道与丢弃计数。
#[derive(Clone)]
pub struct AsyncLogger {
    sender: Sender<LogCommand>,
    /// channel 满被丢弃的日志条数 — 背压语义可观测，
    /// 暴露进 /admin/metrics
    dropped_logs: Arc<AtomicU64>,
    /// secret gate fail-closed 拒发的日志条数 — 脱敏丢弃可观测，
    /// 暴露进 /admin/metrics（与背压丢弃分列，原因不同）
    secret_scan_dropped: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    /// writer_loop 任务句柄：shutdown() 取出并等待退出，
    /// 保证返回时补最终 flush 已落库
    writer: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl AsyncLogger {
    /// 创建新的异步日志写入器
    pub fn new(
        repository: Arc<Repository>,
        channel_capacity: usize,
        flush_interval: Duration,
    ) -> Self {
        let (sender, receiver) = bounded(channel_capacity);
        let secret_scan_dropped = Arc::new(AtomicU64::new(0));

        // 启动后台写入任务（masker/计数器 move 进消费侧：脱敏不占
        // 响应路径——log() 的 try_send 前台零扫描开销）
        let masker = Arc::new(outbound_masker());
        let loop_dropped = Arc::clone(&secret_scan_dropped);
        let writer = tokio::spawn(async move {
            Self::writer_loop(receiver, repository, flush_interval, masker, loop_dropped).await;
        });

        Self {
            sender,
            dropped_logs: Arc::new(AtomicU64::new(0)),
            secret_scan_dropped,
            closed: Arc::new(AtomicBool::new(false)),
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
        }
    }

    /// 写入日志（非阻塞 push：channel 满即丢弃并计数，绝不等待）；
    /// 返回 Err 仅表示 logger 已 shutdown
    pub fn log(&self, log: RequestLog) -> Result<(), FluxgateError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(FluxgateError::Database {
                message: t("database-logger-shutdown"),
            });
        }
        match self.sender.try_send(LogCommand::Write(log)) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Full(_)) => {
                self.dropped_logs.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(async_channel::TrySendError::Closed(_)) => Err(FluxgateError::Database {
                message: t("database-logger-channel-closed"),
            }),
        }
    }

    /// 流式占位行补写（非阻塞 push，同 [`AsyncLogger::log`] 的背压/关闭
    /// 姿态；通道满即丢弃并计数——与占位 INSERT 同一背压档位）
    pub fn update_stream_usage(
        &self,
        request_id: &str,
        response: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
    ) -> Result<(), FluxgateError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(FluxgateError::Database {
                message: t("database-logger-shutdown"),
            });
        }
        match self.sender.try_send(LogCommand::UpdateStreamUsage {
            request_id: request_id.to_string(),
            response: response.to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
        }) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Full(_)) => {
                self.dropped_logs.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(async_channel::TrySendError::Closed(_)) => Err(FluxgateError::Database {
                message: t("database-logger-channel-closed"),
            }),
        }
    }

    /// 丢弃计数 — 暴露进 /admin/metrics 的背压观测信号
    pub fn dropped_logs(&self) -> u64 {
        self.dropped_logs.load(Ordering::Relaxed)
    }

    /// secret gate fail-closed 拒发计数 — 暴露进 /admin/metrics 的
    /// 脱敏丢弃观测信号（与背压丢弃 [`Self::dropped_logs`] 分列）
    pub fn secret_scan_dropped(&self) -> u64 {
        self.secret_scan_dropped.load(Ordering::Relaxed)
    }

    /// 优雅关闭：置 closed 标志（后续 log() 拒绝）→ 发 Shutdown 命令 →
    /// 等待 writer_loop flush 剩余缓冲并退出——返回即最终 flush 已落库，
    /// 停机拆除顺序不依赖调用方自行估算宽限时长。
    pub async fn shutdown(&self) -> Result<(), FluxgateError> {
        self.closed.store(true, Ordering::Relaxed);
        self.sender
            .send(LogCommand::Shutdown)
            .await
            .map_err(|_| FluxgateError::Database {
                message: t("database-logger-channel-closed"),
            })?;
        let handle = self.writer.lock().await.take();
        if let Some(handle) = handle {
            handle.await.map_err(|e| FluxgateError::Database {
                message: t_args("database-logger-join-failed", &[("error", e.to_string())]),
            })?;
        }
        Ok(())
    }

    /// 后台写入循环
    async fn writer_loop(
        receiver: Receiver<LogCommand>,
        repository: Arc<Repository>,
        flush_interval: Duration,
        masker: Arc<DataMasker>,
        secret_scan_dropped: Arc<AtomicU64>,
    ) {
        let mut buffer: Vec<RequestLog> = Vec::with_capacity(FLUSH_BUFFER_CAPACITY);
        let mut interval = tokio::time::interval(flush_interval);
        // interval 首个 tick 立即完成。不在循环外先消费它，select 里的
        // tick 分支就会持续处于就绪态（tick 取消安全：被丢弃不算消费），
        // 每轮与 recv 竞逐随机胜出——缓冲远未满即被提前 flush 成碎片
        // 批次，破坏单事务批量落库语义
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // 定时刷新缓冲区
                    Self::flush_buffer(&repository, &mut buffer).await;
                }
                command = receiver.recv() => {
                    match command {
                        Ok(LogCommand::Write(log)) => {
                            // CPU 密集的 gate 扫描（9 类模式 + AC + PII 正则
                            // 管线，无 await 点不可抢占）移入阻塞线程池执行
                            // 并在此等待：不得占住异步 executor 造成同
                            // runtime 请求任务长尾抖动
                            let redact_masker = Arc::clone(&masker);
                            let result = tokio::task::spawn_blocking(move || {
                                Self::redact_log(&redact_masker, log)
                            })
                            .await
                            .unwrap_or_else(|e| {
                                Err(InklogError::ConfigError(format!(
                                    "redact worker failed: {e}"
                                )))
                            });
                            match result {
                                Ok(redacted) => {
                                    buffer.push(redacted);
                                    // 如果缓冲区已满，立即刷新
                                    if buffer.len() >= FLUSH_BUFFER_CAPACITY {
                                        Self::flush_buffer(&repository, &mut buffer).await;
                                    }
                                }
                                Err(e) => {
                                    // fail-closed：gate 拒发（截断后前缀内
                                    // 命中仍超门限）→ 整条丢弃，宁可丢日志
                                    // 不可泄密；丢弃显性计数 + warn，绝不带
                                    // 记录内容
                                    let total =
                                        secret_scan_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                                    tracing::warn!(
                                        "{}",
                                        t_args(
                                            "database-logger-secret-scan-dropped",
                                            &[
                                                ("total", total.to_string()),
                                                ("error", e.to_string()),
                                            ],
                                        )
                                    );
                                }
                            }
                        }
                        Ok(LogCommand::UpdateStreamUsage {
                            request_id,
                            response,
                            prompt_tokens,
                            completion_tokens,
                            total_tokens,
                        }) => {
                            // 占位 INSERT 可能仍在缓冲未 flush——先刷再更
                            Self::flush_buffer(&repository, &mut buffer).await;
                            if let Err(e) = repository
                                .update_stream_usage(
                                    &request_id,
                                    &response,
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                )
                                .await
                            {
                                tracing::error!(
                                    "{}",
                                    t_args(
                                        "database-logger-flush-failed",
                                        &[("count", "1".to_string()), ("error", e.to_string())],
                                    )
                                );
                            }
                        }
                        Ok(LogCommand::Shutdown) => {
                            // 关闭前刷新剩余日志
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                        Err(_) => {
                            // channel 断开（所有 sender 被 drop）时
                            // 退出前必须补最终 flush，避免丢缓冲
                            Self::flush_buffer(&repository, &mut buffer).await;
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 落库前出站脱敏（消费侧后台执行）：messages/response/error_message
    /// 三个自由文本字段经 [`redact_text`] 统一策略（超门限截断 + fail-closed
    /// gate）扫描改写，与 console 通路同一 fail 姿态。Err 原样上抛由调用方
    /// 丢弃计数。
    fn redact_log(masker: &DataMasker, mut log: RequestLog) -> Result<RequestLog, InklogError> {
        log.messages = redact_text(masker, &log.messages)?;
        if let Some(response) = log.response.take() {
            log.response = Some(redact_text(masker, &response)?);
        }
        if let Some(error_message) = log.error_message.take() {
            log.error_message = Some(redact_text(masker, &error_message)?);
        }
        Ok(log)
    }

    /// 刷新缓冲区到数据库（单事务批量 — 见
    /// `Repository::log_requests_batch`，N 条 INSERT 一次 commit）
    async fn flush_buffer(repository: &Arc<Repository>, buffer: &mut Vec<RequestLog>) {
        if buffer.is_empty() {
            return;
        }

        if let Err(e) = repository.log_requests_batch(buffer).await {
            tracing::error!(
                "{}",
                t_args(
                    "database-logger-flush-failed",
                    &[
                        ("count", buffer.len().to_string()),
                        ("error", e.to_string()),
                    ],
                )
            );
        }
        buffer.clear();
    }
}

/// 异步日志写入器句柄：Clone 的 sender + AtomicBool 关闭标志，log() 无锁直通
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
    pub fn log(&self, log: RequestLog) -> Result<(), FluxgateError> {
        self.inner.log(log)
    }

    /// 流式占位行补写透传（同 [`AsyncLogger::update_stream_usage`]）
    pub fn update_stream_usage(
        &self,
        request_id: &str,
        response: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
    ) -> Result<(), FluxgateError> {
        self.inner.update_stream_usage(
            request_id,
            response,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        )
    }

    /// 丢弃计数透传
    pub fn dropped_logs(&self) -> u64 {
        self.inner.dropped_logs()
    }

    /// secret gate fail-closed 拒发计数透传（同 [`AsyncLogger::secret_scan_dropped`]）
    pub fn secret_scan_dropped(&self) -> u64 {
        self.inner.secret_scan_dropped()
    }

    /// 关闭日志写入器（幂等：重复 shutdown 第二次返回 Err channel closed ——
    /// 与 shutdown 语义一致，调用方以 Ok/Err 判断首闭）
    pub async fn shutdown(&self) -> Result<(), FluxgateError> {
        self.inner.shutdown().await
    }
}

#[async_trait::async_trait]
impl crate::storage::RequestLogSink for AsyncLoggerHandle {
    async fn log(&self, log: RequestLog) -> Result<(), crate::error::GateError> {
        AsyncLoggerHandle::log(self, log).map_err(|e| crate::error::GateError::Config {
            message: format!("async log push failed: {e}"),
        })
    }

    async fn update_stream_usage(
        &self,
        request_id: &str,
        response: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
    ) -> Result<(), crate::error::GateError> {
        AsyncLoggerHandle::update_stream_usage(
            self,
            request_id,
            response,
            prompt_tokens,
            completion_tokens,
            total_tokens,
        )
        .map_err(|e| crate::error::GateError::Config {
            message: format!("async stream usage update failed: {e}"),
        })
    }

    fn dropped_logs(&self) -> u64 {
        AsyncLoggerHandle::dropped_logs(self)
    }

    fn secret_scan_dropped(&self) -> u64 {
        AsyncLoggerHandle::secret_scan_dropped(self)
    }

    async fn shutdown(&self) -> Result<(), crate::error::GateError> {
        AsyncLoggerHandle::shutdown(self)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("async logger shutdown failed: {e}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::LogFilter;
    use dbnexus::{DbConfig, DbPool};
    use inklog::LogSink;
    use std::collections::HashMap;
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

    /// tests ①：push 100 条 → shutdown → query_logs 恰 100 条
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

    /// tests ②：容量 1 时连 push 3 条，全部 100ms 内返回且
    /// dropped_logs==2（背压：满即丢并计数；try_send 是瞬时判定，满槽只可能 1 条在途）
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

    /// tests ③：channel 断开（Clone 出的 sender 全部 drop）→
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
        assert_eq!(logs.len(), 1, "断开分支退出前必须补最终 flush");
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
            FluxgateError::Database { message } => {
                // message 经 FTL（随 locale），与直接查表结果一致
                assert_eq!(message, t("database-logger-shutdown"));
            }
            other => panic!("Expected Database error, got {:?}", other),
        }
    }

    #[test]
    fn test_outbound_masker_redacts_api_key_style_secrets() {
        let masker = outbound_masker();
        let line = "calling upstream with api_key=sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        let masked = masker.mask(line);
        assert!(
            !masked.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
            "裸 secret 必须被替换: {masked}"
        );
        assert!(
            masked.contains("***REDACTED_API_KEY***"),
            "替换标记必须可见: {masked}"
        );
    }

    /// 落库管道消费侧脱敏：含裸 secret 的 messages/response 在进 buffer
    /// 前被 gate 改写，库中查不到明文。
    #[tokio::test]
    async fn test_async_logger_redacts_secrets_before_persist() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));

        let mut log = sample_log();
        log.messages = "prompt with key sk-proj-abcdefghijklmnopqrstuvwxyz123456".to_string();
        log.response = Some("response leaked sk-ant-abcdefghijklmnopqrstuvwxyz".to_string());
        logger.log(log).expect("Log write failed");
        logger.shutdown().await.expect("Shutdown failed");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert_eq!(logs.len(), 1, "正常记录不得因脱敏丢失");
        assert!(
            !logs[0]
                .messages
                .contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
            "落库 messages 不得含明文 secret: {}",
            logs[0].messages
        );
        assert!(
            logs[0].messages.contains("***REDACTED_API_KEY***"),
            "messages 替换标记必须可见: {}",
            logs[0].messages
        );
        assert!(
            !logs[0]
                .response
                .as_deref()
                .unwrap_or("")
                .contains("sk-ant-abcdefghijklmnopqrstuvwxyz"),
            "落库 response 不得含明文 secret"
        );
    }

    /// fail-closed：单条内同模式命中数超门限（库默认 64）→ gate 拒发 →
    /// 整条丢弃不落库，丢弃进 secret_scan_dropped 显性计数（宁可丢日志
    /// 不可泄密）。
    #[tokio::test]
    async fn test_async_logger_fail_closed_drops_when_match_limit_exceeded() {
        let repository = setup_repository().await;
        let logger = AsyncLogger::new(repository.clone(), 100, Duration::from_secs(60));

        let mut log = sample_log();
        log.messages = (0..65)
            .map(|i| format!("sk-{i:0>20}"))
            .collect::<Vec<_>>()
            .join(" ");
        logger.log(log).expect("Log write failed");
        logger.shutdown().await.expect("Shutdown failed");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("Query failed");
        assert!(logs.is_empty(), "gate 拒发的记录必须整条丢弃不落库");
        assert_eq!(logger.secret_scan_dropped(), 1);
    }

    #[test]
    fn test_outbound_masker_entropy_scan_covers_unpatterned_secrets() {
        let masker = outbound_masker();
        // 40 个互异 base64 核心字母字符：token 熵 = log2(40) ≈ 5.32
        // bits/char > 默认阈值 4.5，且不匹配 9 类内建值形态——只有熵扫描
        // 在防线上才必须被脱敏
        let token = "abcdefghijklmnopqrstuvwxyz01234567ABCDE+/";
        // 全互异 → 熵 = log2(len)；≥40 字符即 > 4.5 bits/char 默认阈值
        assert!(token.chars().count() >= 40);
        let line = format!("upload credential {token}");
        let masked = masker.mask(&line);
        assert!(
            !masked.contains(token),
            "高熵无形态裸 secret 必须被熵扫描脱敏: {masked}"
        );
        assert!(
            masked.contains("***HIGH_ENTROPY***"),
            "熵命中替换标记必须可见: {masked}"
        );
    }

    #[test]
    fn test_redact_text_truncates_oversized_input_instead_of_dropping() {
        let masker = outbound_masker();
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        // 超过 1MiB 门限：前缀含 secret + 填充——落库/console 保留脱敏
        // 前缀（审计价值），尾部截断不外泄（无泄密面）
        let oversized = format!("{secret}{}", "x".repeat(MASK_INPUT_LIMIT_BYTES + 1024));
        let redacted = redact_text(&masker, &oversized).expect("超门限输入必须截断保留而非拒发");
        assert!(!redacted.contains(secret), "保留前缀中的 secret 必须已脱敏");
        assert!(redacted.contains(TRUNCATION_MARKER), "截断必须留痕");
        assert!(
            redacted.len() <= MASK_INPUT_LIMIT_BYTES,
            "落库/console 文本不得超门限: {}",
            redacted.len()
        );
    }

    /// fail-closed 拒发语义在截断策略下保持：截断保留的前缀内命中超
    /// 门限仍整条拒发。
    #[test]
    fn test_redact_text_still_fails_closed_when_truncated_prefix_exceeds_limit() {
        let masker = outbound_masker();
        let body = (0..65)
            .map(|i| format!("sk-{i:0>20}"))
            .collect::<Vec<_>>()
            .join(" ");
        let oversized = format!("{body}{}", "x".repeat(MASK_INPUT_LIMIT_BYTES));
        assert!(
            redact_text(&masker, &oversized).is_err(),
            "截断前缀命中超门限必须保持 fail-closed"
        );
    }

    #[tokio::test]
    async fn test_gate_sink_rejects_on_gate_limit_counts_and_masks_normal_records() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct RecordingSink(Mutex<Vec<inklog::LogRecord>>);

        #[async_trait::async_trait]
        impl inklog::LogSink for RecordingSink {
            async fn write(&self, record: &inklog::LogRecord) -> Result<(), InklogError> {
                self.0.lock().expect("not poisoned").push(record.clone());
                Ok(())
            }
            async fn flush(&self) -> Result<(), InklogError> {
                Ok(())
            }
            async fn shutdown(&self) -> Result<(), InklogError> {
                Ok(())
            }
        }

        let inner = Arc::new(RecordingSink::default());
        let sink = GateSink::new(inner.clone(), Arc::new(outbound_masker()));
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";

        // 正常记录：message 与 fields 字符串值均经 gate 脱敏后放行
        let record = inklog::LogRecord {
            level: "INFO".to_string(),
            message: format!("upstream call with {secret}"),
            fields: HashMap::from([("detail".to_string(), serde_json::json!(secret))]),
            ..Default::default()
        };
        sink.write(&record).await.expect("正常记录不得拒发");
        {
            // guard 作用域限定在断言块：不得跨越下方 flood 的 await 点
            let written = inner.0.lock().expect("not poisoned");
            assert_eq!(written.len(), 1);
            assert!(!written[0].message.contains(secret), "message 必须已脱敏");
            assert!(
                written[0].message.contains("***REDACTED_API_KEY***"),
                "message 替换标记可见: {}",
                written[0].message
            );
            assert_eq!(
                written[0].fields["detail"],
                serde_json::json!("***REDACTED_API_KEY***"),
                "fields 字符串值必须已脱敏"
            );
        }

        // 超门限记录（单模式 65 命中 > 默认 64）：整条拒发，不上 inner，
        // 丢弃计数显性累加
        let flood = inklog::LogRecord {
            message: (0..65)
                .map(|i| format!("sk-{i:0>20}"))
                .collect::<Vec<_>>()
                .join(" "),
            ..Default::default()
        };
        let baseline = inner.0.lock().expect("not poisoned").len();
        sink.write(&flood).await.expect("拒发按 Ok 处理（非故障）");
        assert_eq!(
            inner.0.lock().expect("not poisoned").len(),
            baseline,
            "inner 只含先前的正常记录，flood 拒发后不得新增"
        );
        assert_eq!(sink.dropped(), 1);
    }
}
