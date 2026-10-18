// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::sdk::client::FluxgateClientBuilder;
use crate::sdk::error::{ApiError, FluxgateSdkError};
use std::time::Duration;

fn build_client(base_url: &str) -> crate::sdk::FluxgateClient {
    FluxgateClientBuilder::new()
        .base_url(base_url)
        .api_key("test-key")
        .build()
        .unwrap()
}

/// 路由 diff 防回归钉：SDK admin 面 (method, path) 清单必须全部
/// 出现在网关路由表源码（`src/api/router_builder.rs` axum `.route()`
/// 注册）中——SDK 方法与 server 实际路由的零差异契约。失配史：旧 SDK 打
/// `GET /v1/models`（网关未注册）与 `GET /admin/health`（不存在）。
#[test]
fn sdk_admin_routes_are_subset_of_server_route_table() {
    const ROUTE_TABLE_SRC: &str = include_str!("../../api/router_builder.rs");

    // 精确注册模式（与 router_builder.rs 当前源码逐字对齐——
    // 杜绝「路径串偶然出现」的误绿；多行 MethodRouter 链按源码拼装）
    let expected = [
        ".route(\"/admin/models\", get(admin::get_models))",
        concat!(
            ".route(\n            \"/admin/models/{id}\",\n",
            "            post(admin::update_model)\n",
            "                .put(admin::upsert_model)\n",
            "                .delete(admin::delete_model),\n",
            "        )"
        ),
        ".route(\"/admin/vendors\", get(admin::vendors))",
    ];
    for pattern in &expected {
        assert!(
            ROUTE_TABLE_SRC.contains(*pattern),
            "网关路由表缺失注册模式: {pattern:?}"
        );
    }

    // SDK (method, path) 清单逐项对位（方法 × 路径复合断言；POST global
    // 为网关保留契约、SDK 不消费，不入清单—— 删死分支）
    for (method, path) in crate::sdk::admin::sdk_routes::SDK_ADMIN_ROUTES {
        match (*method, *path) {
            ("PUT", "admin/models/{id}") => assert!(
                ROUTE_TABLE_SRC.contains(".put(admin::upsert_model)"),
                "PUT /admin/models/{{id}} 绑定缺失"
            ),
            ("DELETE", "admin/models/{id}") => assert!(
                ROUTE_TABLE_SRC.contains(".delete(admin::delete_model)"),
                "DELETE /admin/models/{{id}} 绑定缺失"
            ),
            ("GET", "admin/models") | ("GET", "admin/vendors") => {
                // 已由 expected 精确模式覆盖
            }
            ("GET", "api/v1/health") => {
                // 已由 forge 属性断言覆盖（health.rs 注册）
            }
            other => panic!("unexpected SDK route {other:?}"),
        }
    }

    // 健康面：/api/v1/health 经 sdforge forge 属性注册（version="v1"，
    // path="/health"）——属性漂移（version/path 改名）即 SDK health() 404
    const HEALTH_SRC: &str = include_str!("../../api/health.rs");
    assert!(
        HEALTH_SRC.contains("version = \"v1\"") && HEALTH_SRC.contains("path = \"/health\""),
        "health forge 注册属性漂移（SDK health() 打 /api/v1/health）"
    );
}

/// GET /admin/models — 网关真实形状：`Vec<ModelCatalogEntry>` 九字段
#[tokio::test]
async fn models_returns_catalog_entries() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"[
                {"model_id":"claude-sonnet-4-20250514","context_window":200000,
                 "input_price":3.0,"output_price":15.0,"cache_read_price":0.3,
                 "cache_creation_price":3.75,"currency":"usd","max_output_tokens":8192,
                 "supports_tools":true,"supports_thinking":true},
                {"model_id":"gpt-4o","context_window":128000}
            ]"#,
        )
        .create();

    let client = build_client(&server.url());
    let models = client.models().await.unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model_id, "claude-sonnet-4-20250514");
    assert_eq!(models[0].context_window, 200_000);
    assert!((models[0].input_price - 3.0).abs() < 1e-9);
    assert_eq!(models[0].currency, "usd");
    assert_eq!(models[0].max_output_tokens, Some(8192));
    assert!(models[0].supports_thinking);
    // 缺省字段（serde default）：第二条目只带 model_id/context_window
    assert_eq!(models[1].input_price, 0.0);
    assert!(!models[1].supports_tools);
}

/// PUT /admin/models/{id} upsert → DELETE 下条（网关真实 success JSON）
#[tokio::test]
async fn upsert_and_delete_model_roundtrip() {
    let mut server = mockito::Server::new_async().await;

    let put_mock = server
        .mock("PUT", "/admin/models/gpt-4o")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "model_id": "gpt-4o",
            "context_window": 128000
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"success":true,"message":"model gpt-4o upserted into the catalog"}"#)
        .create();

    let delete_mock = server
        .mock("DELETE", "/admin/models/gpt-4o")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"success":true,"message":"model gpt-4o removed from the catalog"}"#)
        .create();

    let client = build_client(&server.url());
    client
        .upsert_model(
            "gpt-4o",
            &crate::sdk::ModelCatalogUpsert {
                model_id: "gpt-4o".into(),
                context_window: 128_000,
                ..Default::default()
            },
        )
        .await
        .expect("upsert");

    client.delete_model("gpt-4o").await.expect("delete");

    put_mock.assert_async().await;
    delete_mock.assert_async().await;
}

/// PUT /admin/models/global — 保留键显性 400（validation_error），SDK 映射
/// Api 错误（status/kind 字段区分）
#[tokio::test]
async fn put_global_reserved_key_maps_to_server_error() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("PUT", "/admin/models/global")
        .with_status(400)
        .with_body(r#"{"error":{"type":"validation_error","message":"reserved"}}"#)
        .create();

    let client = build_client(&server.url());
    let err = client
        .upsert_model(
            "global",
            &crate::sdk::ModelCatalogUpsert {
                model_id: "global".into(),
                context_window: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 400),
        other => panic!("expected Api error, got {other:?}"),
    }
}

/// GET /admin/vendors — 网关真实形状（运行面状态机词 + 队列深度 +
/// key_pool 观测； 后字段）
#[tokio::test]
async fn vendors_returns_runtime_status_shape() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/vendors")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"[
                {"id":"openai_main","status":"Available","queue_length":0,
                 "key_pool":{"envs":["OPENAI_API_KEY"],"size":2,"current_index":1,
                             "fingerprints":["abcdef0123456789"]}},
                {"id":"anthropic_main","status":"Throttled","queue_length":3}
            ]"#,
        )
        .create();

    let client = build_client(&server.url());
    let vendors = client.vendors().await.unwrap();

    assert_eq!(vendors.len(), 2);
    assert_eq!(vendors[0].id, "openai_main");
    assert_eq!(vendors[0].status, "Available");
    assert_eq!(vendors[0].queue_length, 0);
    let pool = vendors[0].key_pool.as_ref().expect("key_pool present");
    assert_eq!(pool.size, 2);
    assert_eq!(pool.current_index, 1);
    assert_eq!(pool.envs, vec!["OPENAI_API_KEY"]);
    assert_eq!(vendors[1].status, "Throttled");
    assert!(vendors[1].key_pool.is_none(), "未初始化池为 None");
}

/// GET /api/v1/health — 网关真实形状（{status,version,timestamp}；无
/// vendors 计数字段）
#[tokio::test]
async fn health_returns_gateway_shape() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/api/v1/health")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"status":"ok","version":"0.1.0","timestamp":"2026-10-05T00:00:00+00:00"}"#)
        .create();

    let client = build_client(&server.url());
    let health = client.health().await.unwrap();

    assert_eq!(health.status, "ok");
    assert_eq!(health.version, "0.1.0");
    assert_eq!(health.timestamp, "2026-10-05T00:00:00+00:00");
}

#[tokio::test]
async fn models_500_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/models")
        .with_status(500)
        .with_body("boom")
        .create();

    let client = build_client(&server.url());
    let err = client.models().await.unwrap_err();

    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 500),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn vendors_401_returns_server_error() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("GET", "/admin/vendors")
        .with_status(401)
        .create();

    let client = build_client(&server.url());
    let err = client.vendors().await.unwrap_err();

    // admin 端点统一返回 Api 错误（status/kind 字段区分）
    match err {
        FluxgateSdkError::Api(ApiError { status, .. }) => assert_eq!(status, 401),
        other => panic!("expected Api error, got {other:?}"),
    }
}

/// 慢响应裸 TCP 服务器（mockito 无延迟 API）：按请求路径回对应 JSON
/// body，但每个连接都先停滞 `delay` 再响应。形状对齐网关真实路由。
async fn spawn_slow_admin_server(delay: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => break,
            };
            let mut buf = [0u8; 4096];
            let mut got = Vec::new();
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                }
                if got.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let path = got
                .split(|b| *b == b' ')
                .nth(1)
                .unwrap_or_default()
                .to_vec();
            let body: &[u8] =
                if path.ends_with(b"/admin/models") || path.ends_with(b"/admin/vendors") {
                    br#"[]"#
                } else {
                    br#"{"status":"ok","version":"test","timestamp":"t"}"#
                };
            tokio::time::sleep(delay).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

/// admin 请求族必须有总时长上界：服务器停滞（500ms）超过客户端总超时
///（100ms）时，三个 admin 方法都应 promptly 报 `Timeout`（`is_timeout`
/// 分流），而不是无限悬挂。流式请求不受总超时约束（由 read_timeout
/// 看护），admin / chat / embed 非流式逐请求携带总超时。
#[tokio::test]
async fn admin_requests_bounded_by_total_timeout() {
    let base_url = spawn_slow_admin_server(Duration::from_millis(500)).await;
    // max_retries(0)：本测只关心超时上界，排除重试退避的时间干扰
    let client = FluxgateClientBuilder::new()
        .base_url(&base_url)
        .api_key("test-key")
        .timeout(Duration::from_millis(100))
        .max_retries(0)
        .build()
        .unwrap();

    let models_err = tokio::time::timeout(Duration::from_secs(5), client.models())
        .await
        .expect("总超时必须切断 admin 停滞（不能拖到外层兜底）")
        .unwrap_err();
    match models_err {
        FluxgateSdkError::Timeout { source } => {
            assert!(source.is_timeout(), "models 应被总超时切断: {source}");
        }
        other => panic!("models 应报 Timeout，实际: {other:?}"),
    }

    let vendors_err = tokio::time::timeout(Duration::from_secs(5), client.vendors())
        .await
        .expect("vendors 未被总超时切断")
        .unwrap_err();
    assert!(
        matches!(vendors_err, FluxgateSdkError::Timeout { .. }),
        "vendors 应报 Timeout，实际: {vendors_err:?}"
    );

    let health_err = tokio::time::timeout(Duration::from_secs(5), client.health())
        .await
        .expect("health 未被总超时切断")
        .unwrap_err();
    assert!(
        matches!(health_err, FluxgateSdkError::Timeout { .. }),
        "health 应报 Timeout，实际: {health_err:?}"
    );
}
