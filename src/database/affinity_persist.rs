// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 会话粘性落盘管道（`affinity_sticks` 表的 write-behind 前沿）。
//!
//! 与 `async_logger` 同构：settle 点 `try_send` 入队即返回，通道满丢弃并
//! 计数（亲和是优化非承诺，丢不阻塞请求），消费侧批量 UPSERT。三方言 UPSERT
//! 单源 [`upsert_affinity_sticks_sql`]，参数全部绑定（无 SQL 拼接面）。
//!
//! 管道前沿与存储读侧同体（与 `ResponseStoreSink` 的 Repository 实现同款）：
//! 写半边走通道，读半边（`read_unexpired`）直连库供启动恢复调用。

use crate::database::Repository;
use crate::error::GateError;
use crate::i18n::t_args;
use crate::storage::AffinityStick;
use async_channel::{bounded, Receiver, Sender};
use sea_orm::DatabaseBackend;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 通道槽位（与日志管道同档：1024 槽 ≈ 会话级事件的合理在途量）
pub const DEFAULT_SLOTS: usize = 1024;
/// 单次 flush 的批阈值（条）：达到即提前落库（与日志管道同值）
pub const DEFAULT_FLUSH_BUFFER: usize = 100;
/// 定时 flush 周期（1s，与日志管道同档）
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// 粘性落盘命令
enum StickCommand {
    Write(AffinityStick),
    Shutdown,
}

/// 粘性落盘器（write-behind 前沿 + 存储读侧）。Clone 廉价
///（sender + Arc 仓库 + 原子计数）。
#[derive(Clone)]
pub struct AffinityPersister {
    repository: Arc<Repository>,
    sender: Sender<StickCommand>,
    /// 通道满被丢弃的粘性条数（暴露进 /admin/metrics 的
    /// `affinity_persist_drop_total`）
    pub(crate) dropped_sticks: Arc<AtomicU64>,
    /// 落库失败的批次数（与通道丢弃分列，原因不同）
    flush_failures: Arc<AtomicU64>,
    /// 已关闭标志：置位后的投递一律计入丢弃（与 `async_logger` 的 closed
    /// 语义同款——避免停机后新记录被静默塞进已死通道）
    closed: Arc<AtomicBool>,
}

impl AffinityPersister {
    /// 构造并启动后台 flush 任务（容量/周期显式传入，缺省值见
    /// [`DEFAULT_SLOTS`] / [`DEFAULT_FLUSH_INTERVAL`]）
    pub fn new(repository: Arc<Repository>, capacity: usize, flush_interval: Duration) -> Self {
        let (sender, receiver) = bounded(capacity);
        let failures = Arc::new(AtomicU64::new(0));
        let loop_failures = Arc::clone(&failures);
        let loop_repo = Arc::clone(&repository);
        tokio::spawn(async move {
            writer_loop(receiver, loop_repo, flush_interval, loop_failures).await;
        });
        Self {
            repository,
            sender,
            dropped_sticks: Arc::new(AtomicU64::new(0)),
            flush_failures: failures,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 优雅关闭：先置 closed 标志（后续投递直接计入丢弃，不塞进已死通道）
    /// → 发 Shutdown → 后台任务补最终 flush 后退出。
    pub async fn shutdown(&self) -> Result<(), GateError> {
        self.closed.store(true, Ordering::Relaxed);
        self.sender
            .send(StickCommand::Shutdown)
            .await
            .map_err(|_| GateError::Config {
                message: t_args(
                    "database-affinity-persist-channel-closed",
                    &[("error", "sender closed".to_string())],
                ),
            })
    }

    /// 确定性落盘点（测试与运维）：把给定批次直接 UPSERT 落库，不经通道。
    /// 与后台循环共用 [`flush_buffer`] 的失败处理口径（整批丢弃 + 计数 +
    /// warn，不重试）。
    pub async fn flush_now(&self, sticks: &[AffinityStick]) -> Result<(), GateError> {
        if sticks.is_empty() {
            return Ok(());
        }
        self.repository
            .upsert_affinity_sticks(sticks)
            .await
            .map_err(|e| GateError::Config {
                message: t_args(
                    "database-affinity-persist-flush-failed",
                    &[
                        ("count", sticks.len().to_string()),
                        ("error", e.to_string()),
                    ],
                ),
            })
    }
}

#[async_trait::async_trait]
impl crate::storage::AffinityStickSink for AffinityPersister {
    /// 记录一条粘性（非阻塞 push；通道满丢弃并计数，不阻塞请求路径）
    fn record(&self, stick: AffinityStick) {
        if self.closed.load(Ordering::Relaxed) {
            self.dropped_sticks.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match self.sender.try_send(StickCommand::Write(stick)) {
            Ok(()) => {}
            // 亲和是优化非承诺：满即丢弃并计数，绝不等待（含通道已关闭）
            Err(async_channel::TrySendError::Full(_) | async_channel::TrySendError::Closed(_)) => {
                self.dropped_sticks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn dropped_sticks(&self) -> u64 {
        self.dropped_sticks.load(Ordering::Relaxed)
    }

    fn flush_failures(&self) -> u64 {
        self.flush_failures.load(Ordering::Relaxed)
    }

    /// 读取未过期的前 `limit` 条（按 `expires_at` 降序——最新写入优先，
    /// 超出上限时优先保留刚服务过的会话）
    async fn read_unexpired(&self, now: i64, limit: u32) -> Result<Vec<AffinityStick>, GateError> {
        self.repository
            .read_affinity_sticks(now, limit)
            .await
            .map_err(|e| GateError::Config {
                message: t_args(
                    "database-affinity-sticks-read-failed",
                    &[("error", e.to_string())],
                ),
            })
    }
}

/// 后台 flush 循环：满 [`DEFAULT_FLUSH_BUFFER`] 条或到点即落库，
/// Shutdown / 通道断开时补最终 flush 再退出（不丢缓冲）。
async fn writer_loop(
    receiver: Receiver<StickCommand>,
    repository: Arc<Repository>,
    flush_interval: Duration,
    flush_failures: Arc<AtomicU64>,
) {
    let mut buffer: Vec<AffinityStick> = Vec::with_capacity(DEFAULT_FLUSH_BUFFER);
    let mut interval = tokio::time::interval(flush_interval);
    // interval 首个 tick 立即完成；先消费掉它，否则 select 里 tick 分支恒
    // 就绪、与 recv 竞逐会把批次打碎（与 async_logger 同款）
    interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                flush_buffer(&repository, &mut buffer, &flush_failures).await;
            }
            command = receiver.recv() => {
                match command {
                    Ok(StickCommand::Write(stick)) => {
                        buffer.push(stick);
                        if buffer.len() >= DEFAULT_FLUSH_BUFFER {
                            flush_buffer(&repository, &mut buffer, &flush_failures).await;
                        }
                    }
                    Ok(StickCommand::Shutdown) | Err(_) => {
                        flush_buffer(&repository, &mut buffer, &flush_failures).await;
                        break;
                    }
                }
            }
        }
    }
}

/// 批量 UPSERT（空批短路）。失败整批丢弃并计数 + warn——粘性是优化，
/// 不重试、不阻塞请求路径。
async fn flush_buffer(
    repository: &Arc<Repository>,
    buffer: &mut Vec<AffinityStick>,
    flush_failures: &Arc<AtomicU64>,
) {
    if buffer.is_empty() {
        return;
    }
    let count = buffer.len();
    if let Err(e) = repository.upsert_affinity_sticks(buffer.as_slice()).await {
        flush_failures.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "{}",
            t_args(
                "database-affinity-persist-flush-failed",
                &[("count", count.to_string()), ("error", e.to_string())]
            )
        );
    }
    buffer.clear();
}

/// 三方言 UPSERT 语句（纯函数，供实现与测试断言共用）：
/// sqlite（3.24+）与 postgres 走 `ON CONFLICT DO UPDATE`（`excluded` 引用
/// 新行）；mysql 走 `ON DUPLICATE KEY UPDATE` + `VALUES(col)`。
/// 五列全部 `?` 绑定（跨方言由 sea-orm 按后端改写占位符）。
pub fn upsert_affinity_sticks_sql(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::MySql => {
            r#"
            INSERT INTO affinity_sticks (tenant, session_key, vendor_id, worth_tokens, expires_at)
            VALUES (?, ?, ?, ?, ?)
            ON DUPLICATE KEY UPDATE
                vendor_id = VALUES(vendor_id),
                worth_tokens = VALUES(worth_tokens),
                expires_at = VALUES(expires_at)
        "#
        }
        // DatabaseBackend 带 #[non_exhaustive]：其余（sqlite/postgres）同走
        // ON CONFLICT；存量行引用按表名限定——postgres 的 DO UPDATE 表达式里
        // 裸列名在「目标表 vs excluded」间有歧义（与 token_usage 同款）
        _ => {
            r#"
            INSERT INTO affinity_sticks (tenant, session_key, vendor_id, worth_tokens, expires_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(tenant, session_key)
            DO UPDATE SET
                vendor_id = excluded.vendor_id,
                worth_tokens = excluded.worth_tokens,
                expires_at = excluded.expires_at
        "#
        }
    }
}

/// 未过期读取语句（纯函数）：按 `expires_at` 降序取前 N 条——启动恢复的
/// 「top 512」由 `LIMIT` 下推，不把全表拉进内存。
pub fn read_unexpired_sql(_backend: DatabaseBackend) -> &'static str {
    r#"
    SELECT tenant, session_key, vendor_id, worth_tokens, expires_at
    FROM affinity_sticks
    WHERE expires_at > ?
    ORDER BY expires_at DESC
    LIMIT ?
"#
}

/// 过期行清理语句（纯函数）
pub fn purge_expired_sql(_backend: DatabaseBackend) -> &'static str {
    r#"DELETE FROM affinity_sticks WHERE expires_at <= ?"#
}

#[cfg(test)]
mod tests;
