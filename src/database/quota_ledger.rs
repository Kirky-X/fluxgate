// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `quota_ledger` 表的 SQL 单源（quota-aware-routing V7）：多窗预算账本的
//! 结算 UPSERT 与启动恢复读取、保留清理。三方言 UPSERT 单源
//! [`upsert_quota_ledger_sql`]，参数全部绑定（无 SQL 拼接面）。
//!
//! 行模型见 [`QuotaLedgerRow`]：复合主键 (api_key_hash, window_id,
//! window_start) —— window_start 为 Unix 秒，rolling 窗起点 = 窗内首次
//! 用量、calendar 窗 = 对齐边界，同一 window_id 的每个周期实例各占一行。
//! `api_key_hash` 承载三类键：租户摘要 / `__global__`（scope=global 窗）/
//! `__vendor__:{vendor_id}`（vendor 预算，window_id=`vendor:{id}`）。
//! 周期语义（活动期判定）在内存侧按配置解释——账本只存事实；过期周期行
//! 不恢复也不留存：保留清理（[`purge_expired_quota_ledger_sql`]）按配置
//! 最长窗推导下界删除，恢复读取（[`read_quota_ledger_sql`]）同下界过滤，
//! 表增长有界（旧实现「随部署自行清理」无代码路径，行数无界累积）。

use sea_orm::DatabaseBackend;

/// quota_ledger 单行（结算 UPSERT 与启动恢复的搬运形态）。数值列 i64：
/// 与行类型既有着（AffinityStick/RequestLog）同口径，u64 用量在写入侧
/// 饱和收敛。
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaLedgerRow {
    /// 租户摘要 / `__global__` / `__vendor__:{vendor_id}`
    pub api_key_hash: String,
    /// 窗口 id / `vendor:{vendor_id}`
    pub window_id: String,
    /// 窗口起点（Unix 秒）
    pub window_start: i64,
    pub tokens_used: i64,
    pub cost_used_millicents: i64,
    pub updated_at: i64,
}

/// 三方言 UPSERT 语句（纯函数，供实现与测试断言共用）：sqlite（3.24+）与
/// postgres 走 `ON CONFLICT DO UPDATE`（`excluded` 引用新行）；mysql 走
/// `ON DUPLICATE KEY UPDATE` + `VALUES(col)`。六列全部 `?` 绑定（跨方言由
/// sea-orm 按后端改写占位符）。
pub fn upsert_quota_ledger_sql(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::MySql => {
            r#"
            INSERT INTO quota_ledger (api_key_hash, window_id, window_start, tokens_used, cost_used_millicents, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON DUPLICATE KEY UPDATE
                tokens_used = VALUES(tokens_used),
                cost_used_millicents = VALUES(cost_used_millicents),
                updated_at = VALUES(updated_at)
        "#
        }
        // DatabaseBackend 带 #[non_exhaustive]：其余（sqlite/postgres）同走
        // ON CONFLICT；存量行引用按表名限定——postgres 的 DO UPDATE 表达式里
        // 裸列名在「目标表 vs excluded」间有歧义（与 token_usage 同款）。
        _ => {
            r#"
            INSERT INTO quota_ledger (api_key_hash, window_id, window_start, tokens_used, cost_used_millicents, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(api_key_hash, window_id, window_start)
            DO UPDATE SET
                tokens_used = excluded.tokens_used,
                cost_used_millicents = excluded.cost_used_millicents,
                updated_at = excluded.updated_at
        "#
        }
    }
}

/// 全量读取语句（纯函数）：启动恢复读侧，`?` 绑定 `window_start` 下界——
/// 早于「now − 配置最长窗」的行不可能仍处活动期，SQL 层过滤不拉进内存
/// （下界由 `crate::middleware::quota::ledger_retention_secs` 推导，与保留
/// 清理同参单源）。活动期精判（window_start + 周期 ≥ now）仍在内存侧按
/// 配置周期解释——周期是配置面事实，账本不存。
pub fn read_quota_ledger_sql(_backend: DatabaseBackend) -> &'static str {
    r#"
    SELECT api_key_hash, window_id, window_start, tokens_used, cost_used_millicents, updated_at
    FROM quota_ledger
    WHERE window_start >= ?
"#
}

/// 保留清理语句（纯函数）：删除 `window_start` 早于 `?` 下界的过期周期行
///（下界 = now − 配置最长窗，宁多留一行不误删活动行）。行模型按周期实例
/// 增长，无清理路径则日窗每租户每天一行、永久累积。
pub fn purge_expired_quota_ledger_sql(_backend: DatabaseBackend) -> &'static str {
    r#"
    DELETE FROM quota_ledger
    WHERE window_start < ?
"#
}
