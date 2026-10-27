// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 粘性落盘通道的锁定测试（TDD Red → Green）。
//!
//! 契约：通道是优化非承诺——满即丢弃并计数，绝不阻塞请求路径。

use super::{upsert_affinity_sticks_sql, AffinityPersister, DEFAULT_FLUSH_BUFFER, DEFAULT_SLOTS};
use crate::storage::{AffinityStick, AffinityStickSink};
use sea_orm::DatabaseBackend;
use std::sync::Arc;
use std::time::Duration;

async fn repository() -> Arc<crate::database::Repository> {
    let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
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
    let repo = Arc::new(crate::database::Repository::new(pool));
    repo.initialize().await.expect("schema init");
    repo
}

fn stick(session: &str, vendor: &str, expires_at: i64) -> AffinityStick {
    AffinityStick {
        tenant: String::new(),
        session_key: session.into(),
        vendor_id: vendor.into(),
        worth_tokens: 1_024,
        expires_at,
    }
}

/// 批处理：100 条触发批写（不等 1s 定时），最终落库可查
#[tokio::test]
async fn events_batch_flush_and_overflow_drops_counted() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));

    let mut buffer = Vec::new();
    for i in 0..DEFAULT_FLUSH_BUFFER {
        buffer.push(stick(&format!("s{i}"), "v1", 1_700_000_000 + i as i64));
    }
    // 满 100 条立即 flush（不等 60s 定时）
    persister.flush_now(&buffer).await.expect("flush");

    let rows = persister.read_unexpired(0, 1_000).await.expect("read back");
    assert_eq!(rows.len(), DEFAULT_FLUSH_BUFFER, "整批必须落库");
    assert_eq!(
        persister.dropped_sticks(),
        0,
        "容量 1024 > 100 条，不得丢弃"
    );

    // 通道容量 1 时连发 200 条：溢出丢弃被计数，且不阻塞
    let narrow = Arc::new(AffinityPersister::new(
        repo.clone(),
        1,
        Duration::from_secs(60),
    ));
    let start = std::time::Instant::now();
    for i in 0..200 {
        narrow.record(stick(&format!("n{i}"), "v2", 1_700_000_000 + i as i64));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "record 不得阻塞请求路径（实测 {elapsed:?}）"
    );
    assert!(
        narrow.dropped_sticks() > 0,
        "窄通道必须丢弃并计数（实测丢弃 {}）",
        narrow.dropped_sticks()
    );
}

/// 1s 定时 flush：不发满 100 条也能落库（时间触发路径）
#[tokio::test]
async fn interval_flush_persists_without_buffer_full() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_millis(50),
    ));
    persister.record(stick("solo", "v1", 1_700_000_000));
    tokio::time::sleep(Duration::from_millis(250)).await;

    let rows = persister.read_unexpired(0, 10).await.expect("read");
    assert_eq!(rows.len(), 1, "定时 flush 必须落库");
    assert_eq!(rows[0].session_key, "solo");
}

/// UPSERT 幂等：同 (tenant, session) 重复落库保留最后写入的 vendor
#[tokio::test]
async fn upsert_overwrites_same_key() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));
    persister
        .flush_now(&[stick("s", "v1", 1), stick("s", "v2", 2)])
        .await
        .expect("flush");

    let rows = persister.read_unexpired(0, 10).await.expect("read");
    assert_eq!(rows.len(), 1, "复合主键 UPSERT 不产生第二行");
    assert_eq!(rows[0].vendor_id, "v2", "保留最后写入的 vendor");
    assert_eq!(rows[0].expires_at, 2);

    // 租户维度隔离：同 session 不同租户是两行
    persister
        .flush_now(&[AffinityStick {
            tenant: "tenant-a".into(),
            session_key: "s".into(),
            vendor_id: "v3".into(),
            worth_tokens: 0,
            expires_at: 3,
        }])
        .await
        .expect("flush");
    let rows = persister.read_unexpired(0, 10).await.expect("read");
    assert_eq!(rows.len(), 2, "租户键与 legacy 键互不可见");
}

/// 落盘不阻塞：通道满时 `record` 立即返回（背压语义，与日志管道同款）
#[tokio::test]
async fn full_channel_never_blocks() {
    let repo = repository().await;
    let persister = AffinityPersister::new(repo, 4, Duration::from_secs(60));
    let start = std::time::Instant::now();
    for i in 0..500 {
        persister.record(stick(&format!("f{i}"), "v", i));
    }
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "record 必须瞬时返回（实测 {:?}）",
        start.elapsed()
    );
    assert!(persister.dropped_sticks() > 0);
}

/// 方言分派：sqlite/pg 走 ON CONFLICT，mysql 走 ON DUPLICATE KEY UPDATE
#[test]
fn upsert_dialect_dispatch() {
    for backend in [DatabaseBackend::Sqlite, DatabaseBackend::Postgres] {
        let sql = upsert_affinity_sticks_sql(backend);
        assert!(
            sql.contains("ON CONFLICT(tenant, session_key)"),
            "{backend:?} 冲突键必须是复合主键"
        );
        assert!(sql.contains("excluded.vendor_id"), "{backend:?}");
    }
    let sql = upsert_affinity_sticks_sql(DatabaseBackend::MySql);
    assert!(sql.contains("ON DUPLICATE KEY UPDATE"), "mysql 走 DKU");
    for col in ["vendor_id", "worth_tokens", "expires_at"] {
        assert!(
            sql.contains(&format!("{col} = VALUES({col})")),
            "缺列 {col}"
        );
    }
    // 五列全部参数化（无字符串拼接、无 SQL 注入面）
    for sql in [
        upsert_affinity_sticks_sql(DatabaseBackend::Sqlite),
        upsert_affinity_sticks_sql(DatabaseBackend::MySql),
    ] {
        assert_eq!(sql.matches('?').count(), 5, "五列必须全部绑定参数: {sql}");
    }
}

/// 读取上限：只取前 `limit` 条（启动恢复的 512 上限由本契约保证）
#[tokio::test]
async fn read_respects_limit() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));
    let buffer: Vec<AffinityStick> = (0..10)
        .map(|i| stick(&format!("l{i}"), "v", 1_700_000_000 + i as i64))
        .collect();
    persister.flush_now(&buffer).await.expect("flush");
    let rows = persister.read_unexpired(0, 3).await.expect("read");
    assert_eq!(rows.len(), 3, "limit 必须下推到 SQL");
    // 按 expires_at 降序：最新的三条
    assert_eq!(rows[0].expires_at, 1_700_000_009);
}

/// 读取只取未过期的行（过期行不参与恢复）
#[tokio::test]
async fn read_skips_expired() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));
    persister
        .flush_now(&[stick("old", "v1", 100), stick("fresh", "v2", 1_700_000_000)])
        .await
        .expect("flush");

    let rows = persister.read_unexpired(1_000, 10).await.expect("read");
    assert_eq!(rows.len(), 1, "过期行不得入恢复集");
    assert_eq!(rows[0].session_key, "fresh");
}

/// 关机后 record：通道已关 → 丢弃计数（不 panic、不阻塞）
#[tokio::test]
async fn record_after_shutdown_is_counted_not_panicking() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo,
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));
    persister.shutdown().await.expect("shutdown");
    persister.record(stick("late", "v", 1));
    assert_eq!(
        persister.dropped_sticks(),
        1,
        "关机后的记录必须计入丢弃（亲和是优化非承诺）"
    );
}

/// 流式/无 usage 路径不投递：投递只在 settle 内（本测试钉住 sink 契约——
/// `record` 是唯一写入口，重复记录由 UPSERT 收敛）
#[tokio::test]
async fn repeated_records_converge_in_db() {
    let repo = repository().await;
    let persister = Arc::new(AffinityPersister::new(
        repo.clone(),
        DEFAULT_SLOTS,
        Duration::from_secs(60),
    ));
    let buffer: Vec<AffinityStick> = (0..20).map(|_| stick("dup", "v9", 42)).collect();
    persister.flush_now(&buffer).await.expect("flush");
    let rows = persister.read_unexpired(0, 100).await.expect("read");
    assert_eq!(rows.len(), 1, "重复记录必须收敛为单行");
}
