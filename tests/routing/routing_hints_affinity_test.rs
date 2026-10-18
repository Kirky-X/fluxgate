// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "web-server")]

//! RoutingHints 兜底会话键 → 亲和绑定集成（N1 自 router_test 拆出）。
//!
//! RoutingHints 是 api 层类型（axum::http::HeaderMap 入参，llm-gate 零
//! axum 假设不下沉）；本测试验证「无 X-Session-Id 时同前缀 messages 哈希
//! → 同兜底会话键 → 亲和粘住同 vendor」的跨层语义，随 api 留 fluxgate。

#[path = "../common/mod.rs"]
mod common;

mod web_server_hints {
    use fluxgate::config::ConfigManager;
    use fluxgate::router::affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
    use fluxgate::router::Router;
    use fluxgate::vendor::VendorManager;
    use std::sync::Arc;
    use std::time::Duration;

    /// 两 vendor 同 abstract 模型（shared-model）—— 亲和 boost 的最小场景
    ///（与 gate_router_test affinity mod 的基座逐字段一致：并发上限 1 +
    /// shared-model 单模型）
    fn make_two_vendor_config() -> fluxgate::config::Config {
        crate::common::ensure_test_api_key();
        let vendor = |id: &str| {
            let mut v = crate::common::base_vendor(id, String::new());
            v.name = format!("Vendor {id}");
            v.models = vec![fluxgate::config::ModelConfig {
                model_id: "shared-model".to_string(),
                abstract_id: Some("shared-model".to_string()),
                weight: 1.0,
                enabled: true,

                dimensions: None,
                compat_group: None,
                model_type: None,
            }];
            v.concurrency.requests_per_second = 100;
            v.concurrency.max_concurrent_requests = Some(1);
            v
        };
        fluxgate::config::Config {
            server: fluxgate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..crate::common::test_config(vec![vendor("vendor-a"), vendor("vendor-b")])
        }
    }

    fn make_affinity_router(ttl: Duration) -> (Arc<VendorManager>, Router, Arc<AffinityTable>) {
        let config = make_two_vendor_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let table = Arc::new(AffinityTable::with_memory_backend(ttl));
        let router =
            Router::new(config_manager, vendor_manager.clone()).with_affinity(table.clone());
        (vendor_manager, router, table)
    }

    /// ④ 无 header 时同前缀 messages 哈希 → 同兜底会话键 → 同 vendor
    ///（web-server feature 下经 RoutingHints 的兜底路径）。
    #[cfg(feature = "web-server")]
    #[tokio::test]
    async fn prefix_hash_fallback_binds_same_vendor() {
        use fluxgate::api::RoutingHints;

        let (_vm, router, table) = make_affinity_router(DEFAULT_AFFINITY_TTL);

        let body_a = serde_json::json!({
            "model": "shared-model",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let body_b = serde_json::json!({
            "model": "shared-model",
            "messages": [{"role": "user", "content": "hello"}]
        });

        // 无 X-Session-Id header：兜底哈希生成会话键（同前缀 → 同键）
        let hints_a = RoutingHints::from_headers(&axum::http::HeaderMap::new());
        let key_a = hints_a
            .effective_session_key(&body_a)
            .expect("fallback key");
        let key_b = RoutingHints::from_headers(&axum::http::HeaderMap::new())
            .effective_session_key(&body_b)
            .expect("fallback key");
        assert_eq!(key_a, key_b, "同前缀 messages 必须生成同会话键");

        // 首次路由 + 成功回写（生产由 API 层做）
        let first = router
            .route_with_session("shared-model", Some(&key_a))
            .await
            .expect("route");
        table.bind(None, &key_a, &first.vendor.id).await;

        // 后续同前缀请求（key_b == key_a）粘住同一 vendor
        let second = router
            .route_with_session("shared-model", Some(&key_b))
            .await
            .expect("route");
        assert_eq!(
            second.vendor.id, first.vendor.id,
            "同前缀哈希 → 同键 → 同 vendor"
        );
    }
}
