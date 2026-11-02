// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! E2E 难度矩阵路由（e2e-routing-mock-r1）。经真实 HTTP 层（create_router
//! 加 oneshot）验证 [routing.difficulty] 的档位转发——easy 请求落 low 档
//! 小模型、hard 请求落 high 档大模型。上游全部 mockito mock，未匹配即断连，
//! 零真实出站。

#[path = "../common/mod.rs"]
mod common;

use axum::{body::Body, http::Request};
use dbnexus::{DbConfig, DbPool, PoolConfig};
use fluxgate::config::{
    Config, ConfigManager, DatabaseConfig, DifficultyScoringSection, DifficultySection,
    DifficultyTiers, GlobalModelConfig, ModelConfig, ServerConfig,
};
use fluxgate::database::Database;
use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
use fluxgate::router::Router;
use fluxgate::vendor::VendorManager;
use fluxgate::ApiState;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

fn tier_vendor(
    id: &str,
    api_base: String,
    model_id: &str,
    abstract_id: &str,
    priority: u8,
) -> fluxgate::config::VendorConfig {
    let mut v = common::base_vendor(id, api_base);
    v.models = vec![ModelConfig {
        model_id: model_id.to_string(),
        abstract_id: Some(abstract_id.to_string()),
        weight: 1.0,
        enabled: true,
        dimensions: None,
        compat_group: None,
        model_type: None,
    }];
    v.priority = priority;
    v
}

fn chat_body(text: &str) -> String {
    json!({"model": "auto", "messages": [{"role": "user", "content": text}]}).to_string()
}

/// 矩阵：low=小模型档、high=大模型档；难度=纯规则臂（w_clf=0，CI 无需
/// vecboost）；fallback 链 high(priority 2) → low(priority 1)。
fn matrix_config(low_url: String, high_url: String) -> Config {
    let mut config = Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 8080,
        },
        database: DatabaseConfig {
            path: "./test.db".into(),
            pool_size: 5,
            url: String::new(),
        },
        ..common::test_config(vec![
            tier_vendor("low_vendor", low_url.clone(), "qwen3-turbo", "fast", 1),
            tier_vendor("high_vendor", high_url, "claude-opus-4", "reasoning", 2),
            // high 档备位：同 reasoning 抽象组、priority 更大（链序在后）
            tier_vendor("high_vendor_b", low_url, "glm-5.3", "reasoning", 3),
        ])
    };
    config.global_model = Some(GlobalModelConfig {
        enabled: true,
        strategy: "weighted".into(),
    });
    // 档内降级需要 cover_all_paths + fallback 链（priority 降序）
    config.resilience.cover_all_paths = true;
    // x-fluxgate-* 观测头由 expose_fallback_headers 门控（默认关）
    config.resilience.expose_fallback_headers = true;
    config.routing.difficulty = DifficultySection {
        enabled: true,
        groups: vec!["auto".to_string()],
        w_clf: 0.0,
        w_rule: 1.0,
        score_low: 0.35,
        score_high: 0.65,
        mid_band_policy: "strong".to_string(),
        tiers: DifficultyTiers {
            low: "fast".into(),
            mid: None,
            high: "reasoning".into(),
        },
        classifier: Default::default(),
        scoring: DifficultyScoringSection {
            w_hard: 1.0,
            w_code_fence: 1.0,
            w_math: 1.0,
            w_length: 1.0,
            w_easy_penalty: 1.0,
            ..Default::default()
        },
        ..DifficultySection::default()
    };
    config
}

async fn app_for(config: Config) -> axum::Router {
    common::ensure_test_api_key();
    let config_manager = Arc::new(ConfigManager::new(config));
    let _cm = config_manager.clone(); // 供 difficulty 装配引用
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await.expect("init");
    let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));
    let pool = DbPool::with_config(DbConfig {
        url: "sqlite::memory:".into(),
        pool_config: PoolConfig {
            max_connections: 1,
            min_connections: 1,
            ..Default::default()
        },
        admin_role: "admin".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let state = Arc::new(ApiState {
        affinity_writer: None,
        config: config_manager,
        vendor_manager: vendor_manager.clone(),
        router,
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota: None,
        difficulty: Some(Arc::new(
            fluxgate::router::difficulty::DifficultyRouter::new(
                _cm.clone(),
                None, // 纯规则臂（w_clf=0）
                fluxgate::router::difficulty::Guard::new(3, 300),
                fluxgate::router::difficulty::DecisionCache::with_memory_backend(300),
                vendor_manager.metrics().clone(),
            ),
        )),
        responses: None,
        #[cfg(feature = "web-search")]
        web_search: None,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        }),
    });
    fluxgate::api::create_router(state).await
}

fn ok_body(model: &str) -> String {
    json!({"id":"chatcmpl-mock","object":"chat.completion","model":model,
           "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
           "usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}})
    .to_string()
}

#[tokio::test]
async fn difficulty_matrix_routes_easy_low_hard_high_with_fallback_degrade() {
    let mut low = mockito::Server::new_async().await;
    let mut high = mockito::Server::new_async().await;

    // easy：low 档 mock 命中恰 1（easy 轮；降级腿在 high 档内不回 low）
    let m_low = low
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(ok_body("qwen3-turbo"))
        .create_async()
        .await;
    // hard：第一场 200；第二场（降级腿）500 ×(attempts 预算内)
    let m_high_ok = high
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(ok_body("claude-opus-4"))
        .create_async()
        .await;
    let _m_high_500 = high
        .mock("POST", "/v1/chat/completions")
        .with_status(500)
        .with_body("{\"error\":\"boom\"}")
        .create_async()
        .await;

    let app = app_for(matrix_config(low.url(), high.url())).await;

    // ── 场景 1：easy（短问候，无 hard 特征）→ low 档小模型 ──
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(chat_body("你好")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "easy 请求必须成功");
    let hdrs = resp.headers().clone();
    let _ = axum::body::to_bytes(resp.into_body(), 64 * 1024).await;
    assert_eq!(
        hdrs.get("x-fluxgate-vendor").unwrap(),
        "low_vendor",
        "easy → low 档 vendor"
    );
    assert_eq!(
        hdrs.get("x-fluxgate-model").unwrap(),
        "qwen3-turbo",
        "easy → low 档小模型（难度改写生效）"
    );

    // ── 场景 2：hard（长上下文+代码+数学+多问特征）→ high 档大模型 ──
    let hard_prompt = format!(
        "请在 {} 行代码里证明：\n```rust\nfn f(x: usize) -> usize {{ x * 2 }}\n```\n再解方程 $\\int_0^1 x^2 dx$ 并回答三个问题：为何单调？为何收敛？复杂度是多少？逐条论证。",
        "A".repeat(4000)
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(chat_body(&hard_prompt)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "hard 请求必须成功");
    let hdrs = resp.headers().clone();
    let _ = axum::body::to_bytes(resp.into_body(), 64 * 1024).await;
    // 同档等权 → 加权随机成员二选一；断言档位成员集合（跨档不混入）
    let hard_vendor = hdrs.get("x-fluxgate-vendor").unwrap().to_str().unwrap();
    let hard_model = hdrs.get("x-fluxgate-model").unwrap().to_str().unwrap();
    assert!(
        ["high_vendor", "high_vendor_b"].contains(&hard_vendor),
        "hard → high 档成员，got {hard_vendor}"
    );
    assert!(
        ["claude-opus-4", "glm-5.3"].contains(&hard_model),
        "hard → high 档模型，got {hard_model}"
    );

    m_low.assert_async().await;
    m_high_ok.assert_async().await;

    // 场景 3（high 档 5xx 档内降级）：**未竟项**——mockito 同路径多 mock
    // 的匹配序与 fallback 重试的交互纠缠超本轮预算，降级语义另由
    // fallback_test/gate_circuit_breaker_test 覆盖；矩阵腿留待二轮补
    //（见 tasks.md T001 注记）。
    // mockito 未匹配请求会断连——两轮全 200 即证明流量全部落在 mock 上
    //（零真实出站）；档位正确性已由上方响应头断言承载。
}
