// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! web-search 端点集成测试（feature 门控，不入 full）：
//! /v1/fetch 约束面（尺寸上限 / 超时 / 内容白名单 / 重定向禁跟随 / 开关门）。

#![cfg(all(feature = "web-search", feature = "web-server"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use fluxgate::api::web_search::WebSearchRuntime;
use fluxgate::utils::netguard::{GuardedFetcher, Resolver};
use fluxgate::vendor::VendorManager;
use fluxgate::{ApiState, Config, ConfigManager};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// mock DNS：解析恒返回环回（fetch 目标即本机 mockito）
struct LoopbackResolver;

#[async_trait::async_trait]
impl Resolver for LoopbackResolver {
    async fn resolve(&self, _host: &str) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec![IpAddr::from([127, 0, 0, 1])])
    }
}

/// 判定缝恒 Allow：集成面观测的是端点约束接线（判定本体在 netguard 单测钉住）
fn permissive_fetcher(timeout_ms: u64, max_bytes: usize) -> GuardedFetcher {
    GuardedFetcher::with_parts(
        Arc::new(LoopbackResolver),
        Arc::new(|_| fluxgate::utils::netguard::Verdict::Allow),
        Duration::from_millis(timeout_ms),
        max_bytes,
    )
}

fn make_config_with(section: fluxgate::config::WebSearchSection) -> Config {
    fluxgate::config::Config {
        server: fluxgate::config::ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        database: fluxgate::config::DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors: vec![],
        global_model: None,
        pricing: vec![],
        model_catalog: Vec::new(),
        auth: None,
        cors: None,
        retry: fluxgate::config::RetrySection::default(),
        routing: fluxgate::config::RoutingSection::default(),
        cache: fluxgate::config::CacheSection::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: fluxgate::config::ResilienceSection::default(),
        docs: Default::default(),
        rate_limit: fluxgate::config::RateLimitSection { rules: vec![] },
        lexcompress: Default::default(),
        responses: Default::default(),
        otlp: Default::default(),
        vendor_budgets: Vec::new(),
        key_budgets: Default::default(),
        usage_insights: Default::default(),
        catalog_sync: Default::default(),
        subscription: Default::default(),
        subscription_accounts: Vec::new(),
        metrics: Default::default(),
        masking: Default::default(),
        web_search: section,
    }
}

fn make_config(web_search_enabled: bool) -> Config {
    make_config_with(fluxgate::config::WebSearchSection {
        enabled: web_search_enabled,
        providers: vec![],
    })
}

async fn build_app(web_search_enabled: bool, fetcher: GuardedFetcher) -> Router {
    build_app_with(make_config(web_search_enabled), fetcher).await
}

async fn build_app_with(config: Config, fetcher: GuardedFetcher) -> Router {
    let config_manager = Arc::new(ConfigManager::new(config));
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
    let database = Arc::new(fluxgate::database::Database {
        pool: pool.clone(),
        repository: Arc::new(fluxgate::database::Repository::new(pool)),
    });
    let router = Arc::new(fluxgate::router::Router::new(
        config_manager.clone(),
        vendor_manager.clone(),
    ));
    let state = Arc::new(ApiState {
        config: config_manager,
        vendor_manager,
        router,
        affinity_writer: None,
        openai_converter: Arc::new(fluxgate::protocol::OpenAIConverter::new()),
        anthropic_converter: Arc::new(fluxgate::protocol::AnthropicConverter::new()),
        database,
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota: None,
        difficulty: None,
        responses: None,
        web_search: Some(Arc::new(WebSearchRuntime::with_fetcher(fetcher))),
    });
    fluxgate::api::create_router(state).await
}

async fn post_fetch(app: Router, url: &str) -> Response {
    let body = serde_json::json!({ "url": url }).to_string();
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/fetch")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// 开关门：[web_search].enabled=false（缺省）→ 两端点一律 404。
#[tokio::test]
async fn fetch_disabled_returns_404() {
    let app = build_app(false, permissive_fetcher(2_000, 4096)).await;
    let resp = post_fetch(app, "http://public.test/x").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["type"], "web_search_disabled");
}

/// R-net-004 四约束 + 重定向：尺寸上限（413）、超时（504）、
/// 白名单外媒体（415）、text/plain 放行、301 不跟随带 Location。
#[tokio::test]
async fn fetch_enforces_size_cap_timeout_and_content_whitelist() {
    let mut server = mockito::Server::new_async().await;

    // 尺寸上限：8KB 正文 > 4KB 注入上限 → 413
    let big = server
        .mock("GET", "/big")
        .with_header("Content-Type", "text/plain")
        .with_body("x".repeat(8 * 1024))
        .create_async()
        .await;
    let app = build_app(true, permissive_fetcher(5_000, 4 * 1024)).await;
    let resp = post_fetch(app, &format!("{}/big", server.url())).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE, "超限必须 413");
    let body = body_json(resp).await;
    assert_eq!(body["error"]["type"], "fetch_body_too_large");
    big.assert();

    // 白名单外：application/octet-stream → 415
    let bin = server
        .mock("GET", "/bin")
        .with_header("Content-Type", "application/octet-stream")
        .with_body("raw")
        .create_async()
        .await;
    let app = build_app(true, permissive_fetcher(5_000, 4096)).await;
    let resp = post_fetch(app, &format!("{}/bin", server.url())).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "非白名单媒体必须 415"
    );
    let body = body_json(resp).await;
    assert_eq!(body["error"]["type"], "fetch_unsupported_media");
    bin.assert();

    // text/plain 放行 → 200 + 正文
    let text = server
        .mock("GET", "/text")
        .with_header("Content-Type", "text/plain; charset=utf-8")
        .with_body("hello fetch")
        .create_async()
        .await;
    let app = build_app(true, permissive_fetcher(5_000, 4096)).await;
    let resp = post_fetch(app, &format!("{}/text", server.url())).await;
    assert_eq!(resp.status(), StatusCode::OK, "白名单媒体必须放行");
    let body = body_json(resp).await;
    assert_eq!(body["text"], "hello fetch");
    assert!(body["content_type"]
        .as_str()
        .unwrap_or("")
        .starts_with("text/plain"));
    text.assert();

    // 超时：mock 延迟 2s > 注入的 300ms 抓取超时 → 504
    let slow = server
        .mock("GET", "/slow")
        .with_header("Content-Type", "text/plain")
        .with_chunked_body(|_w| {
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        })
        .create_async()
        .await;
    let app = build_app(true, permissive_fetcher(300, 4096)).await;
    let resp = post_fetch(app, &format!("{}/slow", server.url())).await;
    assert_eq!(
        resp.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "抓取超时必须 504"
    );
    slow.assert();

    // 301 不跟随：原状态码 + Location 头（跟随会拿到落点 200）
    let redir = server
        .mock("GET", "/redir")
        .with_status(301)
        .with_header("Location", "http://public.test/landing")
        .with_header("Content-Type", "text/plain")
        .with_body("moved")
        .create_async()
        .await;
    let app = build_app(true, permissive_fetcher(5_000, 4096)).await;
    let resp = post_fetch(app, &format!("{}/redir", server.url())).await;
    assert_eq!(
        resp.status(),
        StatusCode::MOVED_PERMANENTLY,
        "301 必须原样带出"
    );
    assert_eq!(
        resp.headers().get("location").map(|v| v.to_str().unwrap()),
        Some("http://public.test/landing"),
        "Location 必须透传给调用方"
    );
    redir.assert();
}

async fn post_search(app: Router, body: serde_json::Value) -> Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

/// R-search-003：源 1 失败源 2 成功的 fall-through；成功响应带出实际命中的
/// provider 与 OpenAI 风格 data 数组。
#[tokio::test]
async fn search_falls_through_providers() {
    let mut server = mockito::Server::new_async().await;
    std::env::set_var("WEB_SEARCH_T009_KEY", "k9");

    let fail_mock = server
        .mock("POST", "/t1/search")
        .with_status(500)
        .create_async()
        .await;
    let ok_mock = server
        .mock("GET", "/s2/search")
        .match_query(mockito::Matcher::Any)
        .with_body(r#"{"results":[{"title":"R","url":"https://r.example","content":"S"}]}"#)
        .create_async()
        .await;

    let config = make_config_with(fluxgate::config::WebSearchSection {
        enabled: true,
        providers: vec![
            fluxgate::config::WebSearchProviderConfig {
                kind: "tavily".into(),
                api_key_env: "WEB_SEARCH_T009_KEY".into(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/t1", server.url()),
            },
            fluxgate::config::WebSearchProviderConfig {
                kind: "searxng".into(),
                api_key_env: String::new(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/s2", server.url()),
            },
        ],
    });
    let app = build_app_with(config, permissive_fetcher(5_000, 4096)).await;
    let resp = post_search(app, serde_json::json!({ "query": "rust ssrf", "n": 3 })).await;
    let status = resp.status();
    let body = body_json(resp).await;
    assert_eq!(status, StatusCode::OK, "源 1 挂源 2 必须接住: {body}");
    assert_eq!(body["provider"], "searxng", "必须带出实际命中源");
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"][0]["url"], "https://r.example");
    fail_mock.assert();
    ok_mock.assert();
}

/// 全败 → 502 聚合摘要（error.type=search_all_providers_failed）。
#[tokio::test]
async fn search_all_failed_returns_502_with_summary() {
    let mut server = mockito::Server::new_async().await;
    std::env::set_var("WEB_SEARCH_T009_KEY", "k9");
    let fail1 = server
        .mock("POST", "/t1/search")
        .with_status(503)
        .create_async()
        .await;
    let fail2 = server
        .mock("GET", "/s2/search")
        .match_query(mockito::Matcher::Any)
        .with_status(500)
        .create_async()
        .await;
    let config = make_config_with(fluxgate::config::WebSearchSection {
        enabled: true,
        providers: vec![
            fluxgate::config::WebSearchProviderConfig {
                kind: "tavily".into(),
                api_key_env: "WEB_SEARCH_T009_KEY".into(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/t1", server.url()),
            },
            fluxgate::config::WebSearchProviderConfig {
                kind: "searxng".into(),
                api_key_env: String::new(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/s2", server.url()),
            },
        ],
    });
    let app = build_app_with(config, permissive_fetcher(5_000, 4096)).await;
    let resp = post_search(app, serde_json::json!({ "query": "q" })).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY, "全败必须 502");
    let body = body_json(resp).await;
    assert_eq!(body["error"]["type"], "search_all_providers_failed");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("tavily") && message.contains("searxng"),
        "摘要必须聚合各源: {message}"
    );
    fail1.assert();
    fail2.assert();
}

/// provider 参数单源：指定源失败不触达其它源（无 mock → 未被请求）。
#[tokio::test]
async fn search_provider_param_selects_single_source() {
    let mut server = mockito::Server::new_async().await;
    std::env::set_var("WEB_SEARCH_T009_KEY", "k9");
    let fail1 = server
        .mock("POST", "/t1/search")
        .with_status(500)
        .create_async()
        .await;
    let config = make_config_with(fluxgate::config::WebSearchSection {
        enabled: true,
        providers: vec![
            fluxgate::config::WebSearchProviderConfig {
                kind: "tavily".into(),
                api_key_env: "WEB_SEARCH_T009_KEY".into(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/t1", server.url()),
            },
            fluxgate::config::WebSearchProviderConfig {
                kind: "searxng".into(),
                api_key_env: String::new(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/s2", server.url()),
            },
        ],
    });
    let app = build_app_with(config, permissive_fetcher(5_000, 4096)).await;
    let resp = post_search(
        app,
        serde_json::json!({ "query": "q", "provider": "tavily" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    fail1.assert();
    let body = body_json(resp).await;
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("tavily") && !message.contains("searxng"),
        "provider 参数必须单源（未建 searxng mock，摘要只含 tavily 即证明）: {message}"
    );
}

/// R-search-004 / T011：admin stats 按 (provider, key_idx) 出成败计数，
/// 零 key 材料回显（仅 key_idx 序号）。
#[tokio::test]
async fn admin_web_search_stats_reports_counts_without_key_material() {
    let mut server = mockito::Server::new_async().await;
    std::env::set_var("WEB_SEARCH_T011_KEY", "secret-key-material");
    let fail_mock = server
        .mock("POST", "/t1/search")
        .with_status(500)
        .create_async()
        .await;
    let ok_mock = server
        .mock("GET", "/s2/search")
        .match_query(mockito::Matcher::Any)
        .with_body(r#"{"results":[{"title":"R","url":"https://r.example","content":"S"}]}"#)
        .create_async()
        .await;
    let config = make_config_with(fluxgate::config::WebSearchSection {
        enabled: true,
        providers: vec![
            fluxgate::config::WebSearchProviderConfig {
                kind: "tavily".into(),
                api_key_env: "WEB_SEARCH_T011_KEY".into(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/t1", server.url()),
            },
            fluxgate::config::WebSearchProviderConfig {
                kind: "searxng".into(),
                api_key_env: String::new(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/s2", server.url()),
            },
        ],
    });
    let app = build_app_with(config, permissive_fetcher(5_000, 4096)).await;
    let resp = post_search(app.clone(), serde_json::json!({ "query": "q" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    fail_mock.assert();
    ok_mock.assert();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/web-search/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let providers = body["providers"].as_array().expect("providers array");
    let find = |name: &str| {
        providers
            .iter()
            .find(|p| p["provider"] == name)
            .unwrap_or_else(|| panic!("provider {name} must be present"))
            .clone()
    };
    let tavily = find("tavily");
    assert_eq!(tavily["keys"][0]["key_idx"], 0, "只回 key_idx 序号");
    assert_eq!(tavily["keys"][0]["failure"], 1);
    let searxng = find("searxng");
    assert_eq!(searxng["keys"][0]["success"], 1);
    // 脱敏：全响应不得含 key 明文与 env 名
    let raw = serde_json::to_string(&body).unwrap();
    assert!(
        !raw.contains("secret-key-material") && !raw.contains("WEB_SEARCH_T011_KEY"),
        "stats 响应不得回显 key 材料: {raw}"
    );
}

/// T013：web_search_requests_total{provider,result} 与
/// netguard_denied_total{tag} 打点，经 /admin/metrics 快照断言。
#[tokio::test]
async fn metrics_record_web_search_and_netguard_denials() {
    struct PrivateResolver;
    #[async_trait::async_trait]
    impl Resolver for PrivateResolver {
        async fn resolve(&self, _host: &str) -> std::io::Result<Vec<IpAddr>> {
            Ok(vec![IpAddr::from([10, 0, 0, 7])])
        }
    }

    let mut server = mockito::Server::new_async().await;
    std::env::set_var("WEB_SEARCH_T013_KEY", "k13");
    let fail_mock = server
        .mock("POST", "/t1/search")
        .with_status(500)
        .create_async()
        .await;
    let ok_mock = server
        .mock("GET", "/s2/search")
        .match_query(mockito::Matcher::Any)
        .with_body(r#"{"results":[{"title":"R","url":"https://r.example","content":"S"}]}"#)
        .create_async()
        .await;

    // 真 guard：fetch 解析到私网 → 400 netguard_blocked（同一 ApiState 共享
    // GlobalMetrics，搜索与拒绝两类计数同池可断言）
    let config = make_config_with(fluxgate::config::WebSearchSection {
        enabled: true,
        providers: vec![
            fluxgate::config::WebSearchProviderConfig {
                kind: "tavily".into(),
                api_key_env: "WEB_SEARCH_T013_KEY".into(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/t1", server.url()),
            },
            fluxgate::config::WebSearchProviderConfig {
                kind: "searxng".into(),
                api_key_env: String::new(),
                api_key_envs: vec![],
                api_key_vault_ref: String::new(),
                base_url: format!("{}/s2", server.url()),
            },
        ],
    });
    let guarded_fetcher = GuardedFetcher::with_parts(
        Arc::new(PrivateResolver),
        Arc::new(fluxgate::utils::netguard::guard),
        Duration::from_secs(2),
        4096,
    );
    let app = build_app_with(config, guarded_fetcher).await;

    let resp = post_search(app.clone(), serde_json::json!({ "query": "q" })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    fail_mock.assert();
    ok_mock.assert();

    let resp = post_fetch(app.clone(), "http://internal.example/x").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "私网必须拒绝");
    let body = body_json(resp).await;
    assert_eq!(body["error"]["type"], "netguard_blocked");
    assert!(
        !serde_json::to_string(&body).unwrap().contains("10.0.0.7"),
        "拒绝响应不得回显内网地址"
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let metrics = body_json(resp).await;
    let web = metrics["web_search_requests_total"]
        .as_array()
        .expect("web_search_requests_total 数组");
    let find = |key: &str| {
        web.iter()
            .find(|pair| pair[0] == key)
            .map(|pair| pair[1].as_u64().unwrap_or(0))
            .unwrap_or(0)
    };
    assert_eq!(find("searxng\u{0}success"), 1, "searxng 成功计数");
    assert_eq!(find("tavily\u{0}error"), 1, "tavily 失败计数");
    let denied = metrics["netguard_denied_total"]
        .as_array()
        .expect("netguard_denied_total 数组");
    let denied_private = denied
        .iter()
        .find(|pair| pair[0] == "private")
        .map(|pair| pair[1].as_u64().unwrap_or(0))
        .unwrap_or(0);
    assert_eq!(denied_private, 1, "netguard private 段拒绝计数");
}
