// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 启动恢复的锁定测试（R-affinity-003）：过期行不恢复、超第 512 行不恢复、
//! legacy/off 模式不恢复。

use super::{restore_affinity_sticks, RESTORE_LIMIT};
use crate::config::AffinityMode;
use crate::router::affinity::{AffinityStickTable, AffinityTable, DEFAULT_AFFINITY_TTL};
use crate::storage::{AffinityStick, AffinityStickSink};
use crate::utils::metrics::GlobalMetrics;
use std::sync::Arc;

/// 内存替身：持有一批粘性行（模拟库中已落盘的未过期数据）
struct FakeStore(Vec<AffinityStick>);

#[async_trait::async_trait]
impl AffinityStickSink for FakeStore {
    fn record(&self, _stick: AffinityStick) {}

    async fn read_unexpired(
        &self,
        now: i64,
        limit: u32,
    ) -> Result<Vec<AffinityStick>, crate::error::GateError> {
        let mut rows: Vec<AffinityStick> = self
            .0
            .iter()
            .filter(|s| s.expires_at > now)
            .cloned()
            .collect();
        // 与 SQL 同序：expires_at 降序
        rows.sort_by_key(|b| std::cmp::Reverse(b.expires_at));
        rows.truncate(limit as usize);
        Ok(rows)
    }
}

fn stick(session: &str, vendor: &str, expires_at: i64) -> AffinityStick {
    AffinityStick {
        tenant: String::new(),
        session_key: session.into(),
        vendor_id: vendor.into(),
        worth_tokens: 2_048,
        expires_at,
    }
}

async fn run(
    mode: AffinityMode,
    rows: Vec<AffinityStick>,
) -> (
    Arc<AffinityTable>,
    Arc<AffinityStickTable>,
    Arc<GlobalMetrics>,
) {
    let affinity = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let sticks = Arc::new(AffinityStickTable::new());
    let metrics = Arc::new(GlobalMetrics::new());
    let sink: Arc<dyn AffinityStickSink> = Arc::new(FakeStore(rows));
    restore_affinity_sticks(mode, &affinity, &sticks, Some(&sink), &metrics).await;
    (affinity, sticks, metrics)
}

/// 未来时刻（测试用：now + 1h）
fn far_future() -> i64 {
    chrono::Utc::now().timestamp() + 3_600
}

/// 过期行不入内存；第 513 行（超出 512 上限）不入内存
#[tokio::test]
async fn boot_restores_unexpired_top512() {
    let now = chrono::Utc::now().timestamp();
    let mut rows = Vec::new();
    // 1 条过期（expires_at <= now）
    rows.push(stick("expired", "v-old", now - 10));
    // 恰好 512 条未过期（排序后最新）
    for i in 0..RESTORE_LIMIT {
        rows.push(stick(&format!("fresh-{i}"), "v1", far_future() + i as i64));
    }
    // 再 1 条：排序后落在第 513 位（更早的过期时刻 → 被 limit 截断）
    rows.push(stick("beyond-limit", "v2", far_future() - 1));

    let (affinity, sticks, metrics) = run(AffinityMode::Auto, rows).await;

    // 备注：FakeStore 与 SQL 同序（expires_at 降序后取前 limit），
    // 过期的那一条在 read_unexpired 侧就被过滤掉了。
    assert_eq!(
        affinity.lookup(None, "expired").await,
        None,
        "过期行不得恢复进内存"
    );
    assert_eq!(
        affinity.lookup(None, "beyond-limit").await,
        None,
        "超出第 512 行不得恢复"
    );
    assert_eq!(
        affinity.lookup(None, "fresh-0").await.as_deref(),
        Some("v1"),
        "上限内的新鲜粘性必须恢复"
    );
    assert_eq!(sticks.len(), RESTORE_LIMIT as usize, "粘性表恢复 512 条");
    assert_eq!(metrics.snapshot().affinity_restored_count, 512);
}

/// 恢复后 TTL 按剩余存活时长重建（不会比落盘时更久）
#[tokio::test]
async fn restored_ttl_is_remaining_lifetime() {
    let rows = vec![stick("short", "v1", chrono::Utc::now().timestamp() + 30)];
    let (affinity, _, _) = run(AffinityMode::Turn, rows).await;
    let remaining = affinity
        .remaining_ttl(None, "short")
        .await
        .expect("恢复的键必须有剩余 TTL");
    assert!(
        remaining <= std::time::Duration::from_secs(30)
            && remaining > std::time::Duration::from_secs(1),
        "TTL 必须是剩余存活时长（实测 {remaining:?}）"
    );
}

/// legacy / off 模式不恢复（legacy 不落盘、off 查询短路）
#[tokio::test]
async fn legacy_and_off_modes_skip_restore() {
    for mode in [AffinityMode::Legacy, AffinityMode::Off] {
        let rows = vec![stick("s", "v1", far_future())];
        let (affinity, sticks, metrics) = run(mode, rows).await;
        assert_eq!(affinity.lookup(None, "s").await, None, "{mode:?} 不得恢复");
        assert!(sticks.is_empty(), "{mode:?} 粘性表必须为空");
        assert_eq!(metrics.snapshot().affinity_restored_count, 0);
    }
}

/// 未接落盘通道（sink = None）时静默跳过——不 panic、不 warn 风暴
#[tokio::test]
async fn no_sink_skips_restore() {
    let affinity = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let sticks = Arc::new(AffinityStickTable::new());
    let metrics = Arc::new(GlobalMetrics::new());
    restore_affinity_sticks(AffinityMode::Auto, &affinity, &sticks, None, &metrics).await;
    assert!(sticks.is_empty());
    assert_eq!(metrics.snapshot().affinity_restored_count, 0);
}

/// 租户维度恢复：非空 tenant 的粘性恢复到租户键空间
#[tokio::test]
async fn tenant_scoped_sticks_restore_to_tenant_keyspace() {
    let rows = vec![AffinityStick {
        tenant: "tenant-a".into(),
        session_key: "s".into(),
        vendor_id: "v1".into(),
        worth_tokens: 1_024,
        expires_at: far_future(),
    }];
    let (affinity, _, _) = run(AffinityMode::Auto, rows).await;
    assert_eq!(
        affinity.lookup(Some("tenant-a"), "s").await.as_deref(),
        Some("v1"),
        "租户键必须恢复"
    );
    assert_eq!(
        affinity.lookup(None, "s").await,
        None,
        "租户粘性不得污染 legacy 键空间"
    );
}
