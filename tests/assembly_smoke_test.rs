#![cfg(feature = "web-server")]

//! 装配链冒烟测试 — 验证生产装配（sdforge 站点 + create_router 合并）
//! 与认证挂载的最小行为契约。
//!
//! 断言矩阵（tower::ServiceExt::oneshot，无网络依赖）：
//! 1. sdforge build() + create_router merge 不 panic（站点路由与 /v1 共存，
//!    回归"merge 冲突 duplicate-route panic"风险）
//! 2. /api/v1/health → 200（sdforge #[forge(version="v1")] 注册的站点路由
//!    在 merge 后可达；带版本前缀）
//! 3. /v1/chat/completions 无 key → 401（认证层拦截）
//! 4. /v1/chat/completions 带 [auth] 键 → 非 401/404（进入 handler；
//!    body 缺 model 字段在路由决策前返回 400，不触发上游调用）
//! 5. /admin/metrics 无 key → 401
//! 6. /admin/metrics 带键 → 200

#![cfg(feature = "web-server")]

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use dbnexus::{DbConfig, DbPool};
use nexusflow::api::create_router;
use nexusflow::config::{
    AuthSection, Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig,
    ServerConfig, VendorConfig,
};
use nexusflow::database::Database;
use nexusflow::middleware::ApiAuthState;
use nexusflow::protocol::{AnthropicConverter, OpenAIConverter};
use nexusflow::router::Router as ModelRouter;
use nexusflow::vendor::VendorManager;
use nexusflow::ApiState;
use std::sync::Arc;
use tower::ServiceExt;

/// sha256("smoke-test-key")（sha256sum 实测）— 与 [auth].api_keys 的摘要形态一致
const SMOKE_KEY: &str = "smoke-test-key";
const SMOKE_KEY_HASH: &str = "e9547242bd6a03bfab1da509ab0da8b70381955dbb0f3a36d0fe292e58a9b9e9";

fn assert_valid_sha256_hex() {
    // 防止硬编码摘要与真实 sha256 漂移：长度 + hex 字符集静态校验
    assert_eq!(
        SMOKE_KEY_HASH.len(),
        64,
        "sha256 hex digest must be 64 chars"
    );
    assert!(
        SMOKE_KEY_HASH
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "sha256 hex digest must be lowercase hex"
    );
}

fn test_config() -> Config {
    Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
        },
        vendors: vec![VendorConfig {
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            id: "openai_main".into(),
            name: "OpenAI Main".into(),
            type_: "openai".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![ModelConfig {
                model_id: "gpt-4".into(),
                abstract_id: Some("gpt-4".into()),
                weight: 1.0,
                enabled: true,
            }],
            rate_limit: nexusflow::config::RateLimitConfig::Token {
                max_tokens: 1_000_000,
                window_hours: None,
                sliding: false,
            },
            concurrency: nexusflow::config::ConcurrencyConfig {
                requests_per_second: 1000,
                max_concurrent_requests: None,
                token_weighted: false,
                token_ticket_min: 1,
                token_ticket_max: 16384,
                token_ticket_default: 4096,
            },
        }],
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![],
        auth: Some(AuthSection {
            enabled: true,
            api_keys: vec![SMOKE_KEY_HASH.to_string()],
            keys: vec![],
            header: Default::default(),
        }),
        cors: None,
        retry: nexusflow::config::RetrySection::default(),
        routing: nexusflow::config::RoutingSection::default(),
        cache: nexusflow::config::CacheSection::default(),
        admission: Default::default(),
        resilience: nexusflow::config::ResilienceSection::default(),
    }
}

async fn build_state() -> Arc<ApiState> {
    let config_manager = Arc::new(ConfigManager::new(test_config()));
    // 注入临时路径：auth 热生效测试需要经 update_config 切换 [auth] 段
    let key_store = std::env::temp_dir().join(format!("nx-smoke-auth-{}.toml", std::process::id()));
    let _ = std::fs::remove_file(&key_store);
    std::fs::write(&key_store, "smoke").expect("seed smoke config file");
    config_manager.set_path(key_store);
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));

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
    .expect("sqlite::memory: pool should build");

    let database = Arc::new(Database {
        pool: pool.clone(),
        repository: Arc::new(nexusflow::database::Repository::new(pool)),
    });

    let router = Arc::new(ModelRouter::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));

    Arc::new(ApiState {
        config: config_manager,
        vendor_manager,
        router,
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database,
    })
}

/// 生产装配的等价物：sdforge 站点路由（#[forge] 注册）+ 业务路由合并
async fn build_app() -> Router {
    build_app_with_state().await.0
}

/// 同 build_app，但同时返回 ConfigManager（auth 热生效断言需要经它
/// update [auth] 段）
async fn build_app_with_state() -> (Router, Arc<ConfigManager>) {
    let state = build_state().await;
    let config_manager = state.config.clone();
    let site = sdforge::http::build();
    let app = site.merge(create_router(state).await);
    (app, config_manager)
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, axum::body::Bytes) {
    let response = app.oneshot(req).await.expect("oneshot should not fail");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body should read");
    (status, bytes)
}

#[tokio::test]
async fn test_health_reachable_after_merge() {
    assert_valid_sha256_hex();
    let app = build_app().await;
    // 实际注册路径带版本前缀：#[forge(version = "v1")] 宏把 path 变为
    // /api/v1/health（sdforge-macros lib.rs:1886），裸 /health 不存在
    let (status, _) = send(
        app,
        Request::builder()
            .uri("/api/v1/health")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "/api/v1/health must survive the merge"
    );
}

#[tokio::test]
async fn test_v1_without_key_is_401() {
    let app = build_app().await;
    let (status, body) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"gpt-4"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no key → 401");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON error body");
    assert_eq!(value["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn test_v1_with_valid_key_passes_auth() {
    let app = build_app().await;
    // body 缺 model 字段：认证通过后 handler 在路由决策前返回 400，
    // 不触发任何上游 HTTP 调用
    let (status, _) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", SMOKE_KEY))
            .body(Body::from(r#"{"messages":[]}"#))
            .unwrap(),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "valid key must not be rejected by auth"
    );
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "route must exist after merge"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing model → 400");
}

#[tokio::test]
async fn test_v1_with_invalid_key_is_401() {
    let app = build_app().await;
    let (status, _) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong-key")
            .body(Body::from(r#"{"model":"gpt-4"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong key → 401");
}

#[tokio::test]
async fn test_admin_metrics_without_key_is_401() {
    let app = build_app().await;
    let (status, _) = send(
        app,
        Request::builder()
            .uri("/admin/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "admin surface requires key"
    );
}

#[tokio::test]
async fn test_admin_metrics_with_valid_key_is_200() {
    let app = build_app().await;
    let (status, body) = send(
        app,
        Request::builder()
            .uri("/admin/metrics")
            .header("x-api-key", SMOKE_KEY)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "valid key on admin surface → 200");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("metrics JSON");
    assert!(value["total_requests"].is_number(), "metrics shape intact");
}

/// auth 状态从 [auth] 摘要构造的单元级复核：与 middleware/auth.rs 测试
/// 互补，锁定 create_router 使用的是同一套摘要比较。
#[tokio::test]
async fn test_auth_state_from_config_hashes() {
    let section = AuthSection {
        enabled: true,
        api_keys: vec![SMOKE_KEY_HASH.to_string()],
        keys: vec![],
        header: Default::default(),
    };
    let state = ApiAuthState::from_section(&section);
    assert!(state.validate_key(SMOKE_KEY));
    assert!(!state.validate_key("other"));
}

/// [auth] 热生效端到端（评审 major-2 回归）：同一 Router 上 POST /admin/config
/// 等价的 update_config 吊销旧 key、启用新 key，后续请求立即用新 key 集——
/// 认证中间件每请求从 ConfigManager 热读 [auth] 段，无"返回 success 但
/// 继续用旧 key 集直到重启"的静默失效。
#[tokio::test]
async fn test_auth_key_revocation_takes_effect_immediately() {
    // sha256("rotated-key")（sha256sum 实测）
    const ROTATED_KEY: &str = "rotated-key";
    const ROTATED_KEY_HASH: &str =
        "5be3b05a2339aecdb0c543afefc6a563085f5861e6c109dce236363b3319566e";

    let (app, config) = build_app_with_state().await;

    // 1. 初始 key 可用
    let (status, _) = send(
        app.clone(),
        Request::builder()
            .uri("/admin/metrics")
            .header("x-api-key", SMOKE_KEY)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "初始 key 应放行");

    // 2. 热更新 [auth]：吊销旧 key、启用轮换 key（不改其他段）
    let mut next = config.get_config();
    next.auth = Some(AuthSection {
        enabled: true,
        api_keys: vec![ROTATED_KEY_HASH.to_string()],
        keys: vec![],
        header: Default::default(),
    });
    config
        .update_config(next)
        .expect("auth hot update should succeed");

    // 3. 旧 key 立即失效（同一 Router，未重建）
    let (status, _) = send(
        app.clone(),
        Request::builder()
            .uri("/admin/metrics")
            .header("x-api-key", SMOKE_KEY)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "被吊销的 key 必须立即失效（无重启）"
    );

    // 4. 轮换 key 立即可用
    let (status, _) = send(
        app.clone(),
        Request::builder()
            .uri("/admin/metrics")
            .header("x-api-key", ROTATED_KEY)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "轮换后的新 key 应立即放行");

    // 5. enabled=false 热关闭认证：任意 key 放行
    let mut next = config.get_config();
    if let Some(auth) = next.auth.as_mut() {
        auth.enabled = false;
    }
    config
        .update_config(next)
        .expect("auth disable update should succeed");
    let (status, _) = send(
        app,
        Request::builder()
            .uri("/admin/metrics")
            .header("x-api-key", "anything")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "enabled=false 热关闭后放行");
}
