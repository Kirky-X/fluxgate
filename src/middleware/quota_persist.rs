// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 配额账本落盘管道（`quota_ledger` 表的 write-behind 前沿，quota-aware-routing）。
//!
//! 与 `AffinityPersister` 同构：结算点 `write` 入队即返回，通道满丢弃并计数
//!（预算是约束不是计费——落盘误差显性披露，不阻塞请求路径），消费侧批量
//! UPSERT；另承载 30s 周期全量 flush 的写入面（快照生产方在 bootstrap）。
//! 失败只计数 + warn，不重试、不上浮——内存态始终权威。
//!
//! 崩溃一致性窗口 ≤ 一个 flush 周期（30s）：重启后已用量最多回退 30s
//!（USER_GUIDE 披露）。

use crate::database::quota_ledger::QuotaLedgerRow;
use crate::database::Repository;
use crate::error::GateError;
use crate::i18n::t_args;
use async_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 通道槽位（与日志/粘性管道同档：1024 槽 ≈ 结算事件的合理在途量）
pub const DEFAULT_LEDGER_SLOTS: usize = 1024;
/// 定时 flush 周期（design 决策 3：30s——崩溃回退窗口的上界）
pub const DEFAULT_LEDGER_FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// 落盘命令
enum LedgerCommand {
    Write(Vec<QuotaLedgerRow>),
    Shutdown,
}

/// 配额账本落盘器（write-behind 前沿）。Clone 廉价（sender + Arc 原子计数）。
#[derive(Clone)]
pub struct QuotaLedgerPersister {
    repository: Arc<Repository>,
    sender: Sender<LedgerCommand>,
    /// 落库失败批次数（flush 失败计数不阻塞——`quota_ledger_flush_fail_total`
    /// 的进程内读数，与 /admin/metrics 出口同源）
    flush_failures: Arc<AtomicU64>,
    /// 已关闭标志：置位后的投递一律计入失败（避免停机后新记录被静默塞进
    /// 已死通道——与 async_logger 的 closed 语义同款）
    closed: Arc<AtomicBool>,
    metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
}

impl QuotaLedgerPersister {
    /// 构造并启动后台 flush 任务（容量/周期显式传入，缺省值见
    /// [`DEFAULT_LEDGER_SLOTS`] / [`DEFAULT_LEDGER_FLUSH_INTERVAL`]）。
    pub fn new(
        repository: Arc<Repository>,
        capacity: usize,
        flush_interval: Duration,
        metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
    ) -> Self {
        let (sender, receiver) = bounded(capacity);
        let failures = Arc::new(AtomicU64::new(0));
        let loop_failures = Arc::clone(&failures);
        let loop_repo = Arc::clone(&repository);
        let loop_metrics = metrics.clone();
        tokio::spawn(async move {
            writer_loop(
                receiver,
                loop_repo,
                flush_interval,
                loop_failures,
                loop_metrics,
            )
            .await;
        });
        Self {
            repository,
            sender,
            flush_failures: failures,
            closed: Arc::new(AtomicBool::new(false)),
            metrics,
        }
    }

    /// 记录一批账本行（非阻塞 push；通道满/已关闭丢弃并计数——预算约束的
    /// 落盘是尽力而为，内存态权威）。
    pub fn write(&self, rows: Vec<QuotaLedgerRow>) {
        if rows.is_empty() {
            return;
        }
        if self.closed.load(Ordering::Relaxed) {
            self.count_failure();
            return;
        }
        match self.sender.try_send(LedgerCommand::Write(rows)) {
            Ok(()) => {}
            Err(async_channel::TrySendError::Full(_) | async_channel::TrySendError::Closed(_)) => {
                self.count_failure();
            }
        }
    }

    /// 确定性落盘点（测试与 30s 全量 flush 复用）：把给定批次直接 UPSERT
    /// 落库，不经通道。失败计数 + 上浮（调用方决定处置），不重试。
    pub async fn flush_now(&self, rows: &[QuotaLedgerRow]) -> Result<(), GateError> {
        if rows.is_empty() {
            return Ok(());
        }
        if let Err(e) = self.repository.upsert_quota_ledger(rows).await {
            self.count_failure();
            return Err(GateError::Config {
                message: t_args(
                    "middleware-quota-ledger-flush-failed",
                    &[("count", rows.len().to_string()), ("error", e.to_string())],
                ),
            });
        }
        Ok(())
    }

    /// 落库失败批次数（进程内读数）。
    pub fn flush_failures(&self) -> u64 {
        self.flush_failures.load(Ordering::Relaxed)
    }

    fn count_failure(&self) {
        self.flush_failures.fetch_add(1, Ordering::Relaxed);
        if let Some(metrics) = &self.metrics {
            metrics.record_quota_ledger_flush_fail();
        }
    }

    /// 优雅关闭：先置 closed（后续投递计入失败），再发 Shutdown 令后台任务
    /// 补最终 flush 后退出。
    pub async fn shutdown(&self) -> Result<(), GateError> {
        self.closed.store(true, Ordering::Relaxed);
        self.sender
            .send(LedgerCommand::Shutdown)
            .await
            .map_err(|_| GateError::Config {
                message: t_args(
                    "middleware-quota-ledger-channel-closed",
                    &[("error", "sender closed".to_string())],
                ),
            })
    }
}

/// 后台 flush 循环：到点或收到命令即落库，Shutdown / 通道断开时退出
///（write 路径行数即批，无独立缓冲——批次在命令里自带）。
async fn writer_loop(
    receiver: Receiver<LedgerCommand>,
    repository: Arc<Repository>,
    _flush_interval: Duration,
    flush_failures: Arc<AtomicU64>,
    metrics: Option<Arc<crate::utils::metrics::GlobalMetrics>>,
) {
    while let Ok(command) = receiver.recv().await {
        match command {
            LedgerCommand::Write(rows) => {
                flush_rows(&repository, rows, &flush_failures, &metrics).await;
            }
            LedgerCommand::Shutdown => break,
        }
    }
}

/// 批量 UPSERT（空批短路）。失败计数 + warn——预算落盘尽力而为，内存态
/// 权威，不重试。
async fn flush_rows(
    repository: &Arc<Repository>,
    rows: Vec<QuotaLedgerRow>,
    flush_failures: &Arc<AtomicU64>,
    metrics: &Option<Arc<crate::utils::metrics::GlobalMetrics>>,
) {
    if rows.is_empty() {
        return;
    }
    let count = rows.len();
    if let Err(e) = repository.upsert_quota_ledger(&rows).await {
        flush_failures.fetch_add(1, Ordering::Relaxed);
        if let Some(metrics) = metrics {
            metrics.record_quota_ledger_flush_fail();
        }
        tracing::warn!(
            "{}",
            t_args(
                "middleware-quota-ledger-flush-failed",
                &[("count", count.to_string()), ("error", e.to_string())]
            )
        );
    }
}

#[cfg(all(test, feature = "db-sqlite"))]
mod tests {
    use super::*;
    use crate::database::Repository;
    use dbnexus::{DbConfig, DbPool};
    use sea_orm::{ConnectionTrait, Statement};

    async fn repository() -> Arc<Repository> {
        let pool = DbPool::with_config(DbConfig {
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
        .expect("memory pool");
        let repo = Arc::new(Repository::new(pool));
        repo.initialize().await.expect("init schema");
        repo
    }

    fn row(key: &str, window: &str, start: i64, tokens: i64) -> QuotaLedgerRow {
        QuotaLedgerRow {
            api_key_hash: key.into(),
            window_id: window.into(),
            window_start: start,
            tokens_used: tokens,
            cost_used_millicents: 0,
            updated_at: 1_770_000_000,
        }
    }

    /// 结算落库 + 启动恢复（quota-aware-routing T005）：write 入队 → 后台
    /// 落库；`__global__` / `__vendor__:{id}` 作用域行同表同路；恢复只取
    /// 未过期窗（活动期由 QuotaState 按配置周期解释）。
    #[tokio::test]
    async fn settle_upserts_ledger_and_boot_restores_active_windows() {
        let repo = repository().await;
        let persister = QuotaLedgerPersister::new(
            repo.clone(),
            DEFAULT_LEDGER_SLOTS,
            DEFAULT_LEDGER_FLUSH_INTERVAL,
            None,
        );

        // 三类作用域行同路落库：租户 / 全局 / vendor 预算（design 决策 3/4）
        persister.write(vec![
            row("tenant-digest", "daily", 1_770_000_000, 1200),
            row("__global__", "fleet", 1_770_000_000, 700),
            row("__vendor__:v-a", "vendor:v-a", 1_770_000_000, 500),
        ]);
        // 轮询等待后台消费（try_send 后台异步落库，测试用确定性等待）
        for _ in 0..100 {
            let rows = repo.read_quota_ledger().await.expect("read");
            if rows.len() == 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let rows = repo.read_quota_ledger().await.expect("read");
        assert_eq!(rows.len(), 3, "三类作用域行全部落库");
        assert!(
            rows.iter()
                .any(|r| r.api_key_hash == "__vendor__:v-a" && r.window_id == "vendor:v-a"),
            "vendor 作用域行 window_id=vendor:{{id}}"
        );
        persister.shutdown().await.expect("shutdown");

        // 启动恢复：活动窗（window_start + 周期 ≥ now）才恢复。
        // 构造 daily 窗配置 + 三行账本（一行过期、两行活动）。
        let state = crate::middleware::quota::QuotaState::new();
        let config = test_quota_config();
        let rows = vec![
            QuotaLedgerRow {
                api_key_hash: "tenant-digest".into(),
                window_id: "daily".into(),
                window_start: crate::middleware::quota::Period::Daily
                    .window_start(now_secs(), 0)
                    .expect("daily 起点"),
                tokens_used: 1200,
                cost_used_millicents: 40,
                updated_at: now_secs(),
            },
            QuotaLedgerRow {
                api_key_hash: "tenant-digest".into(),
                window_id: "daily".into(),
                // 过期行：三天前的周期实例
                window_start: now_secs() - 3 * 86_400,
                tokens_used: 999,
                cost_used_millicents: 0,
                updated_at: now_secs() - 3 * 86_400,
            },
            QuotaLedgerRow {
                api_key_hash: "__global__".into(),
                window_id: "fleet".into(),
                window_start: crate::middleware::quota::Period::Daily
                    .window_start(now_secs(), 0)
                    .expect("daily 起点"),
                tokens_used: 700,
                cost_used_millicents: 0,
                updated_at: now_secs(),
            },
        ];
        let restored = state.restore_from_ledger(&rows, &config);
        assert_eq!(restored, 2, "仅恢复未过期窗（过期行跳过）");
        assert_eq!(
            state.snapshot("tenant-digest", "daily"),
            (1200, 40),
            "恢复窗占用与账本一致"
        );
        assert_eq!(
            state.snapshot(crate::middleware::quota::GLOBAL_LEDGER_KEY, "fleet"),
            (700, 0),
            "global 作用域行恢复同路"
        );
        assert_eq!(
            state.snapshot("tenant-digest", "daily").0,
            1200,
            "过期行不得覆盖活动行"
        );
        let _ = config;
    }

    /// flush 失败计数不阻塞（quota-aware-routing T005）：表被删 → UPSERT
    /// 失败 → 计数递增；write 路径不 panic 不上浮（内存态权威）。
    #[tokio::test]
    async fn flush_fail_counted_not_blocking() {
        let repo = repository().await;
        let persister = QuotaLedgerPersister::new(
            repo.clone(),
            DEFAULT_LEDGER_SLOTS,
            DEFAULT_LEDGER_FLUSH_INTERVAL,
            None,
        );
        assert_eq!(persister.flush_failures(), 0);

        // 注入故障：删表使 UPSERT 失败（确定性错误源）。session 显式落 scope——
        // max_connections=1 的测试池上，持住 session 会饿死 flush 侧的 get_session
        {
            let session = repo.pool.get_session("admin").await.expect("session");
            session
                .connection()
                .expect("connection")
                .execute_raw(Statement::from_string(
                    sea_orm::DatabaseBackend::Sqlite,
                    "DROP TABLE quota_ledger".to_string(),
                ))
                .await
                .expect("drop table");
        }

        // 直接落库点：Err 上浮 + 计数
        let result = persister
            .flush_now(&[row("k", "daily", 1_770_000_000, 1)])
            .await;
        assert!(result.is_err(), "删表后 UPSERT 必须失败");
        assert_eq!(persister.flush_failures(), 1, "失败必须计数");

        // write 路径：失败仅计数，不 panic 不阻塞（调用方无感知）
        persister.write(vec![row("k", "daily", 1_770_000_000, 2)]);
        for _ in 0..100 {
            if persister.flush_failures() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(persister.flush_failures(), 2, "write 路径失败同样计数");
        persister.shutdown().await.expect("shutdown");
    }

    fn now_secs() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn test_quota_config() -> crate::config::Config {
        crate::config::Config {
            rate_limit: Default::default(),
            otlp: Default::default(),
            vendor_budgets: Vec::new(),
            usage_insights: Default::default(),
            metrics: Default::default(),
            server: crate::config::ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: crate::config::QuotaSection {
                enabled: true,
                windows: vec![
                    crate::config::QuotaWindowConfig {
                        id: "daily".into(),
                        period: "calendar".into(),
                        calendar: "daily".into(),
                        tokens_limit: 1_000_000,
                        cost_limit_millicents: 100_000,
                        ..Default::default()
                    },
                    crate::config::QuotaWindowConfig {
                        id: "fleet".into(),
                        scope: "global".into(),
                        rolling_secs: 86_400,
                        tokens_limit: 100_000,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
        }
    }
}
