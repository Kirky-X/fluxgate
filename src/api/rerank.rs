// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! `POST /v1/rerank` — Cohere 兼容重排序端点。
//!
//! 路由按 `vendor.type_ == "rerank"` + `model.model_type == "rerank"` 双维度
//! 过滤（`Router::route_rerank`）；上游经 `VendorManager::execute_rerank`
//! 复用共享连接池、限流、熔断与超时治理。日志只落摘要（query/documents
//! 原文不入库），成本按目录每千文档价计请求级金额。

use crate::api::routing_hints::RoutingHints;
use crate::config::{ModelTask, VendorProtocol};
use crate::database::RequestLog;
use crate::error::{FluxgateError, GateError};
use crate::i18n::t;
use crate::protocol::rerank_converter::{
    normalize_rerank_scores, parse_rerank_request, parse_rerank_response, to_upstream_request,
    RerankRequest,
};
use crate::router::RoutingDecision;
use crate::ApiState;
use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;

#[axum::debug_handler]
pub async fn rerank(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, FluxgateError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start_time = std::time::Instant::now();
    let api_key_hash = crate::utils::tenant::api_key_digest(&headers);

    let parsed = parse_rerank_request(&body)?;
    validate_rerank_hints(&state, &headers)?;
    let session_key = RoutingHints::from_headers(&headers).session_key;
    let decision = state
        .router
        .route_rerank(&parsed.model, session_key.as_deref())
        .await?;

    // 点名路径兜底：X-Vendor / X-Model / vendor.model 限定形式不得绕过能力过滤
    if !matches!(decision.vendor.protocol(), VendorProtocol::Rerank)
        || decision
            .model
            .model_type
            .as_deref()
            .and_then(ModelTask::parse)
            != Some(ModelTask::Rerank)
    {
        let error = FluxgateError::from(GateError::Protocol {
            source: t("api-rerank-capability-mismatch"),
            inner: None,
        });
        push_rerank_log(
            &state,
            &request_id,
            &parsed,
            &decision,
            None,
            start_time.elapsed().as_millis() as i64,
            crate::api::status_code_for_error(&error),
            Some(error.to_string()),
            session_key.as_deref(),
        )
        .await;
        return Err(error);
    }

    let upstream_model = decision.model.model_id.clone();
    let upstream_body = to_upstream_request(&parsed, &upstream_model);
    let response_json = match state
        .vendor_manager
        .execute_rerank(&decision.vendor.id, upstream_body)
        .await
    {
        Ok(response) => response,
        Err(err) => {
            state
                .vendor_manager
                .metrics()
                .record_rerank_upstream_failure();
            let error = FluxgateError::from(err);
            push_rerank_log(
                &state,
                &request_id,
                &parsed,
                &decision,
                None,
                start_time.elapsed().as_millis() as i64,
                crate::api::status_code_for_error(&error),
                Some(error.to_string()),
                session_key.as_deref(),
            )
            .await;
            return Err(error);
        }
    };

    let mut items = match parse_rerank_response(&response_json, &parsed) {
        Ok(items) => items,
        Err(err) => {
            state
                .vendor_manager
                .metrics()
                .record_rerank_upstream_failure();
            // 上游已 2xx（已计费）：形状非法也需入分账
            record_rerank_usage(&state, &decision, &api_key_hash, parsed.documents.len()).await;
            let error = FluxgateError::from(err);
            push_rerank_log(
                &state,
                &request_id,
                &parsed,
                &decision,
                None,
                start_time.elapsed().as_millis() as i64,
                crate::api::status_code_for_error(&error),
                Some(error.to_string()),
                session_key.as_deref(),
            )
            .await;
            return Err(error);
        }
    };

    // 分数归一化（默认 none 原样透传；只改分数，不改排序/index）
    let normalization = state
        .config
        .read()
        .routing
        .rerank_score_normalization
        .clone();
    normalize_rerank_scores(&mut items, &normalization);

    let results: Vec<Value> = items
        .iter()
        .map(|item| {
            let mut value = json!({
                "index": item.index,
                "relevance_score": item.relevance_score,
            });
            if parsed.return_documents {
                value["document"] = json!({ "text": parsed.documents[item.index] });
            }
            value
        })
        .collect();
    let response = json!({
        "id": format!("rerank-{}", uuid::Uuid::new_v4()),
        "model": upstream_model,
        "results": results,
        "meta": {
            "vendor": decision.vendor.id,
            "billed_documents": parsed.documents.len(),
        },
    });

    push_rerank_log(
        &state,
        &request_id,
        &parsed,
        &decision,
        Some(&response),
        start_time.elapsed().as_millis() as i64,
        200,
        None,
        session_key.as_deref(),
    )
    .await;
    state
        .vendor_manager
        .metrics()
        .record_rerank(parsed.documents.len() as u64);
    record_rerank_usage(&state, &decision, &api_key_hash, parsed.documents.len()).await;
    // 会话亲和软粘着：成功请求写回实际 vendor（失败路径不绑定）。复用
    // [`crate::api::bind_session_affinity`] 的单点语义——legacy 才写
    //（auto/turn 由 usage 结算点承接，rerank 无 usage 语义；off 不写），
    // 租户隔离开启时走首绑定收敛
    crate::api::bind_session_affinity(
        &state,
        &api_key_hash,
        session_key.as_deref(),
        &decision.vendor.id,
    )
    .await;
    Ok(Json(response))
}

/// 成本分账：rerank 成本记入 `input_cost`，token 三列与 cache 两列恒 0——
/// 不虚增 `/admin/stats/tokens` 的 token 口径，`/admin/cost` 的 total 覆盖
/// 重排成本。记账失败仅 warn（不失败请求，与 chat 记账同语义）。
async fn record_rerank_usage(
    state: &Arc<ApiState>,
    decision: &RoutingDecision,
    api_key_hash: &str,
    documents: usize,
) {
    let Some(aggregator) = state.pre_aggregator.as_ref() else {
        return;
    };
    let usage = crate::storage::KeyedUsage {
        vendor: decision.vendor.id.clone(),
        model: decision.model.model_id.clone(),
        api_key_hash: api_key_hash.to_string(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        input_cost: rerank_cost(state, &decision.model.model_id, documents),
        output_cost: 0.0,
        cache_read_cost: 0.0,
        cache_creation_cost: 0.0,
    };
    if let Err(e) = aggregator.record_keyed(&usage).await {
        tracing::warn!(
            "{}",
            crate::i18n::t_args("api-usage-record-rejected", &[("error", e.to_string())])
        );
    }
}

/// 点名提示能力复核：X-Vendor 必须指向启用的 rerank 厂商，X-Model 必须能
/// 解析到 rerank 条目；不匹配即 ProtocolError（请求不发出）——防止点名
/// 绕过能力过滤（合法点名继续由 `route_rerank` 在能力集合内选择）。
fn validate_rerank_hints(state: &Arc<ApiState>, headers: &HeaderMap) -> Result<(), FluxgateError> {
    let hints = RoutingHints::from_headers(headers);
    let mismatch = || {
        FluxgateError::from(GateError::Protocol {
            source: t("api-rerank-capability-mismatch"),
            inner: None,
        })
    };
    if let Some(vendor_id) = &hints.preferred_vendor {
        let config = state.config.read();
        let capable = config.vendors.iter().any(|vendor| {
            vendor.enabled
                && vendor.id.eq_ignore_ascii_case(vendor_id)
                && matches!(vendor.protocol(), VendorProtocol::Rerank)
        });
        if !capable {
            return Err(mismatch());
        }
    }
    if let Some(model_name) = &hints.preferred_model {
        let capable = state
            .config
            .resolve_model(model_name)
            .iter()
            .any(|(vendor, model)| {
                matches!(vendor.protocol(), VendorProtocol::Rerank)
                    && model.model_type.as_deref().and_then(ModelTask::parse)
                        == Some(ModelTask::Rerank)
            });
        if !capable {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// 请求级成本：目录每 1k 文档价 × 文档数 / 1000；未登记价 = 0。
/// 与目录 `currency` 同口径，不做隐式币种换算。
fn rerank_cost(state: &Arc<ApiState>, model_id: &str, documents: usize) -> f64 {
    rerank_cost_usd(&state.config.read().model_catalog, model_id, documents)
}

/// 成本纯函数：目录每 1k 文档价 × 文档数 / 1000；未登记价 = 0。
fn rerank_cost_usd(
    catalog: &[crate::config::ModelCatalogEntry],
    model_id: &str,
    documents: usize,
) -> f64 {
    catalog
        .iter()
        .find(|entry| entry.model_id == model_id)
        .map(|entry| entry.rerank_price_per_1k_docs * documents as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// 日志请求摘要（messages 字段）：只含长度与计数，绝不含 query/documents 原文。
pub(crate) fn rerank_request_summary(request: &RerankRequest) -> String {
    json!({
        "query_chars": request.query.chars().count(),
        "documents": request.documents.len(),
        "top_n": request.top_n,
    })
    .to_string()
}

/// 日志响应摘要：结果数与最高分，不含文档内容。
pub(crate) fn rerank_response_summary(response: &Value) -> Value {
    let results = response.get("results").and_then(Value::as_array);
    json!({
        "results": results.map(|items| items.len()).unwrap_or(0),
        "top_score": results
            .and_then(|items| items.first())
            .and_then(|item| item.get("relevance_score"))
            .cloned()
            .unwrap_or(Value::Null),
    })
}

/// 异步日志 push：只写摘要（query 长度 / 文档数 / top_n / 结果数 / 最高分），
/// 绝不落 query 与 documents 原文；logger 未接线时跳过落库。
#[allow(clippy::too_many_arguments)]
async fn push_rerank_log(
    state: &Arc<ApiState>,
    request_id: &str,
    request: &RerankRequest,
    decision: &RoutingDecision,
    response: Option<&Value>,
    duration_ms: i64,
    status_code: i32,
    error_message: Option<String>,
    session_key: Option<&str>,
) {
    // rerank 无 token 语义：缓存两列恒 NULL；`context_window_tokens` 也写
    // NULL——否则 0 prompt 会以 0% 占用率落进 context-stats 的首桶，稀释均值。
    // 会话键仍填充（handler 已用它做亲和路由）。
    let rerank_columns = {
        let enabled = state.config.read().usage_insights.enabled;
        crate::api::ContextColumns {
            session_key: if enabled {
                session_key.map(str::to_string)
            } else {
                None
            },
            context_window_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
        }
    };
    let response_summary = response.map(rerank_response_summary);
    let log = RequestLog {
        id: None,
        request_id: request_id.to_string(),
        timestamp: chrono::Utc::now().timestamp(),
        endpoint: "/v1/rerank".to_string(),
        input_format: "cohere".to_string(),
        abstract_model: Some(request.model.clone()),
        messages: rerank_request_summary(request),
        actual_vendor: decision.vendor.id.clone(),
        actual_model: decision.model.model_id.clone(),
        routing_strategy: format!("{:?}", decision.strategy),
        status_code: Some(status_code),
        response: response_summary.map(|value| value.to_string()),
        error_message,
        prompt_tokens: Some(0),
        completion_tokens: Some(0),
        total_tokens: Some(0),
        duration_ms: Some(duration_ms),
        cost_usd: Some(rerank_cost(
            state,
            &decision.model.model_id,
            request.documents.len(),
        )),
        session_key: rerank_columns.session_key,
        context_window_tokens: rerank_columns.context_window_tokens,
        cache_read_tokens: rerank_columns.cache_read_tokens,
        cache_write_tokens: rerank_columns.cache_write_tokens,
    };
    if let Some(logger) = state.logger.as_ref() {
        if let Err(e) = logger.log(log).await {
            tracing::warn!(
                "{}",
                crate::i18n::t_args("api-log-push-rejected", &[("error", e.to_string())])
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> RerankRequest {
        RerankRequest {
            model: "rerank-v3".into(),
            query: "秘密查询文本".into(),
            documents: vec!["机密文档甲".into(), "机密文档乙".into()],
            top_n: 2,
            return_documents: false,
            instruction: None,
        }
    }

    #[test]
    fn request_summary_never_contains_plaintext() {
        let summary = rerank_request_summary(&request());
        assert!(
            !summary.contains("秘密查询文本"),
            "日志摘要不得含 query 原文: {summary}"
        );
        assert!(
            !summary.contains("机密文档"),
            "日志摘要不得含 documents 原文: {summary}"
        );
        let parsed: Value = serde_json::from_str(&summary).expect("摘要为合法 JSON");
        assert_eq!(parsed["query_chars"], 6);
        assert_eq!(parsed["documents"], 2);
        assert_eq!(parsed["top_n"], 2);
    }

    #[test]
    fn response_summary_reports_count_and_top_score() {
        let response = json!({
            "results": [
                {"index": 0, "relevance_score": 0.9},
                {"index": 1, "relevance_score": 0.2},
            ]
        });
        let summary = rerank_response_summary(&response);
        assert_eq!(summary["results"], 2);
        assert_eq!(summary["top_score"], 0.9);

        let empty = rerank_response_summary(&json!({"results": []}));
        assert_eq!(empty["results"], 0);
        assert!(empty["top_score"].is_null());
    }

    #[test]
    fn cost_uses_catalog_price_per_thousand_docs() {
        let catalog = vec![crate::config::ModelCatalogEntry {
            model_id: "rerank-v3".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 2.0,
            ..Default::default()
        }];
        assert!((rerank_cost_usd(&catalog, "rerank-v3", 500) - 1.0).abs() < 1e-9);
        assert_eq!(
            rerank_cost_usd(&catalog, "unknown", 500),
            0.0,
            "未登记价 = 0"
        );
    }
}

/// DB 级落库用例。feature 门控与 `src/api/mod.rs` 测试模块同集——本模块
/// 依赖 database（dbnexus/Repository/AsyncLogger）与 web-server（ApiState）。
#[cfg(all(
    test,
    feature = "resilience",
    feature = "database",
    feature = "web-server"
))]
mod rerank_cost_db_tests {
    use super::*;
    use crate::config::{
        ConcurrencyConfig, Config, ConfigManager, DatabaseConfig, ModelCatalogEntry, ModelConfig,
        RateLimitConfig, RetrySection, RoutingSection, ServerConfig, VendorConfig,
    };
    use crate::database::{AsyncLogger, AsyncLoggerHandle, Database, LogFilter, Repository};
    use crate::protocol::{AnthropicConverter, OpenAIConverter};
    use crate::router::RoutingStrategy;
    use crate::storage::RequestLogSink;
    use crate::vendor::VendorManager;
    use dbnexus::{DbConfig, DbPool};
    use std::time::Duration;

    fn rerank_vendor() -> VendorConfig {
        VendorConfig {
            cache_ttl_secs: 300,
            silent_cache: false,
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            chat_path: None,
            api_style: None,
            id: "rerank-vendor".to_string(),
            name: "Rerank Vendor".to_string(),
            type_: "rerank".to_string(),
            api_key_env: String::new(),
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "rerank-v3".to_string(),
                abstract_id: None,
                weight: 1.0,
                enabled: true,
                dimensions: None,
                compat_group: None,
                model_type: Some("rerank".to_string()),
            }],
            rate_limit: RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: Some(1),
                sliding: false,
            },
            concurrency: ConcurrencyConfig {
                requests_per_second: 100,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
            supports_batch: false,
            anthropic_version: None,
            forward_beta_headers: false,
            send_nonstandard_params: false,
        }
    }

    async fn make_state() -> (Arc<ApiState>, Arc<Repository>) {
        let config = Config {
            rate_limit: Default::default(),
            otlp: Default::default(),
            usage_insights: Default::default(),
            metrics: Default::default(),
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![rerank_vendor()],
            global_model: None,
            pricing: vec![],
            model_catalog: vec![ModelCatalogEntry {
                model_id: "rerank-v3".into(),
                context_window: 128_000,
                rerank_price_per_1k_docs: 2.0,
                ..Default::default()
            }],
            auth: None,
            cors: None,
            retry: RetrySection::default(),
            routing: RoutingSection::default(),
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
        };
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Arc::new(crate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("memory db pool");
        let repository = Arc::new(Repository::new(pool.clone()));
        repository.initialize().await.expect("schema init");
        let logger: Arc<dyn RequestLogSink> = Arc::new(AsyncLoggerHandle::new(AsyncLogger::new(
            repository.clone(),
            1024,
            Duration::from_millis(200),
        )));
        let database: Arc<dyn RequestLogSink> = Arc::new(Database {
            pool,
            repository: repository.clone(),
        });
        let state = Arc::new(ApiState {
            config: config_manager,
            vendor_manager,
            router,
            affinity_writer: None,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: Some(logger),
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
        });
        (state, repository)
    }

    #[tokio::test]
    async fn rerank_log_cost_usd_matches_catalog_price_per_1k_docs() {
        let (state, repository) = make_state().await;
        let vendor = rerank_vendor();
        let decision = RoutingDecision {
            vendor: vendor.clone(),
            model: vendor.models[0].clone(),
            strategy: RoutingStrategy::Direct,
        };
        let request = RerankRequest {
            model: "rerank-v3".into(),
            query: "q".into(),
            documents: vec!["d0".into(), "d1".into(), "d2".into()],
            top_n: 3,
            return_documents: false,
            instruction: None,
        };

        push_rerank_log(
            &state,
            "req-rerank-cost",
            &request,
            &decision,
            None,
            10,
            200,
            None,
            None,
        )
        .await;
        if let Some(logger) = state.logger.as_ref() {
            logger.shutdown().await.expect("logger shutdown");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;

        let logs = repository
            .query_logs(&LogFilter::default())
            .await
            .expect("query logs");
        assert_eq!(logs.len(), 1, "rerank 请求日志恰一条");
        let cost = logs[0].cost_usd.expect("cost_usd 必须落库");
        assert!(
            (cost - 0.006).abs() < 1e-9,
            "目录 2.0/千文档 × 3 文档 = 0.006，实际 {cost}"
        );
    }
}
