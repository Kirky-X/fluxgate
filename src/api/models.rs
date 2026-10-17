// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `GET /v1/models` — OpenAI 兼容模型列表：模型目录 ∪ 各 vendor 配置
//! 模型去重（仅 enabled，按 id 排序确定性输出）。走既有鉴权与统一错误
//! 体系；响应不进缓存（模型列表始终现算）

use crate::error::FluxgateError;
use crate::ApiState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 目录独有条目的 `owned_by` 归属标记（无 vendor 认领时）。
const CATALOG_OWNER: &str = "fluxgate";

#[axum::debug_handler]
pub async fn list_models(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, FluxgateError> {
    let config = state.config.read();

    // vendor 模型先占（owned_by = vendor id），目录条目补缺（不覆盖 vendor
    // 认领）；BTreeMap 保证输出按 id 确定性排序。列出的 id 必须是路由能解析
    // 的公开名：路由按 abstract_id 匹配（缺省回落 model_id，见
    // ConfigManager::resolve_model），裸列上游 model_id 会让清单里的 id
    // 回投即 400「model not found」。
    let mut models: BTreeMap<String, String> = BTreeMap::new();
    for vendor in config.vendors.iter().filter(|v| v.enabled) {
        for model in vendor.models.iter().filter(|m| m.enabled) {
            let public_id = model
                .abstract_id
                .clone()
                .unwrap_or_else(|| model.model_id.clone());
            models.entry(public_id).or_insert_with(|| vendor.id.clone());
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
