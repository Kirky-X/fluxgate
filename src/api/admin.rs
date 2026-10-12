// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::{Config, GlobalModelConfig, ModelCatalogEntry, VendorConfig};
use crate::database::LogFilter;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::{error::NexusFlowError, ApiState};
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(serde::Deserialize)]
pub struct LogQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub limit: Option<u32>,
}

/// 管理面列表上限：`limit` 无夹断时任一有效 key 可拖全表装 Vec（OOM
/// 向量）。默认 100（现状），上限 1000。
const ADMIN_LIST_MAX_LIMIT: u32 = 1000;

#[axum::debug_handler]
pub async fn logs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let filter = LogFilter {
        start_time: params.start_time,
        end_time: params.end_time,
        vendor: params.vendor,
        model: params.model,
        limit: params.limit.unwrap_or(100).min(ADMIN_LIST_MAX_LIMIT),
    };

    let logs = state.database.query_logs(&filter).await?;
    Ok(Json(serde_json::json!(logs)))
}

#[axum::debug_handler]
pub async fn search_logs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let keyword = params.vendor.unwrap_or_default();
    let limit = params.limit.unwrap_or(100).min(ADMIN_LIST_MAX_LIMIT);

    let logs = state.database.search_logs(&keyword, limit).await?;
    Ok(Json(serde_json::json!(logs)))
}

#[axum::debug_handler]
pub async fn token_stats(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    let vendor = params.vendor.as_deref();
    // token 统计表由预聚合器管理（N2 存储缝）：读经 UsageSink，
    // 未接线（测试降级场景）显式拒绝而非虚构空表
    let usage_sink =
        state
            .pre_aggregator
            .as_ref()
            .ok_or_else(|| crate::error::GateError::Config {
                message: t("admin-token-stats-usage-sink-unwired"),
            })?;
    let stats = usage_sink.get_token_stats(vendor).await?;
    Ok(Json(serde_json::json!(stats)))
}

/// 成本分账查询参数：`group_by` 必填（vendor|model），`from`/`to`
/// 为 `YYYY-MM-DD` 日期闭区间（可选）；key 过滤经请求头传递
///（明文 key 进 URL 会落入反代/访问日志，查询串不承载敏感参数）
#[derive(serde::Deserialize)]
pub struct CostQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub group_by: Option<String>,
}

/// key 过滤请求头（明文 key 原文；服务端哈希后过滤，明文不落盘）。
const COST_KEY_HEADER: &str = "x-nexusflow-cost-key";

/// 日期格式校验（`YYYY-MM-DD`：长度/分隔符/数字位 + 月日范围；语义
/// 日历合法性（如 2 月 30 日）由 SQL 字典序比较承载，非法日期仅得空
/// 结果——此处只拦格式与范围错误）。
fn valid_cost_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !value[..4].chars().all(|c| c.is_ascii_digit())
        || !value[5..7].chars().all(|c| c.is_ascii_digit())
        || !value[8..10].chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// GET /admin/cost — 成本分账聚合查询：从预聚合表按维度求和，空窗口
/// 返回空数组；可选 key 过滤经 `x-nexusflow-cost-key` 请求头
#[axum::debug_handler]
pub async fn cost_usage(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<CostQuery>,
) -> Result<Json<Value>, NexusFlowError> {
    use crate::storage::CostGroupBy;
    let group_by = match params.group_by.as_deref() {
        Some("vendor") => CostGroupBy::Vendor,
        Some("model") => CostGroupBy::Model,
        _ => {
            return Err(NexusFlowError::from(crate::error::GateError::Validation {
                field: t("admin-cost-group-by-invalid"),
            }));
        }
    };
    for (name, value) in [("from", &params.from), ("to", &params.to)] {
        if let Some(date) = value {
            if !valid_cost_date(date) {
                return Err(NexusFlowError::from(crate::error::GateError::Validation {
                    field: t_args(
                        "admin-cost-date-format-invalid",
                        &[("name", name.to_string())],
                    ),
                }));
            }
        }
    }
    // key 过滤：头传明文哈希后比对（与分账写入的摘要同源）。
    let api_key_hash = headers
        .get(COST_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
        .map(|key| {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(key.trim().as_bytes()))
        });
    let usage_sink =
        state
            .pre_aggregator
            .as_ref()
            .ok_or_else(|| crate::error::GateError::Config {
                message: t("admin-cost-usage-sink-unwired"),
            })?;
    let rows = usage_sink
        .query_cost_usage(
            params.from.as_deref(),
            params.to.as_deref(),
            group_by,
            api_key_hash.as_deref(),
        )
        .await?;
    Ok(Json(serde_json::json!({
        "group_by": params.group_by,
        "rows": rows,
    })))
}

/// 全局指标快照：请求计数、错误率、平均延迟、各厂商细分、缓存命中计数，
/// 及日志背压/secret gate 丢弃计数
#[axum::debug_handler]
pub async fn metrics(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, NexusFlowError> {
    let snapshot = state.vendor_manager.metrics().snapshot();
    let mut value = serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
    value["dropped_logs"] =
        serde_json::json!(state.logger.as_ref().map(|l| l.dropped_logs()).unwrap_or(0));
    value["secret_scan_dropped"] = serde_json::json!(state
        .logger
        .as_ref()
        .map(|l| l.secret_scan_dropped())
        .unwrap_or(0));
    // 缓存统计：库侧全局计数即本进程响应缓存计数——本模块只建一个
    // oxcache `Cache`（response_cache），其默认指标记录器是
    // `UnifiedMetricsRecorder::global()`，而亲和表走裸 backend 不记账。
    // 供 `[cache].l_target` 等按 /admin/metrics 分布调参读数。
    value["cache"] = serde_json::to_value(oxcache::get_enhanced_stats()).unwrap_or(Value::Null);
    Ok(Json(value))
}

#[axum::debug_handler]
pub async fn vendors(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, NexusFlowError> {
    let statuses = state.vendor_manager.get_all_status();

    let vendors: Vec<Value> = statuses
        .iter()
        .map(|(id, status, queue_len)| {
            // key 池观测只暴露 env 名/数量/下标/指纹，绝不暴露 key 原文
            let key_pool = state
                .vendor_manager
                .key_pool_info(id)
                .map(|info| serde_json::to_value(&info).unwrap_or_default())
                .unwrap_or(Value::Null);
            serde_json::json!({
                "id": id,
                "status": format!("{:?}", status),
                "queue_length": queue_len,
                "key_pool": key_pool,
            })
        })
        .collect();

    Ok(Json(serde_json::json!(vendors)))
}

#[axum::debug_handler]
pub async fn vendor_status(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, NexusFlowError> {
    let status = state.vendor_manager.get_status(&id).await;
    let queue_len = state.vendor_manager.get_queue_length(&id);
    // key 池观测只暴露 env 名/数量/下标/指纹，绝不暴露 key 原文
    let key_pool = state
        .vendor_manager
        .key_pool_info(&id)
        .map(|info| serde_json::to_value(&info).unwrap_or_default())
        .unwrap_or(Value::Null);

    Ok(Json(serde_json::json!({
        "id": id,
        "status": format!("{:?}", status),
        "queue_length": queue_len,
        "key_pool": key_pool,
    })))
}

#[axum::debug_handler]
pub async fn reset_vendor(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // WP6 恢复通道：清空该 vendor 的 key 池黑名单（含游标）与冷却状态。
    // limiteron 限流状态仍自动管理（令牌桶 refill），此处不触限流器。
    if !state.vendor_manager.reset_key_pool_state(&id) {
        return Err(NexusFlowError::from(crate::error::GateError::Validation {
            field: t_args("admin-vendor-reset-unknown-vendor", &[("id", id.clone())]),
        }));
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "message": t_args("admin-vendor-reset", &[("id", id)]),
    })))
}

// Config Admin Functions
#[axum::debug_handler]
pub async fn get_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Config>, NexusFlowError> {
    let config = state.config.get_config();
    Ok(Json(config))
}

#[axum::debug_handler]
pub async fn update_config(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<Config>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 对比建站期消费的段（server/cors 在 main.rs 建站时被 sdforge 固化，
    // 运行期不可热换——axum Router 的 layer 在构造时编译固化，sdforge 无
    // 热重载中间件能力），变更时显式告知需重启，消除"静默失效"误判。
    let old = state.config.get_config();
    let mut restart_required: Vec<&'static str> = Vec::new();
    if old.server != config.server {
        restart_required.push("server");
    }
    if old.cors != config.cors {
        restart_required.push("cors");
    }
    // 第三类生效语义：vendor 实例按 id 惰性创建后进程级缓存（manager 的
    // instances map 无失效路径），CB 窗口变更只对新建实例生效——既非重启级
    // （同段其余键每请求现读）也非即时级，单列 surfaced 防止误判。
    let mut applies_to_new_instances: Vec<&'static str> = Vec::new();
    if old.resilience.circuit_open_timeout_ms != config.resilience.circuit_open_timeout_ms {
        applies_to_new_instances.push("resilience.circuit_open_timeout_ms");
    }
    state.config.update_config(config)?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": if restart_required.is_empty() {
            t("admin-config-updated-immediate")
        } else {
            t_args("admin-config-updated-restart-required", &[(
                "sections",
                restart_required.join(", "),
            )])
        },
        "restart_required": restart_required,
        "applies_to_new_instances": applies_to_new_instances,
    })))
}

#[axum::debug_handler]
pub async fn get_vendors(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<VendorConfig>>, NexusFlowError> {
    let config = state.config.get_config();
    Ok(Json(config.vendors))
}

#[axum::debug_handler]
pub async fn update_vendor(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(vendor): Json<VendorConfig>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 原子 RMW：读-改-校验-落盘在 ConfigManager 写锁内完成，
    // 并发与其他 update_* 端点互斥，不丢失更新
    state.config.update_config_with(|config| {
        match config.vendors.iter().position(|v| v.id == id) {
            Some(idx) => {
                config.vendors[idx] = vendor;
                Ok(())
            }
            None => Err(GateError::Config {
                message: t_args("admin-vendor-not-found", &[("id", id.clone())]),
            }),
        }
    })?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": t("admin-vendor-updated")
    })))
}

/// GET /admin/models — 模型能力目录列表（`Vec<ModelCatalogEntry>`）。
/// 全局模型策略经 GET /admin/config 或 POST /admin/models/global 读写，
/// 不并入本列表
#[axum::debug_handler]
pub async fn get_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<ModelCatalogEntry>>, NexusFlowError> {
    // 审查性能 LOW：只克隆目录段，免去全量 Config 深克隆（13 段逐字段复制）
    Ok(Json(state.config.read().model_catalog.clone()))
}

#[axum::debug_handler]
pub async fn update_model(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(model): Json<GlobalModelConfig>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // Config 只有单例 global_model: Option<GlobalModelConfig>（无 ID 字段），
    // 约定 id == "global" 更新全局模型策略（目录条目走 PUT/DELETE）
    if id == "global" {
        // 原子 RMW（同 update_vendor）：读-改-校验-落盘在写锁内完成
        state.config.update_config_with(|config| {
            config.global_model = Some(model);
            Ok(())
        })?;
        Ok(Json(serde_json::json!({
            "success": true,
            "message": t("admin-global-model-updated")
        })))
    } else {
        Err(NexusFlowError::from(GateError::Config {
            message: t("admin-only-global-model-supported"),
        }))
    }
}

/// PUT /admin/models/{model_id} — upsert 单条目录条目（原子 RMW）；
/// path 的 model_id 权威，body 中的被覆写
#[axum::debug_handler]
pub async fn upsert_model(
    State(state): State<Arc<ApiState>>,
    Path(model_id): Path<String>,
    Json(mut entry): Json<ModelCatalogEntry>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 审查：global 为全局策略段保留键（update_model 的 POST 契约），
    // 目录面显式拒绝——禁止借 PUT/DELETE 旁路改写策略段。Validation 变体
    // 映射 400（客户端侧请求问题；Config 变体映射 500 是服务端故障语义）
    if model_id == "global" {
        return Err(NexusFlowError::from(GateError::Validation {
            field: format!("model_id={model_id} (reserved for global strategy)"),
        }));
    }
    entry.model_id = model_id.clone();
    state.config.update_config_with(|config| {
        match config
            .model_catalog
            .iter_mut()
            .find(|e| e.model_id == entry.model_id)
        {
            Some(slot) => *slot = entry.clone(),
            None => config.model_catalog.push(entry.clone()),
        }
        Ok(())
    })?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": t_args("admin-model-upserted", &[("model", model_id)]),
    })))
}

/// DELETE /admin/models/{model_id} — 目录下条（原子 RMW）；条目不存在
/// 时显性 400，禁止静默成功
#[axum::debug_handler]
pub async fn delete_model(
    State(state): State<Arc<ApiState>>,
    Path(model_id): Path<String>,
) -> Result<impl IntoResponse, NexusFlowError> {
    // 审查：global 保留键（同 upsert_model，Validation → 400）
    if model_id == "global" {
        return Err(NexusFlowError::from(GateError::Validation {
            field: format!("model_id={model_id} (reserved for global strategy)"),
        }));
    }
    state.config.update_config_with(|config| {
        let before = config.model_catalog.len();
        config.model_catalog.retain(|e| e.model_id != model_id);
        if config.model_catalog.len() == before {
            // 显性缺失（禁止静默成功）：Validation → 400（客户端侧请求
            // 问题——请求的资源不存在；Config 变体是服务端故障语义 500）
            return Err(GateError::Validation {
                field: format!("model_id={model_id} (not in catalog)"),
            });
        }
        Ok(())
    })?;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": t_args("admin-model-deleted", &[("model", model_id)]),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 参数校验文案经 i18n 模板拼接：实参进响应文案、无未替换占位符、
    /// 无参键不回落为裸键名（缺键时 `t` 原样返回键名——静默失败）。
    #[test]
    fn test_cost_param_validation_messages_are_localized() {
        let date = GateError::Validation {
            field: t_args("admin-cost-date-format-invalid", &[("name", "from".into())]),
        }
        .user_message();
        assert!(date.contains("from"), "实参未进入文案: {date}");
        assert!(!date.contains('$'), "占位符未被替换: {date}");

        let vendor = GateError::Validation {
            field: t_args(
                "admin-vendor-reset-unknown-vendor",
                &[("id", "vendor-a".into())],
            ),
        }
        .user_message();
        assert!(vendor.contains("vendor-a"), "实参未进入文案: {vendor}");
        assert!(!vendor.contains('$'), "占位符未被替换: {vendor}");

        let bare = GateError::Validation {
            field: t("admin-cost-group-by-invalid"),
        }
        .user_message();
        assert!(
            !bare.contains("admin-cost-group-by-invalid"),
            "键未登记，裸键名进入响应: {bare}"
        );
    }
}
