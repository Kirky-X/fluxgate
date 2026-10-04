// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 集成测试 — /admin/models 模型能力目录 CRUD（T006/N6）。
//!
//! 直调 handler（同 api_test 模式，绕过路由/认证装配——认证沿用既有
//! admin 中间件，与本 CRUD 逻辑正交）。覆盖：
//! - upsert 新增 / 替换（同 id 不产生重复条目）
//! - GET 列表回读
//! - DELETE 下条 / 缺条显性 400（GateError 无 NotFound 变体，冻结约束）
//! - 校验联动：context_window = 0 经 update_config_with 内 validate_config 同拒
//! - 热更新联动：登记后 capabilities 查询立即反映（Manager 热读）

mod common;

use axum::{
    body::Body,
    extract::{Path, State},
    http::Request,
    Json,
};
use nexusflow::{
    api::{admin, create_router},
    config::{ConfigManager, GlobalModelConfig, ModelCatalogEntry, PricingCurrency},
    database::Database,
    error::NexusFlowError,
    protocol::{AnthropicConverter, OpenAIConverter},
    router::Router,
    vendor::VendorManager,
    ApiState,
};
use std::sync::Arc;
use tower::ServiceExt;

fn claude_entry() -> ModelCatalogEntry {
    ModelCatalogEntry {
        model_id: "claude-sonnet-4-20250514".into(),
        context_window: 200_000,
        input_price: 3.0,
        output_price: 15.0,
        cache_read_price: 0.3,
        cache_creation_price: 3.75,
        currency: PricingCurrency::Usd,
        max_output_tokens: Some(8192),
        supports_tools: true,
        supports_thinking: true,
    }
}

async fn create_state() -> Arc<ApiState> {
    let mut config = common::test_config(vec![]);
    config.global_model = Some(GlobalModelConfig {
        enabled: true,
        strategy: "weighted".into(),
    });
    let config_manager = Arc::new(ConfigManager::new(config));
    // 写路径需要落盘路径（ConfigManager 内存态构造拒绝 update_config）——
    // tempfile 唯一目录 + keep()（测试进程内落盘，进程退出随 tmp 清理）
    let dir = tempfile::TempDir::new().expect("tempdir");
    config_manager.set_path(dir.keep().join("config.toml"));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

    let pool = dbnexus::DbPool::with_config(dbnexus::DbConfig {
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
    .unwrap();

    Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(Router::new(config_manager, vendor_manager)),
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(nexusflow::database::Repository::new(pool)),
        }),
    })
}

#[tokio::test]
async fn upsert_new_entry_then_get_lists_it() {
    let state = create_state().await;

    // 初始：目录为空
    let models = admin::get_models(State(state.clone())).await.unwrap().0;
    assert!(models.is_empty(), "未登记目录为空");

    // PUT upsert 新条目
    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(claude_entry()),
    )
    .await
    .expect("upsert new entry");

    // GET 列表回读（含 per-model 条目）
    let models = admin::get_models(State(state.clone())).await.unwrap().0;
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model_id, "claude-sonnet-4-20250514");
    assert_eq!(models[0].context_window, 200_000);
    assert_eq!(models[0].currency, PricingCurrency::Usd);
    assert_eq!(models[0].max_output_tokens, Some(8192));

    // 持久态（ConfigManager 内存面）同步可读
    assert_eq!(state.config.get_config().model_catalog.len(), 1);
}

#[tokio::test]
async fn upsert_replaces_existing_without_duplicates() {
    let state = create_state().await;
    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(claude_entry()),
    )
    .await
    .expect("initial upsert");

    // 同 id 再 upsert：整体替换（改窗口），不产生第二条
    let mut updated = claude_entry();
    updated.context_window = 1_000_000;
    updated.supports_thinking = false;
    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(updated),
    )
    .await
    .expect("replace upsert");

    let models = admin::get_models(State(state.clone())).await.unwrap().0;
    assert_eq!(models.len(), 1, "同 id upsert 不得产生重复条目");
    assert_eq!(models[0].context_window, 1_000_000);
    assert!(!models[0].supports_thinking);
}

/// path 的 model_id 权威：body model_id 被覆写为 URL 标识
#[tokio::test]
async fn upsert_path_id_is_authoritative_over_body() {
    let state = create_state().await;
    let mut entry = claude_entry();
    entry.model_id = "wrong-id-from-body".into();
    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(entry),
    )
    .await
    .expect("upsert");

    let models = admin::get_models(State(state)).await.unwrap().0;
    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].model_id, "claude-sonnet-4-20250514",
        "资源标识取自 URL，body model_id 被覆写"
    );
}

#[tokio::test]
async fn delete_removes_entry_and_absent_is_explicit_error() {
    let state = create_state().await;
    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(claude_entry()),
    )
    .await
    .expect("upsert");

    // DELETE 既有条目 → 移除
    admin::delete_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
    )
    .await
    .expect("delete existing");
    let models = admin::get_models(State(state.clone())).await.unwrap().0;
    assert!(models.is_empty(), "删除后目录为空");

    // DELETE 缺条 → 显性错误（GateError 无 NotFound 变体，冻结约束下以
    // Validation 承载——映射 400 客户端请求问题，禁止静默成功）
    match admin::delete_model(State(state.clone()), Path("ghost-model".to_string())).await {
        Err(NexusFlowError::Gate(nexusflow::error::GateError::Validation { field })) => {
            assert!(
                field.contains("ghost-model"),
                "缺失错误携带 model_id: {field}"
            );
        }
        other => panic!(
            "expected explicit Validation error, got {:?}",
            other.map(|_| ())
        ),
    }
}

/// 校验联动：context_window = 0 经 update_config_with 内 validate_config
/// 同拒（auto-compact 静默不装配守卫）
#[tokio::test]
async fn upsert_zero_context_window_rejected() {
    let state = create_state().await;
    let mut entry = claude_entry();
    entry.model_id = "bad-model".into();
    entry.context_window = 0;
    match admin::upsert_model(
        State(state.clone()),
        Path("bad-model".to_string()),
        Json(entry),
    )
    .await
    {
        Err(NexusFlowError::Gate(nexusflow::error::GateError::Config { message })) => {
            assert!(
                message.contains("bad-model"),
                "校验错误携带 model_id: {message}"
            );
        }
        other => panic!("expected Config error, got {:?}", other.map(|_| ())),
    }
    // 拒绝后目录不留残条（校验失败内存态不变）
    let models = admin::get_models(State(state)).await.unwrap().0;
    assert!(models.is_empty(), "校验失败不得部分落盘");
}

/// 热更新联动：PUT 登记后 capabilities 查询立即反映（Manager 热读
/// ConfigManager，与 POST /admin/config 同语义）
#[tokio::test]
async fn upsert_reflects_in_capabilities_immediately() {
    let state = create_state().await;

    // 登记前：openai 型 vendor 未登记 → 回落 128k
    let caps = state
        .vendor_manager
        .model_capabilities("openai", "claude-sonnet-4-20250514");
    assert_eq!(
        caps.max_context_tokens, 128_000,
        "未登记回落 per-flavor 默认"
    );

    admin::upsert_model(
        State(state.clone()),
        Path("claude-sonnet-4-20250514".to_string()),
        Json(claude_entry()),
    )
    .await
    .expect("upsert");

    let caps = state
        .vendor_manager
        .model_capabilities("anthropic", "claude-sonnet-4-20250514");
    assert_eq!(
        caps.max_context_tokens, 200_000,
        "登记后 capabilities 返回真实值（非 0，auto-compact 守卫）"
    );
    assert_eq!(caps.max_output_tokens, Some(8192));
    assert!(caps.supports_thinking);
}

/// 存量契约：POST /admin/models/global 更新全局模型策略（与目录 CRUD 分道）
#[tokio::test]
async fn post_global_updates_strategy_section() {
    let state = create_state().await;
    admin::update_model(
        State(state.clone()),
        Path("global".to_string()),
        Json(GlobalModelConfig {
            enabled: false,
            strategy: "weighted".into(),
        }),
    )
    .await
    .expect("global update");

    let global = state.config.get_config().global_model.unwrap();
    assert!(!global.enabled, "global 策略段更新生效");
    // 目录面不受 global 更新影响
    let models = admin::get_models(State(state)).await.unwrap().0;
    assert!(models.is_empty());
}

/// 审查 LOW-2：PUT/DELETE /admin/models/global 显式拒绝——global 为全局
/// 策略段保留键（update_model 的 POST 契约），禁止旁路改写
#[tokio::test]
async fn put_and_delete_global_are_rejected_explicitly() {
    let state = create_state().await;

    for method in ["PUT", "DELETE"] {
        let result = match method {
            "PUT" => {
                let mut entry = claude_entry();
                entry.model_id = "global".into();
                admin::upsert_model(
                    State(state.clone()),
                    Path("global".to_string()),
                    Json(entry),
                )
                .await
                .map(|_| ())
            }
            _ => admin::delete_model(State(state.clone()), Path("global".to_string()))
                .await
                .map(|_| ()),
        };
        match result {
            Err(NexusFlowError::Gate(nexusflow::error::GateError::Validation { field })) => {
                assert!(
                    field.contains("global"),
                    "保留键错误显性化（Validation → 400）: {field}"
                );
            }
            other => panic!("expected Validation error for {method}, got {:?}", other),
        }
    }
}

/// 审查架构 LOW-1：HTTP 层 MethodRouter 绑定断言——经真实 create_router
/// 装配打 PUT/DELETE/GET（防路由装配笔误：方法错绑/路径错拼在 handler
/// 直调测试中不可见）。
#[tokio::test]
async fn http_layer_put_delete_get_bound_on_router() {
    let state = create_state().await;
    let app = create_router(state).await;

    // PUT /admin/models/{id} upsert → 200
    let body = serde_json::to_vec(&claude_entry()).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/admin/models/claude-sonnet-4-20250514")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "PUT 绑定于 /admin/models/{{id}}");

    // GET /admin/models 列表含该条目（JSON 数组）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value[0]["model_id"], "claude-sonnet-4-20250514");
    assert_eq!(value[0]["context_window"], 200_000);

    // DELETE /admin/models/{id} 下条 → 200，再 DELETE → 400（显性缺失）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/models/claude-sonnet-4-20250514")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "DELETE 绑定于 /admin/models/{{id}}");

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/models/claude-sonnet-4-20250514")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        400,
        "缺失条目显性 400（Config 错误承载）"
    );
}
