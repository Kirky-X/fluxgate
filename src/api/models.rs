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

    // vendor 模型先占（owned_by = vendor id，task 按条目能力标注），目录
    // 条目补缺（不覆盖 vendor 认领）；BTreeMap 保证输出按 id 确定性排序。
    // 列出的 id 必须是路由能解析的公开名：路由按 abstract_id 匹配（缺省回落
    // model_id，见 ConfigManager::resolve_model），裸列上游 model_id 会让
    // 清单里的 id 回投即 400「model not found」。
    let mut models: BTreeMap<String, (String, &'static str)> = BTreeMap::new();
    for vendor in config.vendors.iter().filter(|v| v.enabled) {
        for model in vendor.models.iter().filter(|m| m.enabled) {
            let public_id = model
                .abstract_id
                .clone()
                .unwrap_or_else(|| model.model_id.clone());
            models
                .entry(public_id)
                .or_insert_with(|| (vendor.id.clone(), model_task(model)));
        }
    }
    for entry in &config.model_catalog {
        // 目录条目无 model_type：仅有 rerank 价 → rerank，其余 chat
        let task = if entry.rerank_price_per_1k_docs > 0.0 {
            "rerank"
        } else {
            "chat"
        };
        models
            .entry(entry.model_id.clone())
            .or_insert_with(|| (CATALOG_OWNER.to_string(), task));
    }

    let data: Vec<Value> = models
        .into_iter()
        .map(|(id, (owned_by, task))| {
            json!({
                "id": id,
                "object": "model",
                "owned_by": owned_by,
                "task": task,
            })
        })
        .collect();

    Ok(Json(json!({
        "object": "list",
        "data": data,
    })))
}

/// 条目任务类型标注：显式 `model_type` 优先；未声明时按嵌入字段
///（`dimensions` / 非空 `compat_group`）推断 embedding，其余 chat。
/// 仅用于 `/v1/models` 的增量字段，不改变 id 集合与排序。
fn model_task(model: &crate::config::ModelConfig) -> &'static str {
    match model.model_type.as_deref() {
        Some("rerank") => "rerank",
        Some("embedding") => "embedding",
        Some(_) => "chat",
        None => {
            let embedding = model.dimensions.is_some()
                || model
                    .compat_group
                    .as_deref()
                    .is_some_and(|group| !group.trim().is_empty());
            if embedding {
                "embedding"
            } else {
                "chat"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelConfig;

    fn model(
        model_type: Option<&str>,
        dimensions: Option<u32>,
        group: Option<&str>,
    ) -> ModelConfig {
        ModelConfig {
            model_id: "m".into(),
            abstract_id: None,
            weight: 1.0,
            enabled: true,
            dimensions,
            compat_group: group.map(str::to_string),
            model_type: model_type.map(str::to_string),
        }
    }

    #[test]
    fn task_prefers_explicit_model_type() {
        assert_eq!(model_task(&model(Some("rerank"), None, None)), "rerank");
        assert_eq!(
            model_task(&model(Some("embedding"), None, None)),
            "embedding"
        );
        // 显式 chat 优先于嵌入字段推断
        assert_eq!(
            model_task(&model(Some("chat"), Some(1024), Some("g"))),
            "chat"
        );
    }

    #[test]
    fn task_infers_embedding_from_legacy_fields() {
        assert_eq!(model_task(&model(None, Some(1536), None)), "embedding");
        assert_eq!(
            model_task(&model(None, None, Some("openai-embedding"))),
            "embedding"
        );
        assert_eq!(model_task(&model(None, None, Some("   "))), "chat");
        assert_eq!(model_task(&model(None, None, None)), "chat");
    }
}
