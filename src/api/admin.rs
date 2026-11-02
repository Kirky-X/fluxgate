// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::config::{Config, GlobalModelConfig, ModelCatalogEntry, VendorConfig};
use crate::database::LogFilter;
use crate::error::GateError;
use crate::i18n::{t, t_args};
// `vendor_budget_view` 由 `VendorBudgetSource` trait 提供（QuotaState 反向实现），
// 需 trait 在作用域内才可调用。
use crate::quota_contract::VendorBudgetSource as _;
use crate::{error::FluxgateError, ApiState};
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
) -> Result<Json<Value>, FluxgateError> {
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
) -> Result<Json<Value>, FluxgateError> {
    let keyword = params.vendor.unwrap_or_default();
    let limit = params.limit.unwrap_or(100).min(ADMIN_LIST_MAX_LIMIT);

    let logs = state.database.search_logs(&keyword, limit).await?;
    Ok(Json(serde_json::json!(logs)))
}

#[axum::debug_handler]
pub async fn token_stats(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<LogQuery>,
) -> Result<Json<Value>, FluxgateError> {
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
const COST_KEY_HEADER: &str = "x-fluxgate-cost-key";

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
/// 返回空数组；可选 key 过滤经 `x-fluxgate-cost-key` 请求头
#[axum::debug_handler]
pub async fn cost_usage(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(params): Query<CostQuery>,
) -> Result<Json<Value>, FluxgateError> {
    use crate::storage::CostGroupBy;
    let group_by = match params.group_by.as_deref() {
        Some("vendor") => CostGroupBy::Vendor,
        Some("model") => CostGroupBy::Model,
        _ => {
            return Err(FluxgateError::from(crate::error::GateError::Validation {
                field: t("admin-cost-group-by-invalid"),
            }));
        }
    };
    for (name, value) in [("from", &params.from), ("to", &params.to)] {
        if let Some(date) = value {
            if !valid_cost_date(date) {
                return Err(FluxgateError::from(crate::error::GateError::Validation {
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

/// 会话查询参数：`limit` 缺省 50、上限 500（越界按边界收窄，不报错——
/// admin 只读面不做请求校验）。
#[derive(serde::Deserialize)]
pub struct SessionsQuery {
    pub limit: Option<u32>,
    /// 回看窗口（小时）。缺省 168（7 天）——全史聚合在无保留策略的表上是
    /// 线性全表扫，窗口让查询走 `idx_timestamp`。
    pub window_hours: Option<u64>,
}

/// 回看窗口缺省 7 天；上限 8760（1 年）。
const INSIGHTS_WINDOW_HOURS_DEFAULT: u64 = 168;
const INSIGHTS_WINDOW_HOURS_MAX: u64 = 8760;

/// 窗口小时 → Unix 秒下界。
fn insights_since_ts(window_hours: u64) -> i64 {
    let hours = window_hours.min(INSIGHTS_WINDOW_HOURS_MAX) as i64;
    chrono::Utc::now().timestamp() - hours * 3600
}

/// `[usage_insights].enabled` 运行时门：关闭时端点 404，采集侧四列写 NULL。
/// 返回 `Err(Response)` 供 handler 早退（不引入新错误变体，`GateError` 冻结）。
fn insights_disabled_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": {
                "type": "not_found",
                "message": t("usage-insights-disabled"),
            }
        })),
    )
        .into_response()
}

/// GET /admin/sessions — 按会话聚合成本/token/请求数与首末时间，
/// 标题取首条用户消息前 80 字符。NULL 会话不参与（无归属）。
#[axum::debug_handler]
pub async fn sessions(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<SessionsQuery>,
) -> Result<axum::response::Response, FluxgateError> {
    use axum::response::IntoResponse;
    if !state.config.read().usage_insights.enabled {
        return Ok(insights_disabled_response());
    }
    // 会话聚合的重查询：默认 50，上限 500（比通用 admin 列表更保守）。
    let limit = params.limit.unwrap_or(50).min(500);
    let window_hours = params.window_hours.unwrap_or(INSIGHTS_WINDOW_HOURS_DEFAULT);
    let since_ts = insights_since_ts(window_hours);
    let rows = state.database.query_sessions(since_ts, limit).await?;
    Ok(axum::Json(serde_json::json!({
        "limit": limit,
        "window_hours": window_hours.min(INSIGHTS_WINDOW_HOURS_MAX),
        "rows": rows,
    }))
    .into_response())
}

/// GET /admin/context-stats — 按模型聚合上下文占用率、缓存读份额与五桶分布。
#[axum::debug_handler]
pub async fn context_stats(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<SessionsQuery>,
) -> Result<axum::response::Response, FluxgateError> {
    use axum::response::IntoResponse;
    if !state.config.read().usage_insights.enabled {
        return Ok(insights_disabled_response());
    }
    let window_hours = params.window_hours.unwrap_or(INSIGHTS_WINDOW_HOURS_DEFAULT);
    let since_ts = insights_since_ts(window_hours);
    let rows = state.database.query_context_stats(since_ts).await?;
    Ok(axum::Json(serde_json::json!({
        "window_hours": window_hours.min(INSIGHTS_WINDOW_HOURS_MAX),
        "rows": rows,
    }))
    .into_response())
}

/// 全局指标快照：请求计数、错误率、平均延迟、各厂商细分、缓存命中计数，
/// 及日志背压/secret gate 丢弃计数
#[axum::debug_handler]
pub async fn metrics(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, FluxgateError> {
    let snapshot = state.vendor_manager.metrics().snapshot();
    let mut value = serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
    value["dropped_logs"] =
        serde_json::json!(state.logger.as_ref().map(|l| l.dropped_logs()).unwrap_or(0));
    // 粘性落盘通道的实时丢弃累计（快照里的 affinity_persist_drop_total 是
    // settle 采样近似；本字段是通道侧精确读数，与日志丢弃同款双出口）
    value["affinity_persist_dropped"] = serde_json::json!(state
        .affinity_writer
        .as_ref()
        .map(|w| w.dropped_sticks())
        .unwrap_or(0));
    // 落库失败批次累计（UPSERT 失败整批丢弃）：与通道丢弃分列——一个是被
    // 背压丢弃，一个是写库失败，运维处置完全不同
    value["affinity_flush_failures"] = serde_json::json!(state
        .affinity_writer
        .as_ref()
        .map(|w| w.flush_failures())
        .unwrap_or(0));
    value["secret_scan_dropped"] = serde_json::json!(state
        .logger
        .as_ref()
        .map(|l| l.secret_scan_dropped())
        .unwrap_or(0));
    // 缓存统计：库侧全局计数即本进程响应缓存计数——本模块只建一个
    // oxcache `Cache`（response_cache），其默认指标记录器是
    // `UnifiedMetricsRecorder::global`，而亲和表走裸 backend 不记账。
    // 供 `[cache].l_target` 等按 /admin/metrics 分布调参读数。
    value["cache"] = serde_json::to_value(oxcache::get_enhanced_stats()).unwrap_or(Value::Null);
    Ok(Json(value))
}

/// GET /admin/quota/ledger?key={digest} — 配额账本读侧（quota-aware-routing）：
/// 逐窗 used/重置时刻/作用域（tenant/global/vendor）。`key` 可选过滤租户
/// 摘要（脱敏口径：digest 本就是不可逆摘要，无 key 原文）；账本未接线或
/// `[quota]` 关闭时空 rows（账本态是运行事实，不视作 404 错误）。
pub async fn quota_ledger(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, FluxgateError> {
    let Some(quota) = state.quota.as_ref() else {
        return Ok(Json(serde_json::json!({ "rows": [] })));
    };
    let key = params.get("key").map(|k| k.trim());
    let rows: Vec<Value> = quota
        .ledger_entries()
        .into_iter()
        .filter(|entry| key.is_none_or(|k| entry.api_key_hash == k))
        .map(|entry| {
            serde_json::json!({
                "api_key_hash": entry.api_key_hash,
                "window_id": entry.window_id,
                "scope": entry.scope,
                "window_start": entry.window_start,
                "tokens_used": entry.tokens_used,
                "cost_used_millicents": entry.cost_used_millicents,
                "reset_at": entry.reset_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "rows": rows })))
}

/// GET /admin/vendor-budgets — vendor 预算状态（quota-aware-routing）：
/// 声明限额 × 实际占用 × 消耗率比（pace 口径）× 重置时刻。预算段未配置
/// 时返回配置面空 rows（只读端点，无副作用）。
pub async fn vendor_budgets(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Value>, FluxgateError> {
    let snapshot = state.config.read();
    let rows: Vec<Value> = snapshot
        .vendor_budgets
        .iter()
        .map(|budget| {
            let view = state
                .quota
                .as_ref()
                .and_then(|quota| quota.vendor_budget_view(&snapshot, &budget.vendor_id));
            serde_json::json!({
                "vendor_id": budget.vendor_id,
                "tokens_limit": budget.tokens_limit,
                "cost_limit_millicents": budget.cost_limit_millicents,
                "tokens_used": view.as_ref().map(|v| v.tokens_used).unwrap_or(0),
                "cost_used_millicents": view.as_ref().map(|v| v.cost_used).unwrap_or(0),
                "pace_ratio": view.as_ref().map(|v| v.pace_ratio()).unwrap_or(0.0),
                "depleted": view.as_ref().map(|v| v.depleted()).unwrap_or(false),
                "reset_secs": view.as_ref().map(|v| v.reset_secs).unwrap_or(0),
                "period_secs": view.as_ref().map(|v| v.period_secs).unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "rows": rows })))
}

#[axum::debug_handler]
pub async fn vendors(State(state): State<Arc<ApiState>>) -> Result<Json<Value>, FluxgateError> {
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
) -> Result<Json<Value>, FluxgateError> {
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
) -> Result<impl IntoResponse, FluxgateError> {
    // 恢复通道：清空该 vendor 的 key 池黑名单（含游标）与冷却状态。
    // limiteron 限流状态仍自动管理（令牌桶 refill），此处不触限流器。
    if !state.vendor_manager.reset_key_pool_state(&id) {
        return Err(FluxgateError::from(crate::error::GateError::Validation {
            field: t_args("admin-vendor-reset-unknown-vendor", &[("id", id.clone())]),
        }));
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "message": t_args("admin-vendor-reset", &[("id", id)]),
    })))
}

/// 嵌入兼容组 pin 句柄（路由未装配 pin 时显式拒绝，参照 usage sink 未接线语义）
fn require_embedding_pin(state: &ApiState) -> Result<&Arc<crate::router::EmbeddingPin>, GateError> {
    state
        .router
        .embedding_pin()
        .ok_or_else(|| GateError::Config {
            message: t("admin-embedding-pins-unwired"),
        })
}

/// GET /admin/embedding-pins — 嵌入 pin 表快照（abstract_id 升序）。
/// 列表项 = `{abstract_id, group_key}`：group_key 为条目声明的 compat_group，
/// 未声明组时为单例键 `"{vendor_id}/::{model_id}"`（不与任何厂商互换）
#[axum::debug_handler]
pub async fn embedding_pins(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Value>, FluxgateError> {
    let pin = require_embedding_pin(&state)?;
    let entries: Vec<Value> = pin
        .list()
        .into_iter()
        .map(|(abstract_id, group_key)| {
            serde_json::json!({
                "abstract_id": abstract_id,
                "group_key": group_key,
            })
        })
        .collect();
    Ok(Json(Value::Array(entries)))
}

/// POST /admin/embedding-pins/{abstract_id}/reset — 单条重置（恢复通道）：
/// 清单条 pin 后下一嵌入请求按权重重新粘着（可命中此前被 pin 排除的厂商）。
/// 级联清除该 abstract 当前解析到的全部 (vendor, model) 维度守门基准——
/// 上游模型确已变更时，仅清 pin 不清基准会让下一请求立刻再 502。
/// abstract_id 与路由侧同源归一（trim/小写/auto → `__global__`，任意书写
/// 形态命中同一 pin）；不存在的 abstract_id 幂等成功，仅实际移除递增
/// `embedding_pin_reset_total`
#[axum::debug_handler]
pub async fn reset_embedding_pin(
    State(state): State<Arc<ApiState>>,
    Path(abstract_id): Path<String>,
) -> Result<impl IntoResponse, FluxgateError> {
    let pin = require_embedding_pin(&state)?;
    let normalized = crate::router::bandit::linucb_arm_key(&abstract_id);
    let cleared = pin.clear_one(&normalized);
    // 维度基准级联：按当前配置解析该 abstract 的候选条目逐一清基准
    //（热载换配置后旧条目基准由 clear_all 兜底，此处覆盖未换配置的常见形态）
    for (vendor, model) in state.config.resolve_model(&normalized) {
        pin.clear_dims_for(&vendor.id, &model.model_id);
    }
    if cleared {
        state.vendor_manager.metrics().record_embedding_pin_reset(1);
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "message": if cleared {
            t_args("admin-embedding-pin-reset", &[("abstract", normalized)])
        } else {
            t_args("admin-embedding-pin-reset-absent", &[("abstract", normalized)])
        },
        "cleared": cleared,
    })))
}

// Config Admin Functions
#[axum::debug_handler]
pub async fn get_config(State(state): State<Arc<ApiState>>) -> Result<Json<Config>, FluxgateError> {
    let config = state.config.get_config();
    Ok(Json(config))
}

#[axum::debug_handler]
pub async fn update_config(
    State(state): State<Arc<ApiState>>,
    Json(config): Json<Config>,
) -> Result<impl IntoResponse, FluxgateError> {
    // 对比建站期消费的段（server/cors/rate_limit 在 create_router 期固化，
    // database/grpc/cache.semantic 在 bootstrap 装配一次，global_model 无热读点），
    // 变更时显式告知需重启，消除"静默失效"误判；口径与 config.example.toml 的
    // ❌ 标注、文件监听侧结构门（hot_reload.rs）一致。
    let old = state.config.get_config();
    let mut restart_required: Vec<&'static str> = Vec::new();
    if old.server != config.server {
        restart_required.push("server");
    }
    if old.cors != config.cors {
        restart_required.push("cors");
    }
    // 以下四段同为建站期消费（config.example.toml 的 ❌ 标注 + 文件监听侧
    // 结构门拒绝口径）：database 在建连时解析、grpc 服务器构造期固化、
    // rate_limit 进 Router 构造、global_model 无热读点（hot_reload.rs），
    // 经本端点改后不热生效——显性告知消除"静默失效"误判。
    if old.database != config.database {
        restart_required.push("database");
    }
    if old.grpc != config.grpc {
        restart_required.push("grpc");
    }
    // OTLP：endpoint/interval/timeout 在 bootstrap 一次性构建 exporter 与
    // reader 周期，仅 enabled 是真热读；其余字段变更必须显式告知需重启。
    if old.otlp.endpoint != config.otlp.endpoint
        || old.otlp.interval_secs != config.otlp.interval_secs
        || old.otlp.timeout_secs != config.otlp.timeout_secs
    {
        restart_required.push("otlp");
    }
    if old.rate_limit != config.rate_limit {
        restart_required.push("rate_limit");
    }
    if old.global_model != config.global_model {
        restart_required.push("global_model");
    }
    // 语义缓存层在 bootstrap 装配一次（enabled 时加载 BGE 模型），热更不重
    // 装配——启用/停用与参数变更均需重启。
    if old.cache.semantic != config.cache.semantic {
        restart_required.push("cache.semantic");
    }
    // docs 开关是建站期挂载判定（create_router 内 `state.config.read.docs.enabled`
    // 决定是否 merge swagger 路由），改后须重启；文件监听侧结构门同样拒绝该段
    //（hot_reload.rs "docs switch changed (router rebuild required)"）。
    if old.docs != config.docs {
        restart_required.push("docs.enabled");
    }
    // 难度定档路由是 bootstrap 一次装配（enabled=false 时不接线，ApiState.difficulty
    // 恒 None），经本端点打开后解释接口仍按未接线拒绝，必须重启。
    if old.routing.difficulty.enabled != config.routing.difficulty.enabled {
        restart_required.push("routing.difficulty.enabled");
    }
    // 难度分类器在 bootstrap 装配一次（EngineFactory 加载 checkpoint），
    // 这两个键热更后惰性直到重启——显性告知防"已生效"误判
    if old.routing.difficulty.classifier.model_path
        != config.routing.difficulty.classifier.model_path
    {
        restart_required.push("routing.difficulty.classifier.model_path");
    }
    if old.routing.difficulty.classifier.engine != config.routing.difficulty.classifier.engine {
        restart_required.push("routing.difficulty.classifier.engine");
    }
    if old.routing.difficulty.classifier.cache_ttl_secs
        != config.routing.difficulty.classifier.cache_ttl_secs
    {
        restart_required.push("routing.difficulty.classifier.cache_ttl_secs");
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
) -> Result<Json<Vec<VendorConfig>>, FluxgateError> {
    let config = state.config.get_config();
    Ok(Json(config.vendors))
}

#[axum::debug_handler]
pub async fn update_vendor(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(vendor): Json<VendorConfig>,
) -> Result<impl IntoResponse, FluxgateError> {
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
) -> Result<Json<Vec<ModelCatalogEntry>>, FluxgateError> {
    // 审查性能 LOW：只克隆目录段，免去全量 Config 深克隆（13 段逐字段复制）
    Ok(Json(state.config.read().model_catalog.clone()))
}

#[axum::debug_handler]
pub async fn update_model(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(model): Json<GlobalModelConfig>,
) -> Result<impl IntoResponse, FluxgateError> {
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
        Err(FluxgateError::from(GateError::Config {
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
) -> Result<impl IntoResponse, FluxgateError> {
    // 审查：global 为全局策略段保留键（update_model 的 POST 契约），
    // 目录面显式拒绝——禁止借 PUT/DELETE 旁路改写策略段。Validation 变体
    // 映射 400（客户端侧请求问题；Config 变体映射 500 是服务端故障语义）
    if model_id == "global" {
        return Err(FluxgateError::from(GateError::Validation {
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
) -> Result<impl IntoResponse, FluxgateError> {
    // 审查：global 保留键（同 upsert_model，Validation → 400）
    if model_id == "global" {
        return Err(FluxgateError::from(GateError::Validation {
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

// ---- 目录自动同步（catalog-sync feature 门控）----

/// POST /admin/catalog/sync 请求体：`{"dry_run": true|false}`；
/// 缺省 dry_run=false（直接落盘）。
#[cfg(feature = "catalog-sync")]
#[derive(serde::Deserialize)]
pub struct CatalogSyncRequest {
    #[serde(default)]
    pub dry_run: bool,
}

/// 统一 JSON 错误体形状（与 insights_disabled_response 同构）
#[cfg(feature = "catalog-sync")]
fn catalog_sync_error(
    status: axum::http::StatusCode,
    kind: &'static str,
    message: String,
    reason: Option<&'static str>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut error = serde_json::json!({ "type": kind, "message": message });
    if let Some(reason) = reason {
        error["reason"] = serde_json::json!(reason);
    }
    (status, axum::Json(serde_json::json!({ "error": error }))).into_response()
}

/// POST /admin/catalog/sync — 模型目录自动同步（catalog-sync 双门控：
/// feature 编译门 + `[catalog_sync].enabled` 运行时门，关时 404）。
/// dry_run=true 返回 diff 不落盘；false 走既有热更新写路径（校验→原子
/// 落盘→切内存）。只增不改：既有条目永不触碰。防重入：进程内单飞，
/// 重入 409；拉取/解析失败 fail-open（目录原样）→ 502/413 + i18n 详情。
#[cfg(feature = "catalog-sync")]
#[axum::debug_handler]
pub async fn catalog_sync(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CatalogSyncRequest>,
) -> Result<axum::response::Response, FluxgateError> {
    use crate::config::catalog_sync::{self, FetchError, SyncError};
    if !state.config.read().catalog_sync.enabled {
        return Ok(catalog_sync_error(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            t("catalog-sync-disabled"),
            None,
        ));
    }
    // 快照后弃读锁（拉取是长 await，不持锁跨热点）
    let (section, existing) = {
        let snapshot = state.config.read();
        (
            snapshot.catalog_sync.clone(),
            snapshot.model_catalog.clone(),
        )
    };
    let metrics = state.vendor_manager.metrics();
    match catalog_sync::plan_from_source(&section, &existing, metrics.as_ref()).await {
        Err(SyncError::InFlight) => Ok(catalog_sync_error(
            axum::http::StatusCode::CONFLICT,
            "conflict",
            t("catalog-sync-in-flight"),
            None,
        )),
        Err(SyncError::Fetch(e)) => {
            let status = if matches!(e, FetchError::TooLarge) {
                axum::http::StatusCode::PAYLOAD_TOO_LARGE
            } else {
                axum::http::StatusCode::BAD_GATEWAY
            };
            Ok(catalog_sync_error(
                status,
                "catalog_sync_failed",
                t_args("catalog-sync-failed", &[("reason", e.to_string())]),
                Some(e.reason()),
            ))
        }
        Err(SyncError::Parse(detail)) => Ok(catalog_sync_error(
            axum::http::StatusCode::BAD_GATEWAY,
            "catalog_sync_failed",
            t_args("catalog-sync-failed", &[("reason", detail.clone())]),
            Some("parse"),
        )),
        Err(SyncError::Persist(e)) => Err(FluxgateError::from(e)),
        Ok(outcome) => match outcome {
            crate::config::catalog_sync::PlanOutcome::NotModified => Ok((
                axum::http::StatusCode::OK,
                axum::Json(serde_json::json!({
                    "dry_run": request.dry_run,
                    "status": "not_modified",
                    "added": 0,
                })),
            )
                .into_response()),
            crate::config::catalog_sync::PlanOutcome::Plan(plan, etag) => {
                if request.dry_run {
                    let report = catalog_sync::SyncReport::from_plan(true, &plan, etag);
                    return Ok((axum::http::StatusCode::OK, axum::Json(report)).into_response());
                }
                catalog_sync::persist_plan(&state.config, &plan, metrics.as_ref()).map_err(
                    |e| match e {
                        // persist 只有 Persist(GateError) 一个失败形态
                        SyncError::Persist(gate) => FluxgateError::from(gate),
                        other => FluxgateError::from(GateError::Config {
                            message: t_args(
                                "catalog-sync-failed",
                                &[("reason", other.to_string())],
                            ),
                        }),
                    },
                )?;
                // etag 随 persist 成功回存（dry_run 不回存——未落盘不占进度）
                catalog_sync::commit_etag(etag.as_deref());
                let report = catalog_sync::SyncReport::from_plan(false, &plan, etag);
                Ok((axum::http::StatusCode::OK, axum::Json(report)).into_response())
            }
        },
    }
}

// ---- subscription（subscription-vendor feature）：订阅账号管理面 ----

/// 订阅账号管理面（authorize / callback / accounts / revoke）。feature 门
/// 在路由挂载侧；`[subscription].enabled` 运行时门在 handler 内热读（关时
/// 404，与 usage-insights 同口径）。响应与日志零 token 片段。
#[cfg(feature = "subscription-vendor")]
pub mod subscription_admin {
    use super::*;
    use crate::vendor::subscription::pool::{AccountPool, AuthorizationStart};
    use crate::vendor::subscription::{store, SubscriptionError};
    use axum::extract::Path;

    /// 池未装配（feature 关或 bootstrap 未注入）时的统一错误。
    fn pool_unavailable() -> FluxgateError {
        FluxgateError::Gate(GateError::VendorUnavailable {
            vendor: "subscription".to_string(),
        })
    }

    fn pool_of(state: &ApiState) -> Result<Arc<AccountPool>, FluxgateError> {
        state
            .vendor_manager
            .subscription_pool()
            .ok_or_else(pool_unavailable)
    }

    /// `[subscription].enabled` 运行时门：关时 404（不暴露端点存在性）。
    fn subscription_disabled_response() -> axum::response::Response {
        (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "type": "not_found",
                    "message": t("subscription-admin-disabled"),
                }
            })),
        )
            .into_response()
    }

    /// `[subscription].enabled` 运行时门：关时 404（不暴露端点存在性）；
    /// Err 装箱与 web_search `gate` / `dispatch_direct` 同款——
    /// `Response` 本体超 `clippy::result_large_err` 阈值，错误路径单次
    /// 堆分配换栈上小 `Result`。
    fn require_enabled(state: &ApiState) -> Result<(), Box<axum::response::Response>> {
        if state.config.read().subscription.enabled {
            Ok(())
        } else {
            Err(Box::new(subscription_disabled_response()))
        }
    }

    /// OAuth 错误 → HTTP 语义：回调无效 400、供应商拒绝/降级 503、账号未知 400。
    fn subscription_error(e: SubscriptionError) -> FluxgateError {
        match e {
            SubscriptionError::InvalidCallback => FluxgateError::Gate(GateError::Validation {
                field: t("subscription-callback-invalid"),
            }),
            SubscriptionError::UnknownAccount(id) => FluxgateError::Gate(GateError::Validation {
                field: t_args("subscription-account-unknown", &[("account", id)]),
            }),
            other => FluxgateError::Gate(GateError::VendorUnavailable {
                vendor: format!("subscription: {other}"),
            }),
        }
    }

    /// 回调地址缺省值：本进程 `[server]` 监听面拼回调路径（部署在反代后
    /// 时可用 authorize 请求体覆写；回调必须打回同一实例——单实例约束）。
    /// `0.0.0.0`/`::`/空 host 是监听面不是浏览器可达地址：兜底换回环并
    /// 告警提示覆写（反代/公网部署必须显式 redirect_uri）。
    fn default_redirect_uri(state: &ApiState) -> String {
        let server = &state.config.read().server;
        let host = match server.host.as_str() {
            "0.0.0.0" | "::" | "" => {
                tracing::warn!(
                    "{}",
                    crate::i18n::t("subscription-redirect-default-loopback")
                );
                "127.0.0.1"
            }
            other => other,
        };
        format!("http://{host}:{}/admin/subscription/callback", server.port)
    }

    #[derive(serde::Deserialize)]
    pub struct AuthorizeRequest {
        /// 目标供应商 kind（当前仅 "anthropic"）。
        pub vendor: String,
        /// 复用 `[[subscription_accounts]]` 已声明账号；缺省 = ad-hoc 自增。
        #[serde(default)]
        pub account_id: Option<String>,
        /// 反代后部署的公网回调地址覆写；缺省 = 本进程 server 面。
        #[serde(default)]
        pub redirect_uri: Option<String>,
    }

    /// POST /admin/subscription/authorize：生成 PKCE 会话并返回授权 URL；
    /// 用户在浏览器完成授权后回调本进程换码。
    pub async fn authorize(
        State(state): State<Arc<ApiState>>,
        Json(body): Json<AuthorizeRequest>,
    ) -> Result<axum::response::Response, FluxgateError> {
        use axum::response::IntoResponse;
        if let Err(response) = require_enabled(&state) {
            return Ok(*response);
        }
        if state.config.read().subscription.client_id.trim().is_empty() {
            return Err(FluxgateError::Gate(GateError::Validation {
                field: t("subscription-client-id-missing"),
            }));
        }
        if !state
            .config
            .read()
            .vendors
            .iter()
            .any(|v| v.id == body.vendor)
        {
            return Err(FluxgateError::Gate(GateError::Validation {
                field: t_args(
                    "subscription-account-unknown-vendor",
                    &[("vendor", body.vendor.clone())],
                ),
            }));
        }
        let pool = pool_of(&state)?;
        let redirect_uri = body
            .redirect_uri
            .clone()
            .unwrap_or_else(|| default_redirect_uri(&state));
        let start: AuthorizationStart = pool
            .begin_authorization(&body.vendor, body.account_id.as_deref(), &redirect_uri)
            .map_err(subscription_error)?;
        Ok(Json(serde_json::json!({
            "account_id": start.account_id,
            "state": start.state,
            "authorize_url": start.authorize_url,
            "expires_in_secs": crate::vendor::subscription::oauth::SESSION_TTL.as_secs(),
            "message": t("subscription-authorize-pending"),
        }))
        .into_response())
    }

    #[derive(serde::Deserialize)]
    pub struct CallbackQuery {
        pub state: String,
        pub code: String,
    }

    /// GET /admin/subscription/callback：核对 state（单次/10 分钟）→ 换码 →
    /// refresh token 落 vault、access token 入池。
    pub async fn callback(
        State(state): State<Arc<ApiState>>,
        axum::extract::Query(query): axum::extract::Query<CallbackQuery>,
    ) -> Result<axum::response::Response, FluxgateError> {
        use axum::response::IntoResponse;
        if let Err(response) = require_enabled(&state) {
            return Ok(*response);
        }
        let pool = pool_of(&state)?;
        let account_id = pool
            .finish_authorization(&query.state, &query.code)
            .await
            .map_err(subscription_error)?;
        Ok(Json(serde_json::json!({
            "account_id": account_id,
            "status": "active",
        }))
        .into_response())
    }

    /// GET /admin/subscription/accounts：池状态脱敏列表（id/vendor/状态/
    /// 过期时间戳，零 token 片段）。
    pub async fn accounts(
        State(state): State<Arc<ApiState>>,
    ) -> Result<axum::response::Response, FluxgateError> {
        use axum::response::IntoResponse;
        if let Err(response) = require_enabled(&state) {
            return Ok(*response);
        }
        let pool = pool_of(&state)?;
        Ok(Json(serde_json::json!({ "accounts": pool.summaries() })).into_response())
    }

    /// DELETE /admin/subscription/accounts/{id}：吊销——删 vault 条目 +
    /// 内存摘除，此后该账号不再参与出站。
    pub async fn revoke(
        State(state): State<Arc<ApiState>>,
        Path(account_id): Path<String>,
    ) -> Result<Json<serde_json::Value>, FluxgateError> {
        if require_enabled(&state).is_err() {
            return Err(FluxgateError::Gate(GateError::Validation {
                field: t("subscription-admin-disabled"),
            }));
        }
        let pool = pool_of(&state)?;
        // vault 条目**先删**（refresh token 无主即删，不留孤儿凭证）：
        // 先摘池后删 vault 时，vault 删除失败提前返回会留下孤儿 refresh
        // token——声明账号重启经 register_declared（检测到 token 即 Active）
        // 复活，与吊销语义相反。先删 vault 失败时池内仍在，可重试吊销。
        // 条目与池同源——写在池装配的 store，不在 manager 的 vault 兜底面
        let mut removed_entries = 0usize;
        if let Some(vault_store) = pool.vault_store() {
            removed_entries = store::delete_account_tokens(&vault_store, &account_id).await?;
        }
        let removed_from_pool = pool.remove(&account_id);
        if removed_from_pool {
            tracing::info!(
                account = %account_id,
                vault_entries = removed_entries,
                "{}",
                crate::i18n::t("subscription-account-revoked")
            );
        }
        Ok(Json(serde_json::json!({
            "removed": removed_from_pool,
            "vault_entries_removed": removed_entries,
        })))
    }
}

/// GET /admin/masking/stats — 出站掩码观测（feature `masking`）：24h 滚动窗
/// 的 per-kind 命中次数与值长度直方图**元数据**；不含任何值本身。门未装配
///（feature 关闭或 bootstrap 未接线）返回空窗与 enabled=false。
#[cfg(feature = "masking")]
pub async fn masking_stats(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Value>, FluxgateError> {
    let gate = state.vendor_manager.mask_gate();
    let enabled = gate.as_ref().is_some_and(|g| g.enabled());
    let mut kinds = serde_json::Map::new();
    if let Some(gate) = &gate {
        for (kind, count, buckets) in gate.stats_window() {
            kinds.insert(
                kind.as_str().to_string(),
                serde_json::json!({
                    "count": count,
                    "value_len_buckets": {
                        "le_8": buckets[0],
                        "le_16": buckets[1],
                        "le_32": buckets[2],
                        "le_64": buckets[3],
                        "le_128": buckets[4],
                        "le_256": buckets[5],
                        "le_1024": buckets[6],
                        "gt_1024": buckets[7],
                    }
                }),
            );
        }
    }
    Ok(Json(serde_json::json!({
        "enabled": enabled,
        "window_hours": 24,
        "kinds": kinds,
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
