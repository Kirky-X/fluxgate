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
