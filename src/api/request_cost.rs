// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 逐请求成本（`request_logs.cost_usd` 单一来源）。
//!
//! 计价单源与分账/配额一致：`[[model_catalog]]` 四档单价经
//! `estimate_cost_from_catalog`（cache 语义单源 `prompt_cache_semantics`）。
//! 无 usage（失败/空响应）→ `None`（不写成本）；未登记单价 → `Some(0.0)`
//!（与 token_usage 分账、rerank 既有口径一致：写 0 而非 None）。

use crate::config::ModelCatalogEntry;

/// 逐请求成本（USD 道语义，币种随目录条目，与 `/admin/cost` 同口径）：
/// `response.usage` 四档 token 求和 × 目录单价。未登记单价 → `Some(0.0)`
///（登记性检查单源 `lookup_pricing_entry`）。
pub(crate) fn request_cost_usd(
    catalog: &[ModelCatalogEntry],
    vendor_type: &str,
    model_id: &str,
    response: &serde_json::Value,
) -> Option<f64> {
    let (prompt, completion, cache_read, cache_creation) =
        crate::protocol::parse_usage_detailed(response)?;
    if crate::vendor::cost::lookup_pricing_entry(catalog, model_id).is_none() {
        return Some(0.0);
    }
    let usage = crate::protocol::TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_tokens: cache_read as u64,
        cache_creation_tokens: cache_creation as u64,
        ..Default::default()
    };
    let semantics = crate::vendor::cost::prompt_cache_semantics(vendor_type);
    crate::vendor::cost::estimate_cost_from_catalog(catalog, model_id, &usage, semantics)
        .map(|estimate| estimate.total_cost)
}

/// 会话/上下文观测列（`session_key` + usage 派生的三列）。
///
/// 与 [`request_cost_usd`] 同源取 usage（`parse_usage_detailed`）与目录条目
///（`lookup_pricing_entry`），保证「成本列非空」与「缓存列非空」口径一致。
/// 一次解析、一次查找，避免热路径重复遍历与整目录克隆。
///
/// `enabled=false`（`[usage_insights]` 关）时全部返回 `None`——采集门与
/// 查询门同源，关闭后不写库（与文档承诺一致）。
pub(crate) fn request_context_columns(
    enabled: bool,
    catalog: &[ModelCatalogEntry],
    model_id: &str,
    response: Option<&serde_json::Value>,
    session_key: Option<&str>,
) -> ContextColumns {
    if !enabled {
        return ContextColumns::default();
    }
    let window = crate::vendor::cost::lookup_pricing_entry(catalog, model_id)
        .map(|entry| entry.context_window.min(i64::MAX as u64) as i64);
    // 缓存列保留上游原值，不在此处归一：归零会永久丢失 Inclusive 协议的
    // 命中量（`cache_read_share` 对其恒 0，且无法回溯）。占用率口径的
    // Inclusive/Exclusive 折中见 `repository::INPUT_TOKENS_EXPR`。
    let (cache_read, cache_write) = response
        .and_then(crate::protocol::parse_usage_detailed)
        .map(|(_, _, read, write)| (Some(read as i64), Some(write as i64)))
        .unwrap_or((None, None));
    ContextColumns {
        session_key: session_key.map(str::to_string),
        context_window_tokens: window,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

/// [`request_context_columns`] 的输出：直接展开进 `RequestLog` 的四列。
#[derive(Debug, Clone, Default)]
pub(crate) struct ContextColumns {
    pub session_key: Option<String>,
    pub context_window_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
}
