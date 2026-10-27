// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 会话粘性启动恢复：把 `affinity_sticks` 表里未过期的粘性灌回内存亲和表。
//!
//! 上限 512 条（对齐 magpie 实测参数），按 `expires_at` 降序取最新的——
//! 超出上限时优先保留刚服务过的会话。TTL 按剩余存活时长重建（`expires_at
//! − now`），恢复后不会比落盘时更久。

use crate::config::AffinityMode;
use crate::i18n::{t, t_args};
use crate::router::affinity::{AffinityStickTable, AffinityTable};
use std::sync::Arc;
use std::time::Duration;

/// 启动恢复上限（条）：与 magpie 的 sticksKept 同值；超出部分不进内存
///（亲和是优化，恢复集是有界近似）
pub const RESTORE_LIMIT: u32 = 512;

/// 启动恢复：读未过期的前 [`RESTORE_LIMIT`] 条 → 按剩余 TTL 重建亲和键 →
/// 灌入粘性记账表（供后续落盘与可观测消费）。
///
/// 只在 auto / turn 模式执行（`legacy` 不落盘，无旧数据；`off` 查询短路）。
/// 失败仅 warn 不阻断启动（恢复是优化增强，不失败服务）。
pub(crate) async fn restore_affinity_sticks(
    mode: AffinityMode,
    affinity: &Arc<AffinityTable>,
    sticks: &Arc<AffinityStickTable>,
    sink: Option<&Arc<dyn crate::storage::AffinityStickSink>>,
    metrics: &crate::utils::metrics::GlobalMetrics,
) {
    if !matches!(mode, AffinityMode::Auto | AffinityMode::Turn) {
        return;
    }
    let Some(sink) = sink else {
        return;
    };
    let now = chrono::Utc::now().timestamp();
    let rows = match sink.read_unexpired(now, RESTORE_LIMIT).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                "{}",
                t_args("main-affinity-restore-failed", &[("error", e.to_string())])
            );
            return;
        }
    };
    let mut restored = 0u64;
    for row in rows {
        let remaining = row.expires_at.saturating_sub(now);
        if remaining <= 0 {
            continue;
        }
        let tenant = if row.tenant.is_empty() {
            None
        } else {
            Some(row.tenant.as_str())
        };
        affinity
            .bind_for(
                tenant,
                &row.session_key,
                &row.vendor_id,
                Duration::from_secs(remaining as u64),
            )
            .await;
        sticks.record(row);
        restored += 1;
    }
    if restored > 0 {
        metrics.record_affinity_restored(restored);
        tracing::info!(
            "{}",
            t_args(
                "main-affinity-restored",
                &[
                    ("count", restored.to_string()),
                    ("limit", RESTORE_LIMIT.to_string()),
                ],
            )
        );
    } else {
        tracing::info!("{}", t("main-affinity-restore-empty"));
    }
}

#[cfg(test)]
mod tests;
