// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! `GET /v1/models` — OpenAI 兼容模型列表（WP3 数据面端点补齐）。
//!
//! 模型集合 = 模型目录（`[[model_catalog]]`）∪ 各 vendor 配置模型去重
//! （仅 enabled vendor 的 enabled 模型；同 id 只出现一次，按 id 排序，
//! 确定性输出）。走既有 API key 鉴权（认证中间件）与统一错误体系。
//!
//! 响应不进入响应缓存：本 handler 不 consult [`ResponseCache`]（模型列表
//! 始终现算；`X-Cache` 头永不出现）。

use crate::error::NexusFlowError;
use crate::ApiState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 目录独有条目的 `owned_by` 归属标记（无 vendor 认领时）。
const CATALOG_OWNER: &str = "nexusflow";

#[axum::debug_handler]
pub async fn list_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Value>, NexusFlowError> {
    let config = state.config.read();

    // vendor 模型先占（owned_by = vendor id），目录条目补缺（不覆盖 vendor
    // 认领）；BTreeMap 保证输出按 id 确定性排序。
    let mut models: BTreeMap<String, String> = BTreeMap::new();
    for vendor in config.vendors.iter().filter(|v| v.enabled) {
        for model in vendor.models.iter().filter(|m| m.enabled) {
            models
                .entry(model.model_id.clone())
                .or_insert_with(|| vendor.id.clone());
        }
    }
    for entry in &config.model_catalog {
        models
            .entry(entry.model_id.clone())
            .or_insert_with(|| CATALOG_OWNER.to_string());
    }

    let data: Vec<Value> = models
        .into_iter()
        .map(|(id, owned_by)| {
            json!({
                "id": id,
                "object": "model",
                "owned_by": owned_by,
            })
        })
        .collect();

    Ok(Json(json!({
        "object": "list",
        "data": data,
    })))
}
