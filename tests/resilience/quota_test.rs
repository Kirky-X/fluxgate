// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 集成测试 — 成本与 token 维度配额（WP8 预扣-回补）：预扣后超限 429、
//! 响应后回补恢复、流式结束回补、窗口边界翻转、已放行流中途不中断、
//! 与入站限流三闸门叠加（任一触发即拒且触发方可辨识）。
//!
//! 走 `create_router` 全中间件栈（认证 + 配额 + 限流与线上同形）。token
//! 维度单测口径（无 pricing 条目 → 成本预扣为 0、成本不限，只行 token 窗）；
//! 成本维度用 `[[pricing]]` 条目 + `cost_quota_millicents` 单独锁定（token
//! 窗放宽至不参与），两维度任一触发即拒。

#![cfg(feature = "web-server")]

#[path = "../common/mod.rs"]
mod common;

use axum::{body::Body, http::Request, http::StatusCode};
use nexusflow::api::create_router;
use nexusflow::config::{
    AuthSection, AuthTier, Config, ConfigManager, ModelCatalogEntry, ModelConfig, PricingConfig,
    QuotaSection, RateLimitRule, RateLimitSection, VendorConfig,
};
use nexusflow::database::Database;
use nexusflow::i18n::t;
use nexusflow::middleware::quota::{
    actual_cost_millicents, precharge_for, UsageCostInput, MILLICENTS_PER_USD,
};
use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
use nexusflow::router::Router as NexusRouter;
use nexusflow::utils::estimate_messages_tokens;
use nexusflow::vendor::VendorManager;
use nexusflow::ApiState;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// sha256("test-good-key")（与限流认证测试同值）。
const GOOD_KEY_DIGEST: &str = "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";

/// 400 字符正文（est ≈ 100 + 8 开销，远大于断言边际，防估算口径漂移）。
const LONG_TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Extra padding to be safe 0123456789.";

/// 成本维度用单价：每 1k token 输入 10 USD / 输出 20 USD（只按 model_id
/// 匹配，跨 vendor 取高，vendor_id 仅为账目标签）。
const INPUT_PRICE_PER_1K: f64 = 10.0;
const OUTPUT_PRICE_PER_1K: f64 = 20.0;
/// 与 `quota_config` 里 `est_chars_per_token` 同值（预扣估算口径）。
const EST_CHARS_PER_TOKEN: u64 = 4;
/// mock 响应的实际 usage (prompt, completion)。
const ACTUAL_USAGE: (u64, u64) = (3, 2);
/// 上游响应体：usage 实际 (3,2)，远小于预估 → 回补必为负向返还。
const ACTUAL_USAGE_BODY: &str = r#"{"id":"q","object":"chat.completion","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;

/// 本文件既有用例保持「目录未登记 → `[[pricing]]` 回退」的档源形态（回归
/// 锚：T012/叠加用例的期望数值不因成本档源改造而变）。
fn no_catalog() -> Vec<ModelCatalogEntry> {
    Vec::new()
}

/// 实际成本输入束（model 固定 "gpt-4"，与 `chat_body` 同源）。
fn cost_input<'a>(
    prompt: u64,
    completion: u64,
    cache_read: u64,
    cache_creation: u64,
    catalog: &'a [ModelCatalogEntry],
    pricing: &'a [PricingConfig],
    vendor_type: &'a str,
) -> UsageCostInput<'a> {
    UsageCostInput {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        catalog,
        pricing,
        model: "gpt-4",
        vendor_type,
    }
}

/// 成本档源目录条目（每 1M tokens 四档价）：目录命中时优先于 `[[pricing]]`。
fn catalog_entry(
    input: f64,
    output: f64,
    cache_read: f64,
    cache_creation: f64,
) -> Vec<ModelCatalogEntry> {
    vec![ModelCatalogEntry {
        model_id: "gpt-4".into(),
        context_window: 8192,
        input_price: input,
        output_price: output,
        cache_read_price: cache_read,
        cache_creation_price: cache_creation,
        ..Default::default()
    }]
}

fn pricing_entries() -> Vec<PricingConfig> {
    vec![PricingConfig {
        vendor_id: "q-a".into(),
        model_id: "gpt-4".into(),
        input_per_1k_tokens: INPUT_PRICE_PER_1K,
        output_per_1k_tokens: OUTPUT_PRICE_PER_1K,
    }]
}

fn openai_vendor(id: &str, api_base: String) -> VendorConfig {
    common::ensure_test_api_key();
    let mut v = common::base_vendor(id, api_base);
    v.models = vec![ModelConfig {
        model_id: "gpt-4".into(),
        abstract_id: Some("gpt-4".into()),
        weight: 1.0,
        enabled: true,
    }];
    v
}

fn anthropic_vendor(id: &str, api_base: String) -> VendorConfig {
    common::ensure_test_api_key();
    let mut v = common::base_vendor(id, api_base);
    v.type_ = "anthropic".into();
    v.models = vec![ModelConfig {
        model_id: "claude-x".into(),
        abstract_id: Some("gpt-4".into()),
        weight: 1.0,
        enabled: true,
    }];
    v
}

fn quota_config(vendors: Vec<VendorConfig>, token_quota: u64, window_secs: u64) -> Config {
    quota_config_full(vendors, token_quota, 0, window_secs, Vec::new())
}

/// 双维度配额配置：`token_quota`/`cost_quota_millicents` 各自 0 = 不限；
/// `pricing` 为空时成本预扣恒为 0（成本窗不参与）。
fn quota_config_full(
    vendors: Vec<VendorConfig>,
    token_quota: u64,
    cost_quota_millicents: u64,
    window_secs: u64,
    pricing: Vec<PricingConfig>,
) -> Config {
    let mut config = common::test_config(vendors);
    config.auth = Some(AuthSection {
        enabled: true,
        api_keys: vec![GOOD_KEY_DIGEST.to_string()],
        header: Default::default(),
        keys: vec![nexusflow::config::AuthKeyEntry {
            hash: GOOD_KEY_DIGEST.to_string(),
            tier: AuthTier::Elastic,
            token_quota,
            cost_quota_millicents,
        }],
    });
    config.quota = QuotaSection {
        enabled: true,
        window_secs,
        est_chars_per_token: EST_CHARS_PER_TOKEN,
    };
    config.pricing = pricing;
    config
}

async fn make_state(config: Config, with_quota: bool) -> Arc<ApiState> {
    make_state_inner(config, with_quota, false).await
}

/// 带响应缓存的形态（M6 集成：缓存 HIT 请求以 0 结算回补预扣）。
async fn make_state_with_cache(config: Config, with_quota: bool) -> Arc<ApiState> {
    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await.expect("vendor init");

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
    let repository = Arc::new(nexusflow::database::Repository::new(pool.clone()));
    repository.initialize().await.expect("schema init");
    let database = Arc::new(Database { pool, repository });
    let router = Arc::new(NexusRouter::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let quota = with_quota.then(|| Arc::new(nexusflow::middleware::quota::QuotaState::new()));
    let response_cache = Arc::new(nexusflow::api::response_cache::ResponseCache::new(
        Arc::new(oxcache::backend::default_memory_backend()),
        config_manager.clone(),
        vendor_manager.metrics().clone(),
    ));

    Arc::new(ApiState {
        config: config_manager,
        vendor_manager,
        router,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database,
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: Some(response_cache),
        bandit: None,
        quota,
    })
}

/// `with_usage_sink` 仅给成本档源一致性用例拉上分账链（WP9 预聚合器），
/// 其余用例保持 `None`（与线上未接线的默认形态一致）。
async fn make_state_inner(
    config: Config,
    with_quota: bool,
    with_usage_sink: bool,
) -> Arc<ApiState> {
    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await.expect("vendor init");

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
    let repository = Arc::new(nexusflow::database::Repository::new(pool.clone()));
    repository.initialize().await.expect("schema init");
    let database = Arc::new(Database { pool, repository });
    let pre_aggregator = with_usage_sink.then(|| {
        Arc::new(nexusflow::database::TokenPreAggregator::new(
            database.repository.clone(),
            Duration::from_secs(60),
        )) as Arc<dyn nexusflow::storage::UsageSink>
    });
    let router = Arc::new(NexusRouter::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let quota = with_quota.then(|| Arc::new(nexusflow::middleware::quota::QuotaState::new()));

    Arc::new(ApiState {
        config: config_manager,
        vendor_manager,
        router,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database,
        logger: None,
        pre_aggregator,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota,
    })
}

/// 同 app 发 GET（成本分账查询用）。
async fn get_app(app: &axum::Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("Authorization", "Bearer test-good-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn read_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn chat_body(stream: bool) -> Value {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": LONG_TEXT}],
        "max_tokens": 10,
        "stream": stream,
    })
}

/// 预估输入 token（与网关同一估算函数；max_tokens 另计）。
fn est_input() -> u64 {
    estimate_messages_tokens(&chat_body(false))
}

async fn post_chat(state: Arc<ApiState>, body: Value) -> axum::response::Response {
    let app = create_router(state).await;
    post_chat_app(&app, body).await
}

/// 在既定 app 上发 chat 请求。跨请求共享限流桶必须复用同一个
/// `create_router` 实例——限流判定件在构造期从 [rate_limit] 段实例化，
/// 每请求重建即新桶（配额账本在 ApiState，不受重建影响）。
async fn post_chat_app(app: &axum::Router, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-good-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn read_body(response: axum::response::Response) -> (StatusCode, Value, bool) {
    let status = response.status();
    let has_retry_after = response.headers().contains_key("retry-after");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, value, has_retry_after)
}

/// 状态 + 原始文本体（两闸门的 429 体为 Fluent 纯文本而非 JSON 响应体，
/// 只能按文案比对）。
async fn read_text(response: axum::response::Response) -> (StatusCode, String, bool) {
    let status = response.status();
    let has_retry_after = response.headers().contains_key("retry-after");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes).to_string(),
        has_retry_after,
    )
}

/// 端点限流规则（`/v1/chat/completions` 每租户 `max_requests` 次/小时）。
fn chat_rate_rule(max_requests: u64) -> Vec<RateLimitRule> {
    vec![RateLimitRule {
        id: "stack-chat".into(),
        path: Some("/v1/chat/completions".into()),
        per_tenant: true,
        window_secs: 3600,
        max_requests,
    }]
}

/// R-quota-cost-001/002：预扣超限即 429 + Retry-After + i18n 文案。
#[tokio::test]
async fn over_quota_rejected_at_entry_with_retry_after() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"q","object":"chat.completion","choices":[]}"#)
        .expect(0)
        .create_async()
        .await;

    // token_quota 远小于单请求预扣 → 首请求即 429，上游零触达。
    let state = make_state(
        quota_config(vec![openai_vendor("q-a", server.url())], 10, 3600),
        true,
    )
    .await;
    let (status, _, has_retry_after) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(has_retry_after, "超限必须带 Retry-After");

    mock.assert_async().await;
}

/// R-quota-cost-002：响应后回补——无回补时请求 2 必 429（2P > Q），
/// 有回补时放行（A+P ≤ Q）。
#[tokio::test]
async fn response_settles_quota_for_next_request() {
    let p = est_input() + 10;
    assert!(p > 15, "测试边际要求 P 远大于实际用量，实测 P={p}");
    let quota = p + 15;

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        // 实际用量 (3,2)=5，远小于预估 → 回补返还。
        .with_body(r#"{"id":"q","object":"chat.completion","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#)
        .expect(2)
        .create_async()
        .await;

    let state = make_state(
        quota_config(vec![openai_vendor("q-a", server.url())], quota, 3600),
        true,
    )
    .await;
    let (s1, _, _) = read_body(post_chat(state.clone(), chat_body(false)).await).await;
    assert_eq!(s1, StatusCode::OK, "首请求预扣 P ≤ Q 必须放行");
    let (s2, _, _) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "回补后 A+P ≤ Q 必须放行（无回补则 2P > Q 必 429）"
    );

    mock.assert_async().await;
}

/// R-quota-cost-001（成本窗口独立触发）：token 窗宽裕不参与，仅成本预扣
/// 超限即 429 + Retry-After，上游零触达——两维度独立叠加、任一触发即拒。
#[tokio::test]
async fn over_cost_quota_rejected_at_entry_while_token_window_ample() {
    let (pre_tokens, pre_cost) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );
    assert!(
        pre_cost > 1,
        "边际要求成本预扣远高于 1 millicent，实测 {pre_cost}"
    );

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(0)
        .create_async()
        .await;

    // token 窗给 100× 预扣（绝不参与），成本窗只给 1 millicent → 拒因只能
    // 是成本维（token 维受限时本断言无法区分触发方）。
    let state = make_state(
        quota_config_full(
            vec![openai_vendor("q-a", server.url())],
            pre_tokens.saturating_mul(100),
            1,
            3600,
            pricing_entries(),
        ),
        true,
    )
    .await;
    let (status, _, has_retry_after) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(has_retry_after, "成本超限必须带 Retry-After");

    mock.assert_async().await;
}

/// R-quota-cost-001/002（成本窗口回补）：限额 = 一次预扣 + 实际成本；
/// 无回补时第二请求 2×pre > quota 必 429，回补后恢复放行（token 窗始
/// 终宽裕，不参与）。
#[tokio::test]
async fn cost_response_settles_quota_for_next_request() {
    let (pre_tokens, pre_cost) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );
    let actual_cost = actual_cost_millicents(&cost_input(
        ACTUAL_USAGE.0,
        ACTUAL_USAGE.1,
        0,
        0,
        &no_catalog(),
        &pricing_entries(),
        "openai",
    ));
    assert!(
        pre_cost > actual_cost,
        "回补边际要求实际成本低于预估，实测 pre={pre_cost} actual={actual_cost}"
    );

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(2)
        .create_async()
        .await;

    let state = make_state(
        quota_config_full(
            vec![openai_vendor("q-a", server.url())],
            pre_tokens.saturating_mul(100),
            pre_cost.saturating_add(actual_cost),
            3600,
            pricing_entries(),
        ),
        true,
    )
    .await;
    let (s1, _, _) = read_body(post_chat(state.clone(), chat_body(false)).await).await;
    assert_eq!(s1, StatusCode::OK, "首请求成本预扣 pre ≤ quota 必须放行");
    let (s2, _, _) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "成本回补后 actual+pre ≤ quota 必须放行（无回补则 2×pre 必 429）"
    );

    mock.assert_async().await;
}

/// R-quota-cost-002（流式）：转换流结束后回补；上限对照组（无回补必 429）。
#[tokio::test]
async fn stream_end_settles_quota() {
    let p = est_input() + 10;
    let quota = p + 15;

    // Anthropic 上游 SSE（含 usage 尾帧 → 转换流 Usage 结算）。
    let sse = format!(
        "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{}}\n\n",
        json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":3,"output_tokens":1}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
    );
    let mut server = mockito::Server::new_async().await;
    let mock_stream = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(1)
        .create_async()
        .await;
    let mock_json = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#)
        .expect(1)
        .create_async()
        .await;

    let state = make_state(
        quota_config(
            vec![anthropic_vendor("q-claude", server.url())],
            quota,
            3600,
        ),
        true,
    )
    .await;
    // OpenAI 入站 + Anthropic 上游 = 转换流（Usage 尾帧可结算）。
    let (status, _, _) = read_body(post_chat(state.clone(), chat_body(true)).await).await;
    assert_eq!(status, StatusCode::OK, "流式首请求必须放行");
    // 流结束后回补 → 后续非流式放行（无回补则 2P > Q 必 429）。
    let (s2, _, _) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(s2, StatusCode::OK, "流结束回补后必须放行");

    mock_stream.assert_async().await;
    mock_json.assert_async().await;
}

/// R-quota-cost-001：窗口边界翻转——耗尽后 429，窗口过后恢复。
#[tokio::test]
async fn window_flip_restores_quota() {
    let p = est_input() + 10;
    let quota = p + 15;

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"q","object":"chat.completion","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#)
        .expect_at_least(2)
        .create_async()
        .await;

    // 开放 vendor（透传直连，无需转换；usage 照常回补）。
    let state = make_state(
        quota_config(vec![openai_vendor("q-a", server.url())], quota, 2),
        true,
    )
    .await;
    // 累积实际用量直至 429（回补后每次 +5，至多 8 次必达）。
    let mut rejected = false;
    for _ in 0..8 {
        let (status, _, _) = read_body(post_chat(state.clone(), chat_body(false)).await).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            rejected = true;
            break;
        }
        assert_eq!(status, StatusCode::OK);
    }
    assert!(rejected, "累积用量必须最终触发 429");

    // 窗口翻转（2s 窗，睡 2.5s）→ 恢复放行。
    tokio::time::sleep(Duration::from_secs_f64(2.5)).await;
    let (status, _, _) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::OK, "窗口翻转后必须恢复放行");

    mock.assert_async().await;
}

/// R-streaming-resilience-003 流中侧：已放行流中途配额耗尽不被中断，
/// 且回补在流结束后生效。
#[tokio::test]
async fn admitted_stream_survives_mid_stream_exhaustion() {
    let p = est_input() + 10;
    let quota = p + 15;

    let sse = format!(
        "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{}}\n\n",
        json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":3,"output_tokens":1}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello-stream"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
    );
    let mut server = mockito::Server::new_async().await;
    let mock_stream = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(1)
        .create_async()
        .await;
    let mock_json = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-x","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#)
        .expect(1)
        .create_async()
        .await;

    let state = make_state(
        quota_config(
            vec![anthropic_vendor("q-claude", server.url())],
            quota,
            3600,
        ),
        true,
    )
    .await;
    // 流式请求先行（oneshot 在响应头就绪即返回，body 尚未消费——流未结束）。
    let app = create_router(state.clone()).await;
    let stream_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-good-key")
                .body(Body::from(serde_json::to_vec(&chat_body(true)).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream_response.status(), StatusCode::OK);

    // 流未结束（未消费 body）：此时配额已被流预扣占满，后续请求 429。
    let (s2, _, has_retry) = read_body(post_chat(state.clone(), chat_body(false)).await).await;
    assert_eq!(s2, StatusCode::TOO_MANY_REQUESTS, "流中配额耗尽必须 429");
    assert!(has_retry);

    // 流完整结束（全部帧 + 无错误帧，不被中途打断）。
    let bytes = axum::body::to_bytes(stream_response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(text.contains("hello-stream"), "流内容完整: {text}");
    assert!(!text.contains("event: error"), "已放行流不得被中断: {text}");

    // 流结束回补生效 → 后续请求放行。
    let (s3, _, _) = read_body(post_chat(state, chat_body(false)).await).await;
    assert_eq!(s3, StatusCode::OK, "流结束回补后必须放行");

    mock_stream.assert_async().await;
    mock_json.assert_async().await;
}

/// R-quota-cost-001（三闸门叠加·配额为触发方）：紧配额（成本窗 1
/// millicent）+ 宽限流（1000 次/窗）→ 429 且体为配额文案、上游
/// 零触达（限流本可放行，拒因只能是配额）。
#[tokio::test]
async fn stacked_gates_tight_quota_rejects_with_quota_message() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(0)
        .create_async()
        .await;

    let mut config = quota_config_full(
        vec![openai_vendor("q-a", server.url())],
        0,
        1,
        3600,
        pricing_entries(),
    );
    config.rate_limit = RateLimitSection {
        rules: chat_rate_rule(1_000),
    };
    let state = make_state(config, true).await;
    let app = create_router(state).await;
    let (status, text, has_retry_after) =
        read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(has_retry_after, "配额 429 必须带 Retry-After");
    assert_eq!(text.trim(), t("middleware-quota-exceeded"), "体: {text}");
    assert_ne!(
        text.trim(),
        t("router-rate-limit-exceeded"),
        "宽限流不得成为触发方"
    );

    mock.assert_async().await;
}

/// R-quota-cost-001（三闸门叠加·限流为触发方）：宽配额（100× 预扣）+
/// 紧限流（1 次/窗）→ 首请求 200（上游 1 次）、次请求 429 且体为限流
/// 文案（上游仍 1 次）——两闸门各自独立判定、触发方可辨识。
/// 注：执行序 auth→quota→rate_limit→admission，次请求先被配额预扣后才
/// 被限流拒（quota.rs 声明的「入口被拒预扣不返还」口径），故不断言
/// 配额账本洁净。
#[tokio::test]
async fn stacked_gates_tight_rate_limit_rejects_with_rate_limit_message() {
    let (pre_tokens, _) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(1)
        .create_async()
        .await;

    let mut config = quota_config_full(
        vec![openai_vendor("q-a", server.url())],
        pre_tokens.saturating_mul(100),
        0,
        3600,
        pricing_entries(),
    );
    config.rate_limit = RateLimitSection {
        rules: chat_rate_rule(1),
    };
    let state = make_state(config, true).await;
    let app = create_router(state).await;
    let (s1, _, _) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(s1, StatusCode::OK, "首请求两闸门均宽裕必须放行");

    let (s2, text, has_retry_after) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(s2, StatusCode::TOO_MANY_REQUESTS);
    assert!(has_retry_after, "限流 429 必须带 Retry-After");
    assert_eq!(text.trim(), t("router-rate-limit-exceeded"), "体: {text}");
    assert_ne!(
        text.trim(),
        t("middleware-quota-exceeded"),
        "宽配额不得成为触发方"
    );

    mock.assert_async().await;
}

/// R-quota-cost-001（三闸门叠加·不误拒）：两闸门均宽裕时连续请求
/// 照常放行（叠加不互相吞掉）。
#[tokio::test]
async fn stacked_gates_both_ample_pass_repeated_requests() {
    let (pre_tokens, pre_cost) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(3)
        .create_async()
        .await;

    let mut config = quota_config_full(
        vec![openai_vendor("q-a", server.url())],
        pre_tokens.saturating_mul(100),
        pre_cost.saturating_mul(100),
        3600,
        pricing_entries(),
    );
    config.rate_limit = RateLimitSection {
        rules: chat_rate_rule(100),
    };
    let state = make_state(config, true).await;
    let app = create_router(state).await;
    for i in 0..3 {
        let (status, text, _) = read_text(post_chat_app(&app, chat_body(false)).await).await;
        assert_eq!(status, StatusCode::OK, "第 {i} 个请求叠加不应误拒: {text}");
    }

    mock.assert_async().await;
}

/// 含 prompt cache 的实际 usage（OpenAI 形态：prompt 含 cached）。
const CACHE_USAGE_BODY: &str = r#"{"id":"c","object":"chat.completion","choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":100,"total_tokens":1100,"prompt_tokens_details":{"cached_tokens":600}}}"#;

/// R-quota-cost-001/005（成本档源单点 + cache 档进入成本窗）：目录登记
/// `gpt-4` 四档价（与 `[[pricing]]` 没有同价），两阶段限额分别落在
/// 「目录四档 vs [[pricing]] 两档」与「不计 cache vs 计 cache」之间：
/// 若配额侧仍走 [[pricing]] 档源或不计 cache 档，阶段 A/B 的次请求结果就会翻转。
/// 同时断言同一请求的 `/admin/cost` 分账总额与配额扣减同源（±1 millicent）。
#[tokio::test]
async fn cost_window_uses_catalog_source_and_counts_cache_tiers() {
    let catalog = catalog_entry(1000.0, 2000.0, 10000.0, 0.0);
    let (pre_tokens, pre_cost) = precharge_for(
        &chat_body(false),
        &catalog,
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );
    // 三种可能口径（全部由同一组输入推导，限额取中点做判别）。
    let catalog_actual = actual_cost_millicents(&cost_input(
        1000,
        100,
        600,
        0,
        &catalog,
        &pricing_entries(),
        "openai",
    ));
    let pricing_actual = actual_cost_millicents(&cost_input(
        1000,
        100,
        600,
        0,
        &no_catalog(),
        &pricing_entries(),
        "openai",
    ));
    let cache_ignored = actual_cost_millicents(&cost_input(
        1000,
        100,
        0,
        0,
        &catalog,
        &pricing_entries(),
        "openai",
    ));
    assert!(
        cache_ignored < catalog_actual && catalog_actual < pricing_actual,
        "判别前提：{cache_ignored} < {catalog_actual} < {pricing_actual}"
    );
    let quota_a = (catalog_actual + pricing_actual) / 2;
    let quota_b = (cache_ignored + catalog_actual) / 2;
    assert!(
        pre_cost <= quota_b,
        "预扣必须能进入两阶段（pre={pre_cost} > quota_b={quota_b} 则边际失效）"
    );

    // 阶段 A：目录档源扣减 → 次请求仍放行；若错误地按 [[pricing]] 扣减则 429。
    let mut server_a = mockito::Server::new_async().await;
    let mock_a = server_a
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CACHE_USAGE_BODY)
        .expect(2)
        .create_async()
        .await;
    let mut config_a = quota_config_full(
        vec![openai_vendor("q-a", server_a.url())],
        pre_tokens.saturating_mul(100),
        quota_a,
        3600,
        pricing_entries(),
    );
    config_a.model_catalog = catalog.clone();
    let state_a = make_state_inner(config_a, true, true).await;
    let app_a = create_router(state_a.clone()).await;
    let (s1, text1, _) = read_text(post_chat_app(&app_a, chat_body(false)).await).await;
    assert_eq!(s1, StatusCode::OK, "阶段 A 首请求必须放行: {text1}");

    // 分账同源（单请求口径）：/admin/cost 总额 × 100k 与配额侧 catalog_actual
    // 差 ≤1 millicent。
    state_a
        .pre_aggregator
        .as_ref()
        .expect("阶段 A 已接线分账链")
        .flush()
        .await
        .expect("预聚合落库");
    let (status, body) = read_json(get_app(&app_a, "/admin/cost?group_by=vendor").await).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["rows"]
        .as_array()
        .and_then(|r| r.first())
        .cloned()
        .unwrap_or(Value::Null);
    let ledger_millicents =
        (row["total_cost"].as_f64().unwrap_or(0.0) * MILLICENTS_PER_USD).round() as u64;
    assert!(
        ledger_millicents.abs_diff(catalog_actual) <= 1,
        "分账成本与配额扣减必须同源：admin/cost={row} vs 配额={catalog_actual}"
    );
    assert_eq!(
        row["cache_read_tokens"].as_i64(),
        Some(600),
        "cache 用量已入账"
    );
    assert_eq!(row["request_count"].as_i64(), Some(1), "单请求行");

    // 次请求仍放行 → 扣减取目录档；若误取 `[[pricing]]` 档，首请求就扣
    // {pricing_actual} > 限额 {quota_a}，次请求会 429。
    let (s2, text2, _) = read_text(post_chat_app(&app_a, chat_body(false)).await).await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "阶段 A 次请求必须放行（配额按目录四档扣减）: {text2}"
    );
    mock_a.assert_async().await;

    // 阶段 B：限额落在「不计 cache」与「计 cache」之间 → 次请求必须 429；
    // 若 cache 两档未进入成本口径则只会放行。
    let mut server_b = mockito::Server::new_async().await;
    let mock_b = server_b
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(CACHE_USAGE_BODY)
        .expect(1)
        .create_async()
        .await;
    let mut config_b = quota_config_full(
        vec![openai_vendor("q-b", server_b.url())],
        pre_tokens.saturating_mul(100),
        quota_b,
        3600,
        pricing_entries(),
    );
    config_b.model_catalog = catalog;
    let state_b = make_state(config_b, true).await;
    let app_b = create_router(state_b.clone()).await;
    let (first, _, _) = read_text(post_chat_app(&app_b, chat_body(false)).await).await;
    assert_eq!(first, StatusCode::OK, "阶段 B 首请求必须放行");
    let (second, _, has_retry) = read_text(post_chat_app(&app_b, chat_body(false)).await).await;
    assert_eq!(
        second,
        StatusCode::TOO_MANY_REQUESTS,
        "cache 两档必须进入成本窗（否则 {cache_ignored} ≤ {quota_b} 会放行）"
    );
    assert!(has_retry, "成本超限必须带 Retry-After");
    mock_b.assert_async().await;
}

// ==================== T003：透传流结算（R-quota-cost-002 流式侧补齐） ====================

/// Anthropic 入站 + Anthropic 上游 = 真透传流（走 `passthrough_stream_body`）。
fn claude_vendor(id: &str, api_base: String) -> VendorConfig {
    common::ensure_test_api_key();
    let mut v = common::base_vendor(id, api_base);
    v.type_ = "anthropic".into();
    v.models = vec![ModelConfig {
        model_id: "claude-x".into(),
        abstract_id: Some("claude-x".into()),
        weight: 1.0,
        enabled: true,
    }];
    v
}

fn messages_body(stream: bool) -> Value {
    json!({
        "model": "claude-x",
        "messages": [{"role": "user", "content": LONG_TEXT}],
        "max_tokens": 10,
        "stream": stream,
    })
}

/// 透传流上游帧：`message_start`（input 3 / output 1）+ 内容帧 +
/// `message_delta`（output 末值 2）+ `message_stop` → 实际用量 (3, 2)。
fn claude_stream_sse() -> String {
    format!(
        "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{}}\n\n",
        json!({"type":"message_start","message":{"id":"msg_p","type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":3,"output_tokens":1}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello-passthrough"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
    )
}

/// OpenAI 透传流不带 usage 尾帧（未带 `stream_options.include_usage`）。
fn openai_stream_sse_without_usage() -> String {
    "data: {\"id\":\"p\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n".to_string()
}

async fn post_messages_app(app: &axum::Router, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-good-key")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// R-quota-cost-002（透传流）：正常收束后按实际回补——限额 = 一次预扣 + 实际
/// 用量，无回补则次请求 2P > Q 必 429，有回补时 5 + P ≤ Q 放行。
#[tokio::test]
async fn passthrough_stream_settles_quota_after_stream_end() {
    let p = estimate_messages_tokens(&messages_body(true)) + 10;
    assert!(p > 15, "边际要求 P 远大于实际用量，实测 P={p}");
    let quota = p + 15;

    let sse = claude_stream_sse();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(2)
        .create_async()
        .await;

    let state = make_state(
        quota_config(vec![claude_vendor("p-claude", server.url())], quota, 3600),
        true,
    )
    .await;
    let app = create_router(state).await;
    let (s1, text1, _) = read_text(post_messages_app(&app, messages_body(true)).await).await;
    assert_eq!(s1, StatusCode::OK, "透传流首请求必须放行: {text1}");
    assert!(
        text1.contains("hello-passthrough"),
        "透传内容原样转发: {text1}"
    );

    let (s2, text2, _) = read_text(post_messages_app(&app, messages_body(true)).await).await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "流结束回补后 5 + P ≤ Q 必须放行（无回补则 2P > Q 必 429）: {text2}"
    );

    mock.assert_async().await;
}

/// R-quota-cost-002（条件性缺口）：OpenAI 透传流不带 `include_usage` → 全程
/// 无可解析 usage → 不回补，次请求因预扣仍占额而 429（锁定模块头声明口径）。
#[tokio::test]
async fn passthrough_stream_without_usage_keeps_precharge() {
    let p = estimate_messages_tokens(&chat_body(true)) + 10;
    let quota = p + 5;

    let sse = openai_stream_sse_without_usage();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(1)
        .create_async()
        .await;

    let state = make_state(
        quota_config(vec![openai_vendor("p-openai", server.url())], quota, 3600),
        true,
    )
    .await;
    let app = create_router(state).await;
    let (s1, _, _) = read_text(post_chat_app(&app, chat_body(true)).await).await;
    assert_eq!(s1, StatusCode::OK, "首请求预扣 P ≤ Q 必须放行");

    let (s2, text2, has_retry) = read_text(post_chat_app(&app, chat_body(true)).await).await;
    assert_eq!(
        s2,
        StatusCode::TOO_MANY_REQUESTS,
        "无 usage 透传流不得回补（否则次请求会放行）: {text2}"
    );
    assert!(has_retry, "超限必须带 Retry-After");

    mock.assert_async().await;
}

/// R-quota-cost-002（零包装形态一）：`quota.enabled = false` 时透传帧字节与
/// 包装前逐字节一致（等价于上游帧原文 + 定界空行），且无网关 error 帧。
#[tokio::test]
async fn passthrough_stream_bytes_unchanged_when_quota_disabled() {
    let sse = claude_stream_sse();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(1)
        .create_async()
        .await;

    let mut config = quota_config(vec![claude_vendor("off-claude", server.url())], 1, 3600);
    config.quota.enabled = false;
    let state = make_state(config, true).await;
    let app = create_router(state).await;
    let (status, text, _) = read_text(post_messages_app(&app, messages_body(true)).await).await;
    assert_eq!(status, StatusCode::OK, "关闭开关时不得判限: {text}");
    assert_eq!(text, sse, "关闭时透传字节逐字节不变");

    mock.assert_async().await;
}

/// R-quota-cost-002（零包装形态二）：`quota.enabled = true` 但该 key 的
/// token 与成本限额均为 0 → 同样零包装（字节不变、不误判限）。
#[tokio::test]
async fn passthrough_stream_bytes_unchanged_when_key_has_no_limits() {
    let sse = claude_stream_sse();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(&sse)
        .expect(1)
        .create_async()
        .await;

    // quota_config 的 cost_quota_millicents 恒 0，token_quota 传 0 → 双不限。
    let state = make_state(
        quota_config(vec![claude_vendor("free-claude", server.url())], 0, 3600),
        true,
    )
    .await;
    let app = create_router(state).await;
    let (status, text, _) = read_text(post_messages_app(&app, messages_body(true)).await).await;
    assert_eq!(status, StatusCode::OK, "双限额为 0 时不得判限: {text}");
    assert_eq!(text, sse, "不限额时透传字节逐字节不变");

    mock.assert_async().await;
}

/// M6（缓存 HIT 结算）：确定性请求重复命中缓存时无上游往返，以实际用量 0
/// 结算回补入口预扣——缓存重复请求不虚耗配额（无修复时第二次预扣滞留账本，
/// 第三个请求即被 429）。
#[tokio::test]
async fn cache_hit_requests_settle_zero_and_do_not_burn_quota() {
    let (pre_tokens, _) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );
    // 限额恰好容纳「一次实际用量 + 两次预扣」：无 HIT 回补时第三次必拒。
    let token_quota = pre_tokens.saturating_mul(2) + (ACTUAL_USAGE.0 + ACTUAL_USAGE.1);

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(1) // 只有首请求达上游，其余走缓存 HIT
        .create_async()
        .await;

    let mut config = quota_config(
        vec![openai_vendor("hit-a", server.url())],
        token_quota,
        3600,
    );
    config.cache.enabled = true;
    let state = make_state_with_cache(config, true).await;
    let app = create_router(state).await;

    // 首请求 MISS：预扣后按实际用量 (3+2) 结算。
    let (first, body, _) = read_body(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(first, StatusCode::OK, "{body}");
    assert_eq!(body["x_cache"], Value::Null);
    // 次请求 HIT：预扣被 0 结算全额回补。
    let second = post_chat_app(&app, chat_body(false)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second
            .headers()
            .get("x-cache")
            .and_then(|v| v.to_str().ok()),
        Some("HIT"),
        "第二请求必须命中缓存"
    );

    // 第三请求仍放行（预扣空间未被 HIT 请求虚耗）——无修复时此处 429。
    let (third, body, has_retry) = read_body(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(third, StatusCode::OK, "HIT 不得虚耗配额: {body}");
    assert!(!has_retry);

    mock.assert_async().await;
}

// ==================== T005：admission × quota 叠加（R-quota-cost-001 第三条） ====================

/// 拥塞 shed 造数用的挂起监听地址（只 accept 不响应，同 admission_test 手法）。
fn hanging_base() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, format!("http://127.0.0.1:{port}/v1"))
}

/// 拥塞驱动配置：dedicated key 在挂起监听上占满 2 个并发 permit（在途最小档
/// = dedicated），受限 key（GOOD_KEY，elastic 档）携带 token 窗限额。
fn admission_stacked_config(api_base: String, admission_enabled: bool, token_quota: u64) -> Config {
    let mut config = quota_config_full(
        vec![openai_vendor("adm-a", api_base)],
        token_quota,
        0,
        3600,
        Vec::new(),
    );
    config.admission = nexusflow::config::AdmissionSection {
        enabled: admission_enabled,
        ..Default::default()
    };
    // 挂起监听上的请求需长时间持有 permit：vendor 超时拉到 1.5s。
    let dedicated = digest_of("dedicated-key");
    for vendor in config.vendors.iter_mut() {
        vendor.timeout_ms = 1_500;
        vendor.stream_ttft_ms = 1_500;
        vendor.stream_idle_ms = 1_500;
        vendor.concurrency.max_concurrent_requests = Some(2);
        vendor.concurrency.requests_per_second = 100;
    }
    if let Some(auth) = config.auth.as_mut() {
        auth.api_keys.push(dedicated.clone());
        auth.keys.push(nexusflow::config::AuthKeyEntry {
            hash: dedicated,
            tier: AuthTier::Dedicated,
            token_quota: 0,
            cost_quota_millicents: 0,
        });
    }
    config
}

fn digest_of(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}

async fn post_chat_with_key(app: &axum::Router, key: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::from(serde_json::to_vec(&chat_body(false)).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// R-quota-cost-001（三闸门叠加·准入为触发方）：配额窗宽裕、准入拥塞 →
/// 429 且响应体为准入文案（非配额文案），两文案必不相等 → 触发方可辨识。
#[tokio::test]
async fn stacked_admission_rejection_is_distinguishable_from_quota() {
    let (_hanging, base) = hanging_base();
    let state = make_state(admission_stacked_config(base, true, 1_000_000), true).await;
    let app = create_router(state).await;

    // 两个 dedicated 请求挂在不响应监听上：占满 2 个并发 permit（占用率 1.0）
    // 且持有中间件在途档 → 在途最小档 = dedicated。
    let mut holders = Vec::new();
    for _ in 0..2 {
        let held = app.clone();
        holders.push(tokio::spawn(async move {
            post_chat_with_key(&held, "dedicated-key").await.status()
        }));
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    // elastic 档的测试 key 低于在途最小档 → 准入 shed（配额窗仍宽裕）。
    let (status, text, has_retry) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
    assert!(has_retry, "准入 429 必须带 Retry-After");
    let admission_text = t("router-admission-rejected");
    assert!(
        text.contains(admission_text.as_str()),
        "拒因必须是准入文案: {text}"
    );
    assert_ne!(
        admission_text,
        t("middleware-quota-exceeded"),
        "准入与配额文案必须可区分"
    );

    for holder in holders {
        let _ = holder.await;
    }
}

/// R-quota-cost-001（三闸门叠加·配额为触发方）：同形态配置但 `[admission]`
/// 关闭 + 配额紧（token 窗 1）→ 429 且响应体为配额文案。
#[tokio::test]
async fn stacked_quota_rejection_is_distinguishable_when_admission_disabled() {
    let (_hanging, base) = hanging_base();
    let state = make_state(admission_stacked_config(base, false, 1), true).await;
    let app = create_router(state).await;

    let (status, text, has_retry) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{text}");
    assert!(has_retry, "配额 429 必须带 Retry-After");
    assert!(
        text.contains(t("middleware-quota-exceeded").as_str()),
        "拒因必须是配额文案: {text}"
    );
}

/// R-quota-cost-001（三闸门叠加·不误拒）：准入开启但不拥塞且配额宽裕 →
/// 请求正常送达上游（两闸门均不吞正常请求）。
#[tokio::test]
async fn stacked_gates_admission_open_and_quota_ample_passes() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        .expect(1)
        .create_async()
        .await;

    let state = make_state(
        admission_stacked_config(server.url(), true, 1_000_000),
        true,
    )
    .await;
    let app = create_router(state).await;
    let (status, text, _) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(status, StatusCode::OK, "不拥塞 + 宽配额必须放行: {text}");

    mock.assert_async().await;
}

/// R-quota-cost-001（per-key 隔离）：GOOD_KEY 的 token 窗打满触发 429 时，
/// 同窗内第二个宽限额 key 照常放行——两 key 各自建账，互不污染。
#[tokio::test]
async fn quota_windows_are_isolated_per_key() {
    let (pre_tokens, _) = precharge_for(
        &chat_body(false),
        &no_catalog(),
        &pricing_entries(),
        EST_CHARS_PER_TOKEN,
    );
    // GOOD_KEY：限额 = 一次预扣 + 实际用量 → 每请求回补后净增「实际用量」，
    // 第三次预扣必越界（前两次各占 5）。第二 key：100× 预扣，全程宽裕。
    let tight = pre_tokens.saturating_add(ACTUAL_USAGE.0 + ACTUAL_USAGE.1);
    let ample = pre_tokens.saturating_mul(100);

    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ACTUAL_USAGE_BODY)
        // GOOD_KEY 2 次 + 第二 key 2 次；GOOD_KEY 第三次被拒不得达上游。
        .expect(4)
        .create_async()
        .await;

    let second_hash = digest_of("second-key");
    let mut config = quota_config(vec![openai_vendor("iso-a", server.url())], tight, 3600);
    if let Some(auth) = config.auth.as_mut() {
        auth.api_keys.push(second_hash.clone());
        auth.keys.push(nexusflow::config::AuthKeyEntry {
            hash: second_hash,
            tier: AuthTier::Elastic,
            token_quota: ample,
            cost_quota_millicents: 0,
        });
    }
    let state = make_state(config, true).await;
    let app = create_router(state).await;

    let (g1, text_g1, _) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(g1, StatusCode::OK, "GOOD_KEY 首请求预扣恰等限额: {text_g1}");
    let (g2, text_g2, _) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(
        g2,
        StatusCode::OK,
        "GOOD_KEY 次请求仍在限额内（回补后账本 = 实际用量×2）: {text_g2}"
    );
    let (g3, text_g3, has_retry) = read_text(post_chat_app(&app, chat_body(false)).await).await;
    assert_eq!(
        g3,
        StatusCode::TOO_MANY_REQUESTS,
        "GOOD_KEY 窗内第三次预扣必越界 → 429: {text_g3}"
    );
    assert!(has_retry, "配额 429 必须带 Retry-After");

    for i in 0..2 {
        let (status, text, _) = read_text(post_chat_with_key(&app, "second-key").await).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GOOD_KEY 越界不得波及第二 key 的窗（第 {i} 次）: {text}"
        );
    }

    mock.assert_async().await;
}
