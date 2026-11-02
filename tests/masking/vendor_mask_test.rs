// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

#![cfg(feature = "masking")]

//! 出站掩码集成测试（feature `masking` + mockito）：三协议出站形态
//! （openai / anthropic / gemini）上游只见占位符、客户端收到原值；
//! 透传流（字节级中继）在掩码启用时 fail-closed 拒发。

#[path = "../common/mod.rs"]
mod common;

mod upstream_never_sees_secret {
    use fluxgate::config::{Config, ConfigManager, MaskingSection};
    use fluxgate::error::GateError;
    use fluxgate::masking::{placeholder_for, MaskGate, MaskKind};
    use fluxgate::protocol::UnifiedRequest;
    use fluxgate::utils::metrics::GlobalMetrics;
    use fluxgate::vendor::VendorManager;
    use mockito::Matcher;
    use serde_json::{json, Value};
    use std::sync::Arc;

    /// regex 无 lookaround，密钥取正则安全形态（字母数字与连字符）。
    const SECRET: &str = "sk-vendor-masking-0123456789abcdefgh";

    fn placeholder() -> String {
        placeholder_for(MaskKind::ApiKey, SECRET)
    }

    fn echoed(text: &str) -> String {
        format!("echo {text} tail")
    }

    fn request() -> UnifiedRequest {
        serde_json::from_value(json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": format!("hello {SECRET} tail")}],
            "max_tokens": 16
        }))
        .expect("UnifiedRequest 形状合法")
    }

    fn config_with(vendor: fluxgate::config::VendorConfig) -> Config {
        let mut config = crate::common::test_config(vec![vendor]);
        config.masking = MaskingSection {
            enabled: true,
            ..Default::default()
        };
        config
    }

    async fn wired(config: Config) -> Arc<VendorManager> {
        crate::common::ensure_test_api_key();
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        vm.set_mask_gate(Arc::new(MaskGate::new(cm, Arc::new(GlobalMetrics::new()))));
        vm.initialize().await.expect("vendor 实例创建成功");
        vm
    }

    /// 客户端（IR UnifiedResponse）拿到还原后的原值文本。
    fn response_text(response: &fluxgate::protocol::UnifiedResponse) -> String {
        let value = serde_json::to_value(response).expect("IR 可序列化");
        let content = &value["choices"][0]["message"]["content"];
        content.as_str().expect("text 形态").to_string()
    }

    /// openai 出站形态：上游收到的 messages content 只含占位符（PartialJson
    /// 精确内容匹配——含原值即失配 501）；客户端收到原值。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn openai_body_masked_and_client_restored() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_body(Matcher::PartialJson(json!({
                "messages": [{"role": "user", "content": format!("hello {} tail", placeholder())}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "id": "chatcmpl-masked",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": echoed(&placeholder())},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let mut vendor =
            crate::common::base_vendor("masked-openai", format!("{}/v1", server.url()));
        vendor.api_key_env = "FLUX_MASK_TEST_KEY".into();
        std::env::set_var("FLUX_MASK_TEST_KEY", "mock-key");
        let vm = wired(config_with(vendor)).await;

        let response = vm
            .execute_request("masked-openai", request())
            .await
            .expect("执行成功");
        mock.assert_async().await;
        assert_eq!(response_text(&response), echoed(SECRET), "客户端收到原值");
    }

    /// anthropic 出站形态：messages[0].content[0].text 只含占位符。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn anthropic_body_masked_and_client_restored() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/messages")
            .match_body(Matcher::PartialJson(json!({
                "messages": [{"content": [{"text": format!("hello {} tail", placeholder())}]}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "id": "msg_masked",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-mock",
                    "content": [{"type": "text", "text": echoed(&placeholder())}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 3, "output_tokens": 2}
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let mut vendor =
            crate::common::base_vendor("masked-anthropic", format!("{}/v1", server.url()));
        vendor.type_ = "anthropic".into();
        vendor.api_key_env = "FLUX_MASK_TEST_KEY".into();
        std::env::set_var("FLUX_MASK_TEST_KEY", "mock-key");
        let vm = wired(config_with(vendor)).await;

        let response = vm
            .execute_request("masked-anthropic", request())
            .await
            .expect("执行成功");
        mock.assert_async().await;
        assert_eq!(response_text(&response), echoed(SECRET), "客户端收到原值");
    }

    /// gemini 出站形态：contents[0].parts[0].text 只含占位符。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gemini_body_masked_and_client_restored() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1beta/models/gpt-4:generateContent")
            .match_body(Matcher::PartialJson(json!({
                "contents": [{"parts": [{"text": format!("hello {} tail", placeholder())}]}]
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": echoed(&placeholder())}]},
                        "finishReason": "STOP",
                        "index": 0
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 3,
                        "candidatesTokenCount": 2,
                        "totalTokenCount": 5
                    },
                    "modelVersion": "gpt-4",
                    "responseId": "r-masked"
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let mut vendor = crate::common::base_vendor("masked-gemini", server.url());
        vendor.type_ = "gemini".into();
        vendor.api_key_env = "FLUX_MASK_TEST_KEY".into();
        std::env::set_var("FLUX_MASK_TEST_KEY", "mock-key");
        let vm = wired(config_with(vendor)).await;

        let response = vm
            .execute_request("masked-gemini", request())
            .await
            .expect("执行成功");
        mock.assert_async().await;
        assert_eq!(response_text(&response), echoed(SECRET), "客户端收到原值");
    }

    /// 透传流（字节级纯中继）在掩码启用时 fail-closed 拒发：上游 0 请求、
    /// 503 `masking-unavailable`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn passthrough_stream_refused_when_masking_enabled() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body("data: {}\n\n")
            .expect(0)
            .create_async()
            .await;

        let mut vendor =
            crate::common::base_vendor("masked-passthrough", format!("{}/v1", server.url()));
        vendor.type_ = "custom-upstream".into();
        vendor.api_key_env = "FLUX_MASK_TEST_KEY".into();
        std::env::set_var("FLUX_MASK_TEST_KEY", "mock-key");
        let vm = wired(config_with(vendor)).await;

        let result = vm
            .execute_passthrough_stream_request("masked-passthrough", request_body_value())
            .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("掩码启用时透传流必须拒绝"),
        };
        match err {
            GateError::MaskingUnavailable => {
                assert_eq!(err.status_code(), 503);
                assert_eq!(err.error_code(), "masking_unavailable");
            }
            other => panic!("必须为 MaskingUnavailable，got {other:?}"),
        }
        mock.assert_async().await;
    }

    fn request_body_value() -> Value {
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": format!("hello {SECRET} tail")}],
            "stream": true
        })
    }
}

mod cache_serves_restored_form {
    use fluxgate::api::response_cache::{CacheOutcome, FinalRoute, ResponseCache};
    use fluxgate::config::{CacheSection, Config, ConfigManager};
    use fluxgate::utils::metrics::GlobalMetrics;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn config() -> Config {
        let mut config = crate::common::test_config(vec![]);
        config.cache = CacheSection {
            enabled: true,
            ttl_secs: 300,
            ..Default::default()
        };
        config
    }

    fn cache() -> ResponseCache {
        ResponseCache::new(
            Arc::new(oxcache::backend::default_memory_backend()),
            Arc::new(ConfigManager::new(config())),
            Arc::new(GlobalMetrics::new()),
        )
    }

    #[derive(Debug, Clone)]
    struct Route {
        vendor: &'static str,
        model: &'static str,
    }

    impl FinalRoute for Route {
        fn vendor_id(&self) -> &str {
            self.vendor
        }
        fn model_id(&self) -> &str {
            self.model
        }
    }

    /// 缓存客户端域一致性：同一未掩码请求两次——上游（派发闭包）只执行
    /// 一次；第二次命中缓存且内容为已还原形态（含原值、无占位符）。
    #[tokio::test]
    async fn cache_serves_restored_form() {
        let cache = cache();
        let headers = axum::http::HeaderMap::new();
        let body: Value = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hello sk-vendor-masking-0123456789abcdefgh tail"}],
            "temperature": 0
        });
        let upstream_calls = Arc::new(AtomicUsize::new(0));

        // 派发闭包模拟「掩码出站 → 上游回显占位符 → 还原回客户端域」：
        // 返回值即客户端所见（已还原形态），缓存存取都在该域
        for round in 0..2 {
            let upstream_calls = Arc::clone(&upstream_calls);
            let flow = cache
                .get_or_response(&headers, "v1", "m", &body, move || {
                    let upstream_calls = Arc::clone(&upstream_calls);
                    async move {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        Ok((
                            json!({"content": "restored-sk-vendor-masking-0123456789abcdefgh"}),
                            Route {
                                vendor: "v1",
                                model: "m",
                            },
                        ))
                    }
                })
                .await
                .expect("get_or 成功");
            assert!(flow.participated);
            if round == 0 {
                assert!(flow.computed, "首轮 MISS（leader 计算）");
            } else {
                assert!(!flow.computed, "次轮 HIT（缓存复用）");
            }
            assert!(
                flow.value["content"]
                    .as_str()
                    .expect("文本")
                    .contains("sk-vendor-masking-0123456789abcdefgh"),
                "缓存内容必须是已还原形态（含原值）: {flow:?}"
            );
            assert!(
                !flow.value.to_string().contains("{{FLUX_"),
                "缓存内容不得含占位符"
            );
        }
        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            1,
            "上游只收到一次请求"
        );

        // 键维度核对：指纹取掩码前请求体（canonical 对入站 body 计算）
        let canonical = fluxgate::api::response_cache::canonical_body(&body);
        assert!(
            canonical.contains("sk-vendor-masking-0123456789abcdefgh"),
            "缓存键基于未掩码请求体"
        );
        let outcome = cache.check(&headers, "v1", "m", &body).await;
        assert!(matches!(outcome, CacheOutcome::Hit(_)), "同请求命中");
    }
}

mod stats_endpoint {
    use fluxgate::api::admin;
    use fluxgate::config::{ConfigManager, MaskingSection};
    use fluxgate::masking::{MaskGate, RuleDetector};
    use fluxgate::utils::metrics::GlobalMetrics;
    use fluxgate::vendor::VendorManager;
    use std::sync::Arc;

    /// `stats_endpoint_excludes_values`：/admin/masking/stats 只含元数据
    ///（kind 计数与长度直方图），响应中无任何原值子串。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stats_endpoint_excludes_values() {
        let secret = "sk-stats-secret-0123456789abcdefgh";
        let mut config = crate::common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            ..Default::default()
        };
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let detector: Arc<dyn fluxgate::masking::MaskDetector> =
            Arc::new(RuleDetector::build(&[], false, &[]).expect("内置检测器"));
        let gate = Arc::new(
            MaskGate::new(cm.clone(), Arc::new(GlobalMetrics::new())).with_detector(detector),
        );
        vm.set_mask_gate(Arc::clone(&gate));
        // 产生命中（真实检测器 + 真实掩码路径 → 窗口记元数据）
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": format!("see {secret} here")}]
        });
        let (_, _ticket) = gate.mask_body(body).expect("掩码成功");

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
        .expect("内存库");
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        let state = Arc::new(fluxgate::api::ApiState {
            affinity_writer: None,
            config: Arc::clone(&cm),
            vendor_manager: vm,
            router: Arc::new(fluxgate::router::Router::new(
                Arc::clone(&cm),
                Arc::new(VendorManager::new(Arc::clone(&cm))),
            )),
            openai_converter: Arc::new(fluxgate::protocol::OpenAIConverter::new()),
            anthropic_converter: Arc::new(fluxgate::protocol::AnthropicConverter::new()),
            database: Arc::new(fluxgate::database::Database {
                pool,
                repository: repository.clone(),
            }),
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            #[cfg(feature = "web-search")]
            web_search: None,
        });
        let app = axum::Router::new()
            .route(
                "/admin/masking/stats",
                axum::routing::get(admin::masking_stats),
            )
            .with_state(state);

        use tower::ServiceExt;
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/masking/stats")
                    .body(axum::body::Body::empty())
                    .expect("请求构造"),
            )
            .await
            .expect("请求");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["enabled"], serde_json::json!(true));
        assert_eq!(value["window_hours"], serde_json::json!(24));
        assert_eq!(value["kinds"]["api_key"]["count"], serde_json::json!(1));
        // 长度直方图元数据在位（值长度 34 落 le_64 桶）
        assert_eq!(
            value["kinds"]["api_key"]["value_len_buckets"]["le_64"],
            serde_json::json!(1)
        );
        // 原值零出现：响应不含值本身（元数据口径）
        assert!(!text.contains(secret), "stats 响应不得包含原值: {text}");
        assert!(!text.contains("{{FLUX_"), "stats 响应不得含占位符: {text}");
    }
}

/// 台账 admin 面（R-masking-002 验收）：关闭语义 + 开启可查。
mod ledger_endpoint {
    use fluxgate::api::admin;
    use fluxgate::config::{ConfigManager, MaskingLedgerSection, MaskingSection};
    use fluxgate::masking::{MaskGate, MaskingLedger, RuleDetector};
    use fluxgate::utils::metrics::GlobalMetrics;
    use fluxgate::vendor::VendorManager;
    use std::sync::Arc;

    async fn api_state(
        cm: Arc<ConfigManager>,
        vm: Arc<VendorManager>,
    ) -> Arc<fluxgate::api::ApiState> {
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
        .expect("内存库");
        let repository = Arc::new(fluxgate::database::Repository::new(pool.clone()));
        let router = Arc::new(fluxgate::router::Router::new(
            Arc::clone(&cm),
            Arc::new(VendorManager::new(Arc::clone(&cm))),
        ));
        Arc::new(fluxgate::api::ApiState {
            affinity_writer: None,
            config: cm,
            vendor_manager: vm,
            router,
            openai_converter: Arc::new(fluxgate::protocol::OpenAIConverter::new()),
            anthropic_converter: Arc::new(fluxgate::protocol::AnthropicConverter::new()),
            database: Arc::new(fluxgate::database::Database { pool, repository }),
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
            responses: None,
            #[cfg(feature = "web-search")]
            web_search: None,
        })
    }

    /// `[masking].ledger.enabled=false`（默认）：404 明确关闭语义。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ledger_disabled_yields_404() {
        let cm = Arc::new(ConfigManager::new(crate::common::test_config(vec![])));
        let vm = Arc::new(VendorManager::new(Arc::clone(&cm)));
        let gate = Arc::new(MaskGate::new(
            Arc::clone(&cm),
            Arc::new(GlobalMetrics::new()),
        ));
        vm.set_mask_gate(gate);
        let state = api_state(Arc::clone(&cm), vm).await;
        let app = axum::Router::new()
            .route(
                "/admin/masking/ledger",
                axum::routing::get(admin::masking_ledger),
            )
            .with_state(state);
        use tower::ServiceExt;
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/masking/ledger")
                    .body(axum::body::Body::empty())
                    .expect("请求构造"),
            )
            .await
            .expect("请求");
        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    /// enabled=true：掩码命中落台账，查询返回 per-kind 聚合与明细，
    /// 响应不含原值；明细分页 page_size=1 生效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ledger_enabled_serves_aggregate_and_entries() {
        let secret = "sk-ledger-admin-0123456789abcdefgh";
        let mut config = crate::common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            ledger: MaskingLedgerSection {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(Arc::clone(&cm)));
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = Arc::new(MaskingLedger::with_dir(
            Some(dir.path().to_path_buf()),
            Arc::clone(&cm),
        ));
        let detector: Arc<dyn fluxgate::masking::MaskDetector> =
            Arc::new(RuleDetector::build(&[], false, &[]).expect("内置检测器"));
        let gate = Arc::new(
            MaskGate::new(Arc::clone(&cm), Arc::new(GlobalMetrics::new()))
                .with_detector(detector)
                .with_ledger(ledger),
        );
        vm.set_mask_gate(Arc::clone(&gate));
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": format!("see {secret} and again {secret}")}]
        });
        let (_, _ticket) = gate.mask_body(body).expect("掩码成功");
        let state = api_state(Arc::clone(&cm), vm).await;
        let app = axum::Router::new()
            .route(
                "/admin/masking/ledger",
                axum::routing::get(admin::masking_ledger),
            )
            .with_state(state);
        use tower::ServiceExt;
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/admin/masking/ledger?page=1&page_size=1")
                    .body(axum::body::Body::empty())
                    .expect("请求构造"),
            )
            .await
            .expect("请求");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["enabled"], serde_json::json!(true));
        assert_eq!(value["total"], serde_json::json!(2), "两次命中各记一条");
        assert_eq!(
            value["per_kind"]["api_key"],
            serde_json::json!(2),
            "per-kind 聚合在位"
        );
        assert_eq!(
            value["entries"].as_array().expect("entries").len(),
            1,
            "分页生效"
        );
        assert!(!text.contains(secret), "台账查询响应不得含原值: {text}");
    }
}
