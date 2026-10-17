// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! 请求追踪——响应头 `X-Request-Id` 回传 + tracing span 关联。

use axum::{body::Body, http::Request, http::StatusCode, Router};
use dbnexus::{DbConfig, DbPool};
use fluxgate::{
    api::create_router,
    config::{Config, ConfigManager, DatabaseConfig},
    database::Database,
    protocol::{AnthropicConverter, OpenAIConverter},
    vendor::VendorManager,
    ApiState,
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing_subscriber::{layer::SubscriberExt, registry::LookupSpan, Layer};

/// 收集新建 span 的全部字段（debug 渲染），供断言 request_id 关联。
#[derive(Clone, Default)]
struct SpanCollector {
    spans: Arc<Mutex<Vec<String>>>,
}

struct FieldWriter<'a>(&'a mut String);

impl<'a> tracing::field::Visit for FieldWriter<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={:?};", field.name(), value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push_str(&format!("{}={};", field.name(), value));
    }
}

impl<S> Layer<S> for SpanCollector
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = String::new();
        attrs.record(&mut FieldWriter(&mut fields));
        if let Some(span) = ctx.span(id) {
            fields.push_str(&format!("span={};", span.name()));
        }
        self.spans.lock().unwrap().push(fields);
    }
}

fn collector() -> &'static SpanCollector {
    static COLLECTOR: std::sync::OnceLock<SpanCollector> = std::sync::OnceLock::new();
    COLLECTOR.get_or_init(|| {
        let collector = SpanCollector::default();
        let subscriber = tracing_subscriber::registry().with(collector.clone());
        // 进程内只允许一次全局装配
        let _ = tracing::subscriber::set_global_default(subscriber);
        collector
    })
}

fn make_config() -> Config {
    Config {
        server: fluxgate::config::ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        database: DatabaseConfig {
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
        lexcompress: Default::default(),
        rate_limit: fluxgate::config::RateLimitSection { rules: vec![] },
    }
}

async fn build_app() -> Router {
    let config_manager = Arc::new(ConfigManager::new(make_config()));
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
    .unwrap();
    let database = Arc::new(Database {
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
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database,
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota: None,
        difficulty: None,
    });
    create_router(state).await
}

/// 客户端透传的 X-Request-Id：响应原样回传，且同请求 span 携带该 ID。
#[tokio::test]
async fn request_id_echoed_and_bound_to_span() {
    let collector = collector();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let request_id = format!("t009-{nanos}");
    let app = build_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("X-Request-Id", request_id.clone())
                .body(Body::from(
                    r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let echoed = resp
        .headers()
        .get("x-request-id")
        .expect("响应必须回传 X-Request-Id")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(echoed, request_id, "透传 ID 必须原样回传");

    // 给 span 落盘一点时间（span 在 handler 返回后关闭，收集同步发生；
    // oneshot 返回后收集已完成，此处直接断言）
    let spans = collector.spans.lock().unwrap();
    assert!(
        spans.iter().any(
            |s| s.contains(&format!("request_id={request_id};")) && s.contains("span=request;")
        ),
        "同请求 span 必须可见该 request_id，got {spans:?}"
    );
}

/// 未带 X-Request-Id：网关生成（req- 前缀）并回传。
#[tokio::test]
async fn missing_request_id_is_generated() {
    collector();
    let app = build_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .body(Body::from(
                    r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let echoed = resp
        .headers()
        .get("x-request-id")
        .expect("响应必须回传生成的 X-Request-Id")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        echoed.starts_with("req-"),
        "生成的 ID 应带 req- 前缀，got {echoed}"
    );
}
