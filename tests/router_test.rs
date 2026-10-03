#![cfg(feature = "router")]

// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, ModelConfig, RateLimitConfig, ServerConfig,
    };
    use nexusflow::router::{Router, RoutingStrategy};
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn create_router_test_config() -> Config {
        // 与共享基座的差异字段（显式保留原值）：双 vendor 双 type_ 布局、
        // Interval 限流、低 rps 与真实 db 路径是路由解析用例的语义主体
        let mut openai = common::base_vendor("openai_primary", String::new());
        openai.name = "OpenAI Primary".into();
        openai.api_key_env = "OPENAI_API_KEY".into();
        openai.models = vec![
            ModelConfig {
                model_id: "gpt-4".to_string(),
                abstract_id: Some("gpt-4".to_string()),
                weight: 1.0,
                enabled: true,
            },
            ModelConfig {
                model_id: "gpt-3.5-turbo".to_string(),
                abstract_id: Some("gpt-3.5".to_string()),
                weight: 2.0,
                enabled: true,
            },
        ];
        openai.rate_limit = RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        openai.concurrency.requests_per_second = 10;
        let mut anthropic = common::base_vendor("anthropic_primary", String::new());
        anthropic.name = "Anthropic Primary".into();
        anthropic.type_ = "anthropic".into();
        anthropic.api_key_env = "ANTHROPIC_API_KEY".into();
        anthropic.models = vec![ModelConfig {
            model_id: "claude-3-5-sonnet-20241022".to_string(),
            abstract_id: Some("claude-3.5-sonnet".to_string()),
            weight: 1.5,
            enabled: true,
        }];
        anthropic.rate_limit = RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        anthropic.concurrency.requests_per_second = 5;
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            ..common::test_config(vec![openai, anthropic])
        }
    }

    fn create_test_manager_and_router() -> (Arc<ConfigManager>, Arc<VendorManager>, Router) {
        let config = create_router_test_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());
        (config_manager, vendor_manager, router)
    }

    #[tokio::test]
    async fn test_route_exact_model_match() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        let decision = router.route("gpt-4").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        assert_eq!(decision.vendor.id, "openai_primary");
        assert_eq!(decision.model.model_id, "gpt-4");
    }

    #[tokio::test]
    async fn test_route_by_abstract_id() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        // gpt-3.5 should resolve via abstract_id
        let decision = router.route("gpt-3.5").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        assert_eq!(decision.vendor.id, "openai_primary");
        assert_eq!(decision.model.abstract_id, Some("gpt-3.5".to_string()));
    }

    #[tokio::test]
    async fn test_route_anthropic_model() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        let decision = router.route("claude-3.5-sonnet").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        assert_eq!(decision.vendor.id, "anthropic_primary");
        assert_eq!(decision.model.model_id, "claude-3-5-sonnet-20241022");
    }

    #[tokio::test]
    async fn test_route_model_not_found() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        let decision = router.route("nonexistent-model").await;
        assert!(decision.is_err());
    }

    #[tokio::test]
    async fn test_route_case_insensitive() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        // Test uppercase
        let decision = router.route("GPT-4").await;
        assert!(decision.is_ok());

        // Test mixed case
        let decision = router.route("Gpt-4").await;
        assert!(decision.is_ok());
    }

    #[tokio::test]
    async fn test_route_weighted_selection() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        // For global model routing, we should get weighted selection
        // Multiple calls should potentially return different vendors based on weight
        let mut got_openai = false;

        for _ in 0..20 {
            let result = router.route("auto").await;
            if let Ok(d) = result {
                if d.vendor.id == "openai_primary" {
                    got_openai = true;
                    break;
                }
            }
        }

        // With weights configured, we should see openai
        assert!(got_openai);
    }

    #[tokio::test]
    async fn test_routing_decision_structure() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        let decision = router.route("gpt-4").await;
        assert!(decision.is_ok());
        let decision = decision.unwrap();

        // Verify decision structure
        assert!(!decision.vendor.id.is_empty());
        assert!(!decision.model.model_id.is_empty());

        // Strategy should be set
        matches!(
            decision.strategy,
            RoutingStrategy::Direct | RoutingStrategy::Abstract | RoutingStrategy::Global
        );
    }

    #[tokio::test]
    async fn test_route_disabled_vendor_excluded() {
        let config = create_router_test_config();
        let mut vendors = config.vendors.clone();
        vendors[1].enabled = false; // Disable anthropic

        let disabled_config = Config {
            rate_limit: Default::default(),
            server: config.server.clone(),
            database: config.database.clone(),
            vendors,
            global_model: config.global_model.clone(),
            pricing: config.pricing.clone(),
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
        };

        let config_manager = Arc::new(ConfigManager::new(disabled_config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        // claude-3.5-sonnet should fail because vendor is disabled
        let decision = router.route("claude-3.5-sonnet").await;
        assert!(decision.is_err());
    }

    #[tokio::test]
    async fn test_route_disabled_model_excluded() {
        let config = create_router_test_config();
        let mut vendors = config.vendors.clone();
        vendors[0].models[0].enabled = false; // Disable gpt-4

        let disabled_config = Config {
            rate_limit: Default::default(),
            server: config.server.clone(),
            database: config.database.clone(),
            vendors,
            global_model: config.global_model.clone(),
            pricing: config.pricing.clone(),
            auth: None,
            cors: None,
            retry: nexusflow::config::RetrySection::default(),
            routing: nexusflow::config::RoutingSection::default(),
            cache: nexusflow::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: nexusflow::config::ResilienceSection::default(),
        };

        let config_manager = Arc::new(ConfigManager::new(disabled_config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        // gpt-4 should fail because model is disabled
        let decision = router.route("gpt-4").await;
        assert!(decision.is_err());

        // gpt-3.5 should still work
        let decision = router.route("gpt-3.5").await;
        assert!(decision.is_ok());
    }

    #[tokio::test]
    async fn test_multiple_model_versions() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        // Test various model ID formats
        let models = vec!["gpt-4", "gpt-3.5", "claude-3.5-sonnet"];

        for model in models {
            let decision = router.route(model).await;
            assert!(decision.is_ok(), "Failed to route model: {}", model);
        }
    }

    #[tokio::test]
    async fn test_concurrent_routing_requests() {
        let (_config, _vendor, router) = create_test_manager_and_router();

        // Test concurrent routing
        let (result1, result2, result3) = tokio::join!(
            router.route("gpt-4"),
            router.route("gpt-3.5"),
            router.route("claude-3.5-sonnet")
        );

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert!(result3.is_ok());

        // Verify each got the correct vendor
        assert_eq!(result1.as_ref().unwrap().vendor.id, "openai_primary");
        assert_eq!(result2.as_ref().unwrap().vendor.id, "openai_primary");
        assert_eq!(result3.as_ref().unwrap().vendor.id, "anthropic_primary");
    }
}

// ============ recId=4/10：queue_factor 真实队列 + 权重公式升级 ============

#[cfg(test)]
mod scheduling_weight {
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, ModelConfig, RoutingSection, ServerConfig};
    use nexusflow::router::{compute_weight, Router, RoutingStrategy};
    use nexusflow::vendor::VendorManager;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::sync::Arc;
    use std::time::Duration;

    /// 两 vendor 同 abstract 模型、同 model.weight —— 差异只能来自运行时
    /// 因子（队列/窗口/EWMA）。
    fn make_two_vendor_config() -> Config {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：并发上限 1 是队列滞留用例的
        // 语义主体（慢响应占满唯一 permit，后续请求滞留 worker 队列）
        let vendor = |id: &str| {
            let mut v = common::base_vendor(id, String::new());
            v.name = format!("Vendor {id}");
            v.models = vec![ModelConfig {
                model_id: "shared-model".to_string(),
                abstract_id: Some("shared-model".to_string()),
                weight: 1.0,
                enabled: true,
            }];
            v.concurrency.requests_per_second = 100;
            v.concurrency.max_concurrent_requests = Some(1);
            v
        };
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vec![vendor("idle-vendor"), vendor("busy-vendor")])
        }
    }

    /// recId=4①：塞 pending 请求不取走 → get_queue_length>0，且真实
    /// calculate_weight 相应下降（queue_factor 生效，不再是死因子）。
    ///
    /// 塞队列机制：vendor 并发上限 1，慢响应占住 permit，后续请求滞留
    /// worker 队列不被取走。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn queue_pending_requests_raise_queue_length_and_lower_weight() {
        let mut server = mockito::Server::new_async().await;
        // 慢响应（400ms）占住并发 permit，让后续入队请求滞留
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_chunked_body(|w| {
                let _ = w.write_all(b"data: x\n\n");
                let _ = w.flush();
                std::thread::sleep(Duration::from_millis(400));
                Ok::<(), std::io::Error>(())
            })
            .expect(1)
            .create_async()
            .await;

        let mut config = make_two_vendor_config();
        // busy vendor 指向慢 mock；idle vendor 指向不可达地址（本测试不触达）
        config.vendors[1].api_base = format!("{}/v1", server.url());
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("init");
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let request =
            serde_json::from_value::<nexusflow::protocol::UnifiedRequest>(serde_json::json!({
                "model": "shared-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            }))
            .unwrap();

        // 占住 permit（慢 HTTP in-flight）
        let busy_handle = {
            let vm = vendor_manager.clone();
            let req = request.clone();
            tokio::spawn(async move { vm.execute_request("busy-vendor", req).await })
        };
        // 等第一个请求被 worker 取走、进入慢 HTTP
        tokio::time::sleep(Duration::from_millis(150)).await;

        // 再塞 3 条：worker 卡在 concurrency permit 上，pending 滞留队列
        let mut pending = Vec::new();
        for _ in 0..3 {
            let vm = vendor_manager.clone();
            let req = request.clone();
            pending.push(tokio::spawn(async move {
                vm.execute_request("busy-vendor", req).await
            }));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;

        let queue_len = vendor_manager.get_queue_length("busy-vendor");
        assert!(
            queue_len > 0,
            "滞留的 pending 必须反映在 get_queue_length（recId=4①：直读 instance.queue）"
        );
        let idle_len = vendor_manager.get_queue_length("idle-vendor");
        assert_eq!(idle_len, 0);

        // 真实权重链路：busy 侧 queue_factor 生效 → 权重严格低于 idle 侧
        let candidates = config_manager.resolve_model("shared-model");
        assert_eq!(candidates.len(), 2);
        let weighted = router.calculate_weights(candidates).await;
        let weight_of = |id: &str| {
            weighted
                .iter()
                .find(|(_, (v, _))| v.id == id)
                .map(|(w, _)| *w)
                .unwrap()
        };
        let idle_w = weight_of("idle-vendor");
        let busy_w = weight_of("busy-vendor");
        assert!(
            busy_w < idle_w,
            "队列非空必须降低权重（recId=4① queue_factor 生效）：busy={busy_w} idle={idle_w}"
        );

        // 收尾：放走全部 pending，避免任务泄漏告警
        let _ = busy_handle.await;
        for p in pending {
            let _ = p.await;
        }
    }

    /// recId=6：在途（in-flight）信号只在 load_signal 选中时生效。
    ///
    /// 机制（评审 issue#9b 采纳：VendorInstance 私有，测试走 manager 公共
    /// API）：busy vendor 并发上限 max_concurrent_requests=2，两条慢
    /// chunked 响应（mockito）占满 2 个 permit → get_in_flight==2：
    /// - load_signal="in_flight" → calculate_weights 该 vendor 权重严格
    ///   低于空闲 vendor；
    /// - load_signal="in_flight_relative" → 相对化后同样更低（两倍舰队
    ///   均值饱和触底，差距更大）；
    /// - load_signal="queue"（现状回归）→ 队列为空，同样的在途不影响
    ///   权重（历史行为锁定）。
    /// 无需 20 并发，2/2 即覆盖「信号量占用 → 权重」链路。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn in_flight_load_signal_lowers_weight_only_when_selected() {
        let mut server = mockito::Server::new_async().await;
        // 慢响应（800ms）×2 占满并发 permit，让两条请求在途悬挂
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_chunked_body(|w| {
                let _ = w.write_all(b"data: x\n\n");
                let _ = w.flush();
                std::thread::sleep(Duration::from_millis(800));
                Ok::<(), std::io::Error>(())
            })
            .expect(2)
            .create_async()
            .await;

        let mut config = make_two_vendor_config();
        // busy vendor 上限提到 2：两条并发慢请求恰好占满
        config.vendors[1].concurrency.max_concurrent_requests = Some(2);
        config.vendors[1].api_base = format!("{}/v1", server.url());

        // 热更配置面：update_config 需要路径（落盘 + DynamicField 切换）
        let dir = std::env::temp_dir().join("nexusflow-router-inflight-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, "seed").unwrap();
        let config_manager = Arc::new(ConfigManager::new(config));
        config_manager.set_path(path);
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("init");
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let set_load_signal = |signal: &str| {
            let mut next = config_manager.get_config();
            next.routing.load_signal = signal.to_string();
            config_manager.update_config(next).expect("hot update ok");
        };
        async fn weights_of(router: &Router, cm: &ConfigManager) -> (f64, f64) {
            let candidates = cm.resolve_model("shared-model");
            assert_eq!(candidates.len(), 2);
            let weighted = router.calculate_weights(candidates).await;
            let weight_of = |id: &str| {
                weighted
                    .iter()
                    .find(|(_, (v, _))| v.id == id)
                    .map(|(w, _)| *w)
                    .unwrap()
            };
            (weight_of("idle-vendor"), weight_of("busy-vendor"))
        }

        let request =
            serde_json::from_value::<nexusflow::protocol::UnifiedRequest>(serde_json::json!({
                "model": "shared-model",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8
            }))
            .unwrap();

        // 两条并发慢请求：worker 逐条取走并各占 1 个 permit
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let vm = vendor_manager.clone();
                let req = request.clone();
                tokio::spawn(async move { vm.execute_request("busy-vendor", req).await })
            })
            .collect();
        // 轮询等待两个 permit 都被持有（避免固定 sleep 竞态）
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while vendor_manager.get_in_flight("busy-vendor") < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "两条慢请求未在 2s 内占满 2 个 permit"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            vendor_manager.get_in_flight("busy-vendor"),
            2,
            "两条在途必须反映在 get_in_flight"
        );
        assert_eq!(
            vendor_manager.get_in_flight("idle-vendor"),
            0,
            "空闲侧零在途"
        );
        assert_eq!(
            vendor_manager.get_queue_length("busy-vendor"),
            0,
            "两条都被 worker 取走，队列深度为空——queue 档看不到该负载"
        );

        // in_flight 档：busy 权重严格低于 idle
        set_load_signal("in_flight");
        let (idle_w, busy_w) = weights_of(&router, &config_manager).await;
        assert!(
            busy_w < idle_w,
            "load_signal=in_flight 时在途必须降权: busy={busy_w} idle={idle_w}"
        );

        // in_flight_relative 档：busy=2×fleet 均值 → 饱和触底（factor 0），
        // idle 低于均值 → 全空闲分（factor 1）
        set_load_signal("in_flight_relative");
        let (idle_w_rel, busy_w_rel) = weights_of(&router, &config_manager).await;
        assert!(
            busy_w_rel < idle_w_rel,
            "load_signal=in_flight_relative 时相对在途必须降权: busy={busy_w_rel} idle={idle_w_rel}"
        );
        assert!(
            busy_w_rel < busy_w,
            "相对化档差距应比绝对档更大（饱和触底）: rel={busy_w_rel} abs={busy_w}"
        );

        // 现状回归：queue 档下同样的在途不影响权重（队列空 → 两侧相等）
        set_load_signal("queue");
        let (idle_w_q, busy_w_q) = weights_of(&router, &config_manager).await;
        assert_eq!(
            busy_w_q, idle_w_q,
            "load_signal=queue（默认）时在途不参与权重（历史行为）: busy={busy_w_q} idle={idle_w_q}"
        );

        // 收尾：放走两条在途，避免任务泄漏告警
        for h in handles {
            let _ = h.await;
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// recId=4②：同权重一忙一闲 → weighted_select_with 注入固定 seed
    /// StdRng，200 次选择分布偏向空闲侧且**逐位可复现**（不再依赖
    /// thread_rng）。忙闲差 = queue 0 vs 100 的 queue_factor 全幅。
    #[test]
    fn weighted_select_with_fixed_seed_prefers_idle_side_reproducibly() {
        let config = make_two_vendor_config();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let idle = config_manager.get_vendor("idle-vendor").unwrap();
        let busy = config_manager.get_vendor("busy-vendor").unwrap();
        let idle_model = idle.models[0].clone();
        let busy_model = busy.models[0].clone();

        // 同 model.weight，忙侧 queue=100 → queue_factor=0：
        // idle = 1.0×0.4 + 1.0×0.3 + 0.5×0.3 = 0.85；busy = 0.4 + 0 + 0.15 = 0.55
        let weighted = vec![
            (
                compute_weight(1.0, 0, None, &RoutingSection::default(), None),
                (idle, idle_model),
            ),
            (
                compute_weight(1.0, 100, None, &RoutingSection::default(), None),
                (busy, busy_model),
            ),
        ];

        // 单一 rng 贯穿 200 次选择（固定 seed 42 → 完整序列可复现）
        let mut rng = StdRng::seed_from_u64(42);
        let mut idle_count = 0usize;
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            if d.vendor.id == "idle-vendor" {
                idle_count += 1;
            }
        }
        assert!(
            idle_count > 120,
            "200 次选择应显著偏向空闲侧（期望 ~60.7%），实际 {idle_count}"
        );

        // 可复现性：同 seed 重放，选择序列逐位一致
        let mut rng_a = StdRng::seed_from_u64(42);
        let mut rng_b = StdRng::seed_from_u64(42);
        for _ in 0..200 {
            let a = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_a,
            );
            let b = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_b,
            );
            assert_eq!(a.vendor.id, b.vendor.id, "固定 seed 下序列必须逐位一致");
        }
    }

    // ============ recId=10 tests：权重公式升级 ============

    /// ① 系数 (1,0,0)：queue/window 因子完全不影响权重（纯函数断言）。
    #[test]
    fn routing_coefficients_one_zero_zero_ignore_queue_and_window() {
        let routing = RoutingSection {
            w_model: 1.0,
            w_queue: 0.0,
            w_window: 0.0,
            ..RoutingSection::default()
        };
        let exhausted = nexusflow::vendor::WindowInfo {
            max_tokens: Some(100),
            used_tokens: 100,
            window_end: None,
        };
        let w_full = compute_weight(1.0, 0, None, &routing, None);
        let w_queued = compute_weight(1.0, 100, None, &routing, None);
        let w_exhausted = compute_weight(1.0, 0, Some(&exhausted), &routing, None);
        assert!((w_full - 1.0).abs() < 1e-9);
        assert!(
            (w_queued - w_full).abs() < 1e-9 && (w_exhausted - w_full).abs() < 1e-9,
            "(1,0,0) 下 queue/window 因子不得影响权重: {w_queued} / {w_exhausted}"
        );
    }

    /// ③ error_factor：错误率 1.0 的 vendor 权重严格低于零错误 vendor
    /// （w_error>0 时；w_error=0 默认下两者相同 —— 默认行为不变）。
    #[test]
    fn error_factor_penalizes_failing_vendor_only_when_enabled() {
        let routing = RoutingSection {
            w_error: 0.5,
            ..RoutingSection::default()
        };
        // (ewma_latency_ms, ewma_error_rate)：延迟相同，错误率 1.0 vs 0.0
        let failing = compute_weight(1.0, 0, None, &routing, Some((100.0, 1.0)));
        let healthy = compute_weight(1.0, 0, None, &routing, Some((100.0, 0.0)));
        assert!(
            failing < healthy,
            "全错误 vendor 权重必须更低: failing={failing} healthy={healthy}"
        );
        // error_factor = 1 - 1.0 = 0 → 该维贡献 0
        assert!((failing - (0.4 + 0.3 + 0.15)).abs() < 1e-9);

        // 默认 w_error=0：错误率不影响权重（行为不变约束）
        let default_failing =
            compute_weight(1.0, 0, None, &RoutingSection::default(), Some((100.0, 1.0)));
        let default_healthy =
            compute_weight(1.0, 0, None, &RoutingSection::default(), Some((100.0, 0.0)));
        assert_eq!(default_failing, default_healthy);
    }

    /// ③ latency_factor：有界 (0,1)、ewma==ref 时 0.5、低延迟厂商得分高；
    /// 默认 w_latency=0 时 EWMA 数据不影响权重。
    #[test]
    fn latency_factor_bounds_and_neutrality() {
        let routing = RoutingSection {
            w_latency: 0.3,
            ..RoutingSection::default()
        };
        let fast = compute_weight(1.0, 0, None, &routing, Some((100.0, 0.0)));
        let at_ref = compute_weight(1.0, 0, None, &routing, Some((1000.0, 0.0)));
        let slow = compute_weight(1.0, 0, None, &routing, Some((10_000.0, 0.0)));
        // ref/(ref+ewma)：100ms→0.909、1000ms→0.5、10s→0.09
        assert!(fast > at_ref && at_ref > slow, "低延迟必须得分高");
        assert!(
            (at_ref - 0.5 * routing.w_latency - 0.85).abs() < 1e-9,
            "ewma==ref 时因子 0.5"
        );

        // 默认关闭：有/无 EWMA 数据权重一致
        let neutral_default = RoutingSection::default();
        assert_eq!(
            compute_weight(1.0, 0, None, &neutral_default, Some((5.0, 0.9))),
            compute_weight(1.0, 0, None, &neutral_default, None)
        );
    }

    /// ④ temperature 平滑：T=2 压平差距，且**全链路恰好平滑一次**。
    ///
    /// 评审修正回归（recId=10④）：此前 compute_weight 与
    /// weighted_select_with 各平滑一次，等效 w^(1/T²)。本测试经生产链路
    /// calculate_weights→weighted_select_with（T=2）断言：
    /// - compute_weight 返回未平滑的因子加权和（1.0/4.0 → 0.85/2.05）；
    /// - 权重 1:4 在 T=1 下弱侧理论占比 0.85/2.9 ≈ 29.3%，T=2 下
    ///   0.85^0.5/2.354 ≈ 39.1% —— 同一 seed 下 flat 必须显著高于 baseline
    ///   （若双次平滑则等效 T=4，弱侧 ~47.6% 与 39.1% 可区分）。
    #[tokio::test]
    async fn temperature_smoothing_applied_exactly_once_end_to_end() {
        // 两 vendor 仅 model.weight 不同（1.0 vs 4.0），queue=0、无窗口、无 EWMA
        let mut config = make_two_vendor_config();
        config.routing.temperature = 2.0;
        config.vendors[0].models[0].weight = 1.0;
        config.vendors[1].models[0].weight = 4.0;
        let flat_cm = Arc::new(ConfigManager::new(config));
        let flat_router = Router::new(
            flat_cm.clone(),
            Arc::new(VendorManager::new(flat_cm.clone())),
        );

        // compute_weight 是纯因子加权：无平滑（评审修正的静态断言）
        let routing = flat_cm.read().routing.clone();
        let raw_light = compute_weight(1.0, 0, None, &routing, None);
        let raw_heavy = compute_weight(4.0, 0, None, &routing, None);
        assert!(
            ((raw_light - 0.85).abs() < 1e-9) && ((raw_heavy - 2.05).abs() < 1e-9),
            "compute_weight 必须返回未平滑加权和: {raw_light} / {raw_heavy}"
        );

        // 生产链路权重（不含平滑——compute_weight 已回归纯因子加权）
        let candidates = flat_cm.resolve_model("shared-model");
        let weighted = flat_router.calculate_weights(candidates).await;

        // T=1 基线 vs T=2：同一权重输入、同一 seed，仅温度不同
        let mut baseline_config = make_two_vendor_config();
        baseline_config.vendors[0].models[0].weight = 1.0;
        baseline_config.vendors[1].models[0].weight = 4.0;
        let baseline_cm = Arc::new(ConfigManager::new(baseline_config));
        let baseline_router = Router::new(
            baseline_cm.clone(),
            Arc::new(VendorManager::new(baseline_cm.clone())),
        );

        let count_light = |router: &Router| {
            let mut rng = StdRng::seed_from_u64(11);
            let mut n = 0;
            for _ in 0..400 {
                let d = router.weighted_select_with(
                    weighted.clone(),
                    RoutingStrategy::Abstract,
                    &mut rng,
                );
                if d.vendor.id == "idle-vendor" {
                    n += 1;
                }
            }
            n
        };
        let baseline_light = count_light(&baseline_router);
        let flat_light = count_light(&flat_router);

        assert!(
            flat_light > baseline_light + 20,
            "T=2 必须压平分布且全链路恰好平滑一次（双次平滑等效 T=4，弱侧占比              明显偏离单次理论值 39.1%）：flat={flat_light}/400 baseline={baseline_light}/400 \
             （单次理论 156，双次理论 190+，不平滑 117）"
        );
    }

    /// ⑤ Po2C：权重 100:1 下注入固定 seed StdRng，忙侧（高 EWMA 延迟）
    /// 不再垄断选择 —— 分布显著均衡（对照 weighted 模式 ~1/101）。
    #[test]
    fn power_of_two_choices_rebalances_extreme_weights() {
        let mut config = make_two_vendor_config();
        config.routing.selection = "power_of_two".into();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        // 制造 EWMA 数据：heavy 忙（500ms）、light 闲（10ms）。
        // recId=8 迁移：延迟 EWMA 只由 e2e 道喂入（record_request 已分道），
        // 单样本即种子——Peak-EWMA 同刻重复采样 Δt=0 → α=0 是 no-op。
        vendor_manager.metrics().record_e2e(
            "busy-vendor",
            Duration::from_millis(500),
            std::time::Instant::now(),
        );
        vendor_manager.metrics().record_e2e(
            "idle-vendor",
            Duration::from_millis(10),
            std::time::Instant::now(),
        );

        let heavy = config_manager.get_vendor("busy-vendor").unwrap();
        let light = config_manager.get_vendor("idle-vendor").unwrap();
        let heavy_model = heavy.models[0].clone();
        let light_model = light.models[0].clone();
        // 权重 100:1（经温度平滑 T=1 不变）
        let weighted = vec![(100.0, (heavy, heavy_model)), (1.0, (light, light_model))];

        // Po2C：两候选时每对必含 light，light 延迟低 → light 恒胜或平局回退
        // 权重（EWMA 数据存在 → 非平局）。light 比例必须远高于 weighted 的
        // ~1/101 基线。
        let mut light_count = 0;
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            if d.vendor.id == "idle-vendor" {
                light_count += 1;
            }
        }
        assert!(
            light_count >= 120,
            "Po2C 下低延迟侧必须显著均衡（>=60%），实际 {light_count}/200（weighted 基线 ~2/200）"
        );

        // 可复现：同 seed 序列逐位一致
        let mut rng_a = StdRng::seed_from_u64(7);
        let mut rng_b = StdRng::seed_from_u64(7);
        for _ in 0..50 {
            let a = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_a,
            );
            let b = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_b,
            );
            assert_eq!(a.vendor.id, b.vendor.id);
        }
    }

    // ============ 评审 major 回归：无 e2e 数据 ≠ 0ms 假延迟 ============

    /// 纯流式 vendor（生产只有 record_request + record_ttft 落账）不得以
    /// 0.0 初始 ewma_latency_ms 冒充「延迟数据」：w_latency>0 时其
    /// latency_factor 必须走中性 0.5（而非 ref/(ref+0)=1.0 恒满分）——
    /// 混部舰队里 e2e 500ms 的 vendor 权重必须高于它（修前方向相反）。
    #[tokio::test]
    async fn latency_factor_treats_vendors_without_e2e_data_as_neutral() {
        let mut config = make_two_vendor_config();
        config.routing.w_latency = 0.4;
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let now = std::time::Instant::now();
        // busy：纯流式形态（成败记账 + TTFT 道，无任何 e2e 落账）
        vendor_manager
            .metrics()
            .record_request("busy-vendor", true, Duration::from_millis(80));
        vendor_manager
            .metrics()
            .record_ttft("busy-vendor", Duration::from_millis(50), now);
        // idle：真实 e2e 500ms
        vendor_manager
            .metrics()
            .record_e2e("idle-vendor", Duration::from_millis(500), now);

        // 对称基线（queue=0/无实例窗口=None），唯一差异是延迟道：
        // streaming-only → factor 0.5 → 0.4+0.3+0.15+0.2 = 1.05
        // e2e 500ms → factor 1000/1500 ≈ 0.667 → 0.4+0.3+0.15+0.267 ≈ 1.117
        let candidates = config_manager.resolve_model("shared-model");
        let weighted = router.calculate_weights(candidates).await;
        let weight_of = |id: &str| {
            weighted
                .iter()
                .find(|(_, (v, _))| v.id == id)
                .map(|(w, _)| *w)
                .unwrap()
        };
        let busy_w = weight_of("busy-vendor");
        let idle_w = weight_of("idle-vendor");
        assert!(
            idle_w > busy_w,
            "e2e 500ms 必须高于无 e2e 数据者（中性 0.5，非 0ms 假满分）: idle={idle_w} busy={busy_w}"
        );
        assert!(
            (busy_w - 1.05).abs() < 1e-9,
            "无 e2e 数据的延迟因子必须恰为中性 0.5: {busy_w}"
        );
    }

    /// Po2C 消费点：无 e2e 数据的 vendor latency_score 必须走保守
    /// 2×ref_ms（而非 0.0 恒胜）——纯流式 vendor 即使权重 10000:1，
    /// 两候选配对时恒败给真实 e2e 500ms 者（确定性 200/200）。
    #[test]
    fn po2c_treats_vendors_without_e2e_data_as_conservative() {
        let mut config = make_two_vendor_config();
        config.routing.selection = "power_of_two".into();
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let now = std::time::Instant::now();
        vendor_manager
            .metrics()
            .record_request("busy-vendor", true, Duration::from_millis(80));
        vendor_manager
            .metrics()
            .record_ttft("busy-vendor", Duration::from_millis(50), now);
        vendor_manager
            .metrics()
            .record_e2e("idle-vendor", Duration::from_millis(500), now);

        let busy = config_manager.get_vendor("busy-vendor").unwrap();
        let idle = config_manager.get_vendor("idle-vendor").unwrap();
        // busy 权重 10000:1——若 0.0 假延迟生效则 busy 恒胜（修前行为）
        let weighted = vec![
            (10_000.0, (busy.clone(), busy.models[0].clone())),
            (1.0, (idle.clone(), idle.models[0].clone())),
        ];

        let mut idle_count = 0;
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            if d.vendor.id == "idle-vendor" {
                idle_count += 1;
            }
        }
        assert_eq!(
            idle_count, 200,
            "无 e2e 数据侧 Po2C 得分=2×ref 保守值，两候选配对时恒败于真实 e2e 500ms"
        );
    }

    // ============ recId=8③ tests：min+buffer 候选集 ============

    /// min+buffer 候选集：buffer=0.5、候选 e2e [100ms, 120ms, 500ms] →
    /// 阈值 100×1.5=150，500ms 者不入集合（即使其权重巨大、纯轮盘赌下
    /// 必然垄断）；集合内均匀随机（fast/mid 都出现）。StdRng 断言选择域。
    #[test]
    fn min_buffer_candidate_set_excludes_latency_outlier() {
        let mut config = make_two_vendor_config();
        config.routing.latency_buffer = 0.5;
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        // 第三候选：克隆 idle 配置改名（weighted_select_with 只读
        // routing + metrics + 入参 weighted，不要求 vendor 在 config 注册）
        let mut slow = config_manager.get_vendor("idle-vendor").unwrap();
        slow.id = "slow-vendor".to_string();

        // e2e 道（recId=8：候选集判定口径 = vendor_ewma_e2e 有样本）
        let now = std::time::Instant::now();
        vendor_manager
            .metrics()
            .record_e2e("idle-vendor", Duration::from_millis(100), now);
        vendor_manager
            .metrics()
            .record_e2e("busy-vendor", Duration::from_millis(120), now);
        vendor_manager
            .metrics()
            .record_e2e("slow-vendor", Duration::from_millis(500), now);

        let idle = config_manager.get_vendor("idle-vendor").unwrap();
        let busy = config_manager.get_vendor("busy-vendor").unwrap();
        // slow 权重 10000：纯轮盘赌下 ~98% 垄断——候选集必须排除它
        let weighted = vec![
            (1.0, (idle.clone(), idle.models[0].clone())),
            (1.0, (busy.clone(), busy.models[0].clone())),
            (10_000.0, (slow.clone(), slow.models[0].clone())),
        ];

        let mut rng = StdRng::seed_from_u64(42);
        let (mut fast_count, mut mid_count, mut slow_count) = (0, 0, 0);
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            match d.vendor.id.as_str() {
                "idle-vendor" => fast_count += 1,
                "busy-vendor" => mid_count += 1,
                "slow-vendor" => slow_count += 1,
                other => panic!("意外候选 {other}"),
            }
        }
        assert_eq!(
            slow_count, 0,
            "500ms 超出 最低×1.5=150ms 阈值，不得入集合（fast=100 mid=120）"
        );
        assert!(
            fast_count > 0 && mid_count > 0,
            "集合内均匀随机：两者都必须出现 fast={fast_count} mid={mid_count}"
        );
    }

    /// buffer=0（默认）完全惰性：同样的延迟数据下候选集阶段整体跳过、
    /// rng 零消耗，纯轮盘赌按权重分布（大权重高延迟者照常垄断）——
    /// 默认路径与历史行为逐位一致的结构性回归。
    #[test]
    fn latency_buffer_zero_keeps_pure_roulette_with_latency_data() {
        let config = make_two_vendor_config(); // latency_buffer 默认 0.0
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        let mut slow = config_manager.get_vendor("idle-vendor").unwrap();
        slow.id = "slow-vendor".to_string();

        let now = std::time::Instant::now();
        vendor_manager
            .metrics()
            .record_e2e("idle-vendor", Duration::from_millis(100), now);
        vendor_manager
            .metrics()
            .record_e2e("busy-vendor", Duration::from_millis(120), now);
        vendor_manager
            .metrics()
            .record_e2e("slow-vendor", Duration::from_millis(500), now);

        let idle = config_manager.get_vendor("idle-vendor").unwrap();
        let busy = config_manager.get_vendor("busy-vendor").unwrap();
        let weighted = vec![
            (1.0, (idle.clone(), idle.models[0].clone())),
            (1.0, (busy.clone(), busy.models[0].clone())),
            (10_000.0, (slow.clone(), slow.models[0].clone())),
        ];

        let mut rng = StdRng::seed_from_u64(42);
        let mut slow_count = 0;
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            if d.vendor.id == "slow-vendor" {
                slow_count += 1;
            }
        }
        assert!(
            slow_count > 150,
            "buffer=0 时候选集必须整体跳过（大权重者按轮盘赌垄断），实际 {slow_count}/200"
        );
    }

    /// 候选不足 2 个（仅 1 个 vendor 有 e2e 数据）→ 回落轮盘赌（不单选
    /// 唯一低延迟者，保留权重语义）。
    #[test]
    fn min_buffer_falls_through_with_fewer_than_two_scored() {
        let mut config = make_two_vendor_config();
        config.routing.latency_buffer = 0.5;
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Router::new(config_manager.clone(), vendor_manager.clone());

        // 只有 idle 有 e2e 数据：scored.len()==1 < 2 → 阶段不命中
        vendor_manager.metrics().record_e2e(
            "idle-vendor",
            Duration::from_millis(100),
            std::time::Instant::now(),
        );

        let idle = config_manager.get_vendor("idle-vendor").unwrap();
        let busy = config_manager.get_vendor("busy-vendor").unwrap();
        let weighted = vec![
            (1.0, (idle.clone(), idle.models[0].clone())),
            (100.0, (busy.clone(), busy.models[0].clone())),
        ];

        // 回落轮盘赌：busy 权重 100:1，应按权重垄断（候选集若误命中会
        // 只选 idle——它有延迟数据）
        let mut rng = StdRng::seed_from_u64(42);
        let mut busy_count = 0;
        for _ in 0..200 {
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            if d.vendor.id == "busy-vendor" {
                busy_count += 1;
            }
        }
        assert!(
            busy_count > 150,
            "scored < 2 必须回落轮盘赌（权重主导），实际 busy={busy_count}/200"
        );
    }
}

// ============ recId=11：会话/前缀亲和路由（oxcache 亲和表） ============

#[cfg(test)]
mod session_affinity {
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, ModelConfig, ServerConfig};
    use nexusflow::router::affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
    use nexusflow::router::Router;
    use nexusflow::vendor::{VendorManager, VendorStatus};
    use std::sync::Arc;
    use std::time::Duration;

    /// 两 vendor 同 abstract 模型（shared-model）—— 亲和 boost 的最小场景
    fn make_two_vendor_config() -> Config {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：并发上限 1 与 shared-model
        // 单模型是亲和绑定的最小场景语义
        let vendor = |id: &str| {
            let mut v = common::base_vendor(id, String::new());
            v.name = format!("Vendor {id}");
            v.models = vec![ModelConfig {
                model_id: "shared-model".to_string(),
                abstract_id: Some("shared-model".to_string()),
                weight: 1.0,
                enabled: true,
            }];
            v.concurrency.requests_per_second = 100;
            v.concurrency.max_concurrent_requests = Some(1);
            v
        };
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            ..common::test_config(vec![vendor("vendor-a"), vendor("vendor-b")])
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

    /// ① 同一会话键连续 route：绑定后（执行成功回写的模拟）TTL 内全部
    /// 粘住同一 vendor。
    #[tokio::test]
    async fn same_session_key_sticks_to_bound_vendor() {
        let (_vm, router, table) = make_affinity_router(DEFAULT_AFFINITY_TTL);

        // 首次路由（无绑定）→ weighted_select；模拟执行成功后回写绑定
        let first = router
            .route_with_session("shared-model", Some("sess-1"))
            .await
            .expect("first route");
        table.bind(None, "sess-1", &first.vendor.id).await;

        // TTL 内连续路由必须全部粘住同一 vendor
        for i in 0..10 {
            let decision = router
                .route_with_session("shared-model", Some("sess-1"))
                .await
                .unwrap_or_else(|e| panic!("route {i} failed: {e}"));
            assert_eq!(
                decision.vendor.id, first.vendor.id,
                "亲和命中必须粘住首次绑定的 vendor（boost 语义）"
            );
        }
    }

    /// ② 短 TTL 50ms 过期后允许漂移：闲置超 TTL 的绑定过期（lookup None），
    /// 路由恢复正常 weighted_select（不报错、可再次绑定）。
    #[tokio::test]
    async fn short_ttl_expiry_allows_drift() {
        let (_vm, router, table) = make_affinity_router(Duration::from_millis(50));

        let first = router
            .route_with_session("shared-model", Some("drift"))
            .await
            .expect("first route");
        table.bind(None, "drift", &first.vendor.id).await;
        assert_eq!(
            table.lookup(None, "drift").await.as_deref(),
            Some(first.vendor.id.as_str())
        );

        // 闲置 60ms > TTL 50ms：绑定过期（漂移许可）
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            table.lookup(None, "drift").await,
            None,
            "闲置超过 TTL 的会话必须过期（允许漂移）"
        );

        // 过期后路由仍正常（weighted_select 重新决策）
        let after = router
            .route_with_session("shared-model", Some("drift"))
            .await;
        assert!(after.is_ok(), "过期后路由不得失败");
    }

    /// ③ 亲和 vendor 被排除（update_status 置 Unavailable）→ 选其他 vendor
    /// 且旧键清除——boost 不硬粘，合格集合外绝不强选。
    #[tokio::test]
    async fn excluded_affinity_vendor_falls_back_and_unbinds() {
        let (vm, router, table) = make_affinity_router(DEFAULT_AFFINITY_TTL);
        vm.initialize()
            .await
            .expect("vendor init（update_status 需要 instance）");

        // 显式绑定 vendor-a，确认亲和生效
        table.bind(None, "excl", "vendor-a").await;
        let stuck = router
            .route_with_session("shared-model", Some("excl"))
            .await
            .expect("route");
        assert_eq!(stuck.vendor.id, "vendor-a");

        let misses_before = vm.metrics().snapshot().affinity_misses;

        // vendor-a 进入排除态（公开同步 API；recId=2 已核实 limiteron CB
        // 无公开 force API，不再引用 force_circuit_open）
        vm.update_status("vendor-a", VendorStatus::Unavailable);

        let decision = router
            .route_with_session("shared-model", Some("excl"))
            .await
            .expect("排除后必须回落其他 vendor 而非失败");
        assert_ne!(
            decision.vendor.id, "vendor-a",
            "合格集合外的亲和 vendor 绝不强选"
        );
        assert_eq!(decision.vendor.id, "vendor-b");

        // 旧键清除：防止后续请求持续 miss 在失效绑定上
        assert_eq!(
            table.lookup(None, "excl").await,
            None,
            "绑定 vendor 被排除后必须清除旧键"
        );
        let misses_after = vm.metrics().snapshot().affinity_misses;
        assert!(
            misses_after > misses_before,
            "命中但 vendor 不在合格集合必须计 miss（{misses_before} → {misses_after}）"
        );
    }

    /// ④ 无 header 时同前缀 messages 哈希 → 同兜底会话键 → 同 vendor
    ///（web-server feature 下经 RoutingHints 的兜底路径）。
    #[cfg(feature = "web-server")]
    #[tokio::test]
    async fn prefix_hash_fallback_binds_same_vendor() {
        use nexusflow::api::RoutingHints;

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

    /// ⑤ hit_rate 从 0 递增：未绑定路由计 miss，命中路由计 hit。
    #[tokio::test]
    async fn affinity_hit_rate_increments() {
        let (vm, router, table) = make_affinity_router(DEFAULT_AFFINITY_TTL);

        let snap = vm.metrics().snapshot();
        assert_eq!(snap.affinity_hits, 0);
        assert_eq!(snap.affinity_misses, 0);
        assert_eq!(snap.affinity_hit_rate, 0.0, "无样本 hit_rate = 0");

        // miss：无绑定的会话键
        let _ = router
            .route_with_session("shared-model", Some("fresh"))
            .await
            .expect("route");
        let snap = vm.metrics().snapshot();
        assert_eq!(snap.affinity_misses, 1, "无绑定路由计 miss");
        assert_eq!(snap.affinity_hits, 0);
        assert_eq!(snap.affinity_hit_rate, 0.0);

        // hit：绑定后再次路由
        table.bind(None, "sticky", "vendor-b").await;
        let d = router
            .route_with_session("shared-model", Some("sticky"))
            .await
            .expect("route");
        assert_eq!(d.vendor.id, "vendor-b");
        let snap = vm.metrics().snapshot();
        assert_eq!(snap.affinity_hits, 1, "命中必须计 hit");
        assert!(
            snap.affinity_hit_rate > 0.0,
            "hit_rate 必须从 0 递增：hits={} misses={}",
            snap.affinity_hits,
            snap.affinity_misses
        );
        assert!(
            (snap.affinity_hit_rate - 0.5).abs() < 1e-9,
            "1 hit / 1 miss = 0.5"
        );

        // 不参与亲和（无会话键）不进分母
        let _ = router.route("shared-model").await.expect("plain route");
        let snap = vm.metrics().snapshot();
        assert_eq!(
            snap.affinity_hits + snap.affinity_misses,
            2,
            "无 session_key 的路由不计入"
        );
    }
}

// ============ recId=7（W5）：失败率软降权（failure_rate_gate） ============

#[cfg(test)]
mod failure_rate_gate {
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, ModelConfig, ResilienceSection, ServerConfig};
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    use std::time::Instant;

    fn make_two_vendor_config(gate: bool) -> Config {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：shared-model 单模型 +
        // "Vendor {id}" 命名 + rps 100 为本模块历史形态
        let vendor = |id: &str| {
            let mut v = common::base_vendor(id, String::new());
            v.name = format!("Vendor {id}");
            v.models = vec![ModelConfig {
                model_id: "shared-model".to_string(),
                abstract_id: Some("shared-model".to_string()),
                weight: 1.0,
                enabled: true,
            }];
            v.concurrency.requests_per_second = 100;
            v
        };
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            resilience: ResilienceSection {
                failure_rate_gate: gate,
                ..ResilienceSection::default()
            },
            ..common::test_config(vec![vendor("degraded-vendor"), vendor("healthy-vendor")])
        }
    }

    async fn weights_of(router: &Router, cm: &ConfigManager) -> (f64, f64) {
        let candidates = cm.resolve_model("shared-model");
        assert_eq!(candidates.len(), 2);
        let weighted = router.calculate_weights(candidates).await;
        let weight_of = |id: &str| {
            weighted
                .iter()
                .find(|(_, (v, _))| v.id == id)
                .map(|(w, _)| *w)
                .unwrap()
        };
        (weight_of("degraded-vendor"), weight_of("healthy-vendor"))
    }

    /// gate=true：灌 6 失败/6 总（≥ min_samples=5、fail_pct 1.0 > 0.5）→
    /// 劣化 vendor 权重恰好 ×penalty(0.1)（软降权只打折，候选不被剔除）。
    #[tokio::test]
    async fn gate_on_punishes_degraded_vendor_by_penalty_factor() {
        let config = make_two_vendor_config(true);
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm.clone(), vm.clone());

        // 纯观测窗口：直接经 metrics 注入（生产由 manager 记账点驱动）
        let t0 = Instant::now();
        for _ in 0..6 {
            vm.metrics()
                .record_vendor_outcome_at("degraded-vendor", false, t0);
        }

        let (degraded, healthy) = weights_of(&router, &cm).await;
        assert!(
            (degraded - healthy * 0.1).abs() < 1e-9,
            "gate 开启且超阈必须恰好 ×penalty(0.1): degraded={degraded} healthy={healthy}"
        );
    }

    /// gate=true：总样本 < min_samples(5) → 无惩罚（防小样本误杀）。
    #[tokio::test]
    async fn gate_on_below_min_samples_leaves_weight_untouched() {
        let config = make_two_vendor_config(true);
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm.clone(), vm.clone());

        let t0 = Instant::now();
        for _ in 0..4 {
            vm.metrics()
                .record_vendor_outcome_at("degraded-vendor", false, t0);
        }

        let (degraded, healthy) = weights_of(&router, &cm).await;
        assert_eq!(
            degraded, healthy,
            "总样本 4 < min_samples 5 不得惩罚: degraded={degraded} healthy={healthy}"
        );
    }

    /// gate=false（默认）：同样的 (6 失败/6 总) 数据 → 权重不变
    ///（默认路径回归——软降权关闭时行为与现状逐位一致）。
    #[tokio::test]
    async fn gate_off_same_data_keeps_weights_identical() {
        let config = make_two_vendor_config(false);
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm.clone(), vm.clone());

        let t0 = Instant::now();
        for _ in 0..6 {
            vm.metrics()
                .record_vendor_outcome_at("degraded-vendor", false, t0);
        }

        let (degraded, healthy) = weights_of(&router, &cm).await;
        assert_eq!(
            degraded, healthy,
            "gate=false（默认）必须忽略失败率窗口: degraded={degraded} healthy={healthy}"
        );
    }
}

// ============ recId=19：冷却与候选集的低成本判定改进包 ============

#[cfg(test)]
mod cooldown_improvements {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, RateLimitConfig, RoutingSection, ServerConfig,
        VendorConfig,
    };
    use nexusflow::error::NexusFlowError;
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn make_vendor(id: &str, weight: f64, max_tokens: u64) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：weight/max_tokens 是参数化的
        // 权重与冷却驱动值；window_hours=Some(1) 与 rps 100 为本模块历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.models = vec![ModelConfig {
            model_id: "shared-model".to_string(),
            abstract_id: Some("shared-model".to_string()),
            weight,
            enabled: true,
        }];
        v.rate_limit = RateLimitConfig::Token {
            max_tokens,
            window_hours: Some(1),
            sliding: false,
        };
        v.concurrency.requests_per_second = 100;
        v
    }

    fn make_config(routing: RoutingSection, vendors: Vec<VendorConfig>) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            routing,
            ..common::test_config(vendors)
        }
    }

    async fn make_router(config: Config) -> (Arc<Router>, Arc<VendorManager>) {
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        (Arc::new(Router::new(cm, vm.clone())), vm)
    }

    // ---- recId=19(a)：请求级权重 ----

    /// allow_request_weight 门控：开启透传 Some、关闭（默认）丢弃为 None。
    #[tokio::test]
    async fn request_weight_gate_discards_when_disabled() {
        let (router_off, _) = make_router(make_config(
            RoutingSection::default(),
            vec![make_vendor("v", 1.0, 1_000_000)],
        ))
        .await;
        assert_eq!(
            router_off.gate_request_weight(Some(3.0)),
            None,
            "门控关闭（默认）时 X-Weight 必须被忽略"
        );
        assert_eq!(router_off.gate_request_weight(None), None);

        let routing = RoutingSection {
            allow_request_weight: true,
            ..Default::default()
        };
        let (router_on, _) =
            make_router(make_config(routing, vec![make_vendor("v", 1.0, 1_000_000)])).await;
        assert_eq!(router_on.gate_request_weight(Some(3.0)), Some(3.0));
        assert_eq!(router_on.gate_request_weight(None), None);
    }

    /// (a) X-Weight 覆盖 model.weight 的数值断言：基线 w=1.0 → 0.85；
    /// 覆盖 3.0 → 0.4×3.0 + 0.3×1.0 + 0.3×0.5 = 1.65（未接线实例 →
    /// 无窗口数据 → window_factor 中性 0.5）。
    #[tokio::test]
    async fn request_weight_overrides_model_weight_in_calculate() {
        let routing = RoutingSection {
            allow_request_weight: true,
            ..Default::default()
        };
        let (router, _) =
            make_router(make_config(routing, vec![make_vendor("v", 1.0, 1_000_000)])).await;
        let vendor = make_vendor("v", 1.0, 1_000_000);
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];

        let baseline = router.calculate_weights(candidates.clone()).await;
        assert!(
            (baseline[0].0 - 0.85).abs() < 1e-9,
            "基线 0.85: {}",
            baseline[0].0
        );

        let overridden = router
            .calculate_weights_overridden(candidates, Some(3.0), None)
            .await;
        assert!(
            (overridden[0].0 - 1.65).abs() < 1e-9,
            "X-Weight=3.0 覆盖 model.weight: {}",
            overridden[0].0
        );
    }

    /// (a) 容量自动权重（第 3 级）：model.weight=0 → limit/max 归一。
    /// 100/300 断言用 delta（wb−wa = 0.4×(1−100/300)），对窗口因子取值
    /// 免疫（两 vendor 窗口同时创建、因子相同抵消）。
    #[tokio::test]
    async fn capacity_weight_normalizes_by_max_limit() {
        let (router, vm) = make_router(make_config(
            RoutingSection::default(),
            vec![make_vendor("v-a", 0.0, 100), make_vendor("v-b", 0.0, 300)],
        ))
        .await;
        vm.initialize().await.expect("vendor init");

        let a = make_vendor("v-a", 0.0, 100);
        let b = make_vendor("v-b", 0.0, 300);
        let candidates = vec![
            (a.clone(), a.models[0].clone()),
            (b.clone(), b.models[0].clone()),
        ];

        let weighted = router.calculate_weights(candidates).await;
        let wa = weighted.iter().find(|(_, (v, _))| v.id == "v-a").unwrap().0;
        let wb = weighted.iter().find(|(_, (v, _))| v.id == "v-b").unwrap().0;
        let expected_delta = 0.4 * (1.0 - 100.0 / 300.0);
        assert!(
            (wb - wa - expected_delta).abs() < 1e-6,
            "容量归一 delta: wb-wa={} expected={expected_delta}",
            wb - wa
        );
        assert!(wb > wa, "limit 大者权重高: wa={wa} wb={wb}");
    }

    /// (a) 评审修复回归：route_with_hints 的 preferred_model 分支同样必须
    /// 过 allow_request_weight 门控（此前绕过：X-Model + X-Weight 在门控
    /// 关闭时仍覆盖 model.weight 参与轮盘赌）。判别场景：model.weight
    /// 1.0/5.0 不等权 + X-Weight 3.0 —— 门控正确（忽略头）→ 权重
    /// 0.85/2.45（heavy ≈74%）；绕过（bug）→ 双双覆盖为 3.0（各 50%）。
    /// 200 次统计断言（阈值 130：正确侧 ~4.7σ，bug 侧检出率 >99.99%）。
    #[tokio::test]
    async fn hints_model_branch_respects_request_weight_gate_off() {
        let (router, _) = make_router(make_config(
            RoutingSection::default(),
            vec![
                make_vendor("v-light", 1.0, 1_000_000),
                make_vendor("v-heavy", 5.0, 1_000_000),
            ],
        ))
        .await;

        let mut heavy = 0usize;
        for _ in 0..200 {
            let d = router
                .route_with_hints(
                    "shared-model",
                    None,
                    Some("shared-model"),
                    Some(3.0),
                    None,
                    None,
                    None,
                )
                .await
                .expect("route");
            if d.vendor.id == "v-heavy" {
                heavy += 1;
            }
        }
        assert!(
            heavy > 130,
            "门控关时 X-Weight 必须被忽略（配置权重 5.0 侧应占 ~74%），实际 {heavy}/200"
        );
    }

    /// (a) 门控开：X-Weight 覆盖 model.weight → 两侧同为 3.0 → 等权
    ///（两侧都应显著出现；若被二次门控吞掉则 heavy 单边 ~74%，可检出）。
    #[tokio::test]
    async fn hints_model_branch_applies_request_weight_when_gate_on() {
        let routing = RoutingSection {
            allow_request_weight: true,
            ..Default::default()
        };
        let (router, _) = make_router(make_config(
            routing,
            vec![
                make_vendor("v-light", 1.0, 1_000_000),
                make_vendor("v-heavy", 5.0, 1_000_000),
            ],
        ))
        .await;

        let mut heavy = 0usize;
        for _ in 0..200 {
            let d = router
                .route_with_hints(
                    "shared-model",
                    None,
                    Some("shared-model"),
                    Some(3.0),
                    None,
                    None,
                    None,
                )
                .await
                .expect("route");
            if d.vendor.id == "v-heavy" {
                heavy += 1;
            }
        }
        assert!(
            heavy > 60 && heavy < 140,
            "门控开时覆盖为等权 3.0（两侧各 ~50%）: heavy={heavy}/200"
        );
    }

    /// (a) 均匀兜底（第 4 级）：weight=0 且无窗口数据（未接线实例）→
    /// 容量无解 → effective 1.0，权重与 baseline 逐位一致。
    #[tokio::test]
    async fn capacity_falls_back_uniform_without_window_data() {
        let (router, _) = make_router(make_config(
            RoutingSection::default(),
            vec![make_vendor("v", 0.0, 1_000_000)],
        ))
        .await;
        let vendor = make_vendor("v", 0.0, 1_000_000);
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];

        let weighted = router.calculate_weights(candidates).await;
        assert!(
            (weighted[0].0 - 0.85).abs() < 1e-9,
            "无容量数据 → effective 1.0（均匀兜底）: {}",
            weighted[0].0
        );
    }

    // ---- recId=19(b)：fail-open 安全网 ----

    /// (b) 两 vendor 全 Throttled（Token max_tokens=0 → check() 恒假 →
    /// get_status Throttled）：门控关（默认）→ Validation 错误（现状）；
    /// 开 → 回落忽略状态的候选集，返回其中之一。
    #[tokio::test]
    async fn fail_open_returns_candidate_when_all_cooling() {
        // 默认关：保持 Validation（router.rs 全剔除收敛错误）
        let (router_off, vm_off) = make_router(make_config(
            RoutingSection::default(),
            vec![make_vendor("v-a", 1.0, 0), make_vendor("v-b", 1.0, 0)],
        ))
        .await;
        vm_off.initialize().await.expect("vendor init");
        let err = router_off.route("shared-model").await.unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "门控关必须保持 Validation 现状: {err}"
        );

        // 开：fail-open 回落
        let routing = RoutingSection {
            fail_open_when_all_cooling: true,
            ..Default::default()
        };
        let (router_on, vm_on) = make_router(make_config(
            routing,
            vec![make_vendor("v-a", 1.0, 0), make_vendor("v-b", 1.0, 0)],
        ))
        .await;
        vm_on.initialize().await.expect("vendor init");
        let decision = router_on
            .route("shared-model")
            .await
            .expect("fail-open 决策");
        assert!(
            decision.vendor.id == "v-a" || decision.vendor.id == "v-b",
            "回落候选集必须含原冷却 vendor: {}",
            decision.vendor.id
        );
    }
}

// ============ recId=10：会话亲和软化（soft 模式）/ 键租户隔离 E2E ============

#[cfg(test)]
mod soft_affinity {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, RoutingSection, ServerConfig, VendorConfig,
    };
    use nexusflow::router::affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
    use nexusflow::router::{apply_affinity_boost, Router, RoutingStrategy};
    use nexusflow::vendor::VendorManager;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::sync::Arc;

    fn make_vendor(id: &str) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：shared-model 单模型、rps 100、
        // 并发上限 1 为本模块历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.models = vec![ModelConfig {
            model_id: "shared-model".to_string(),
            abstract_id: Some("shared-model".to_string()),
            weight: 1.0,
            enabled: true,
        }];
        v.concurrency.requests_per_second = 100;
        v.concurrency.max_concurrent_requests = Some(1);
        v
    }

    fn make_config(mode: &str, boost: f64, tenant_scoped: bool) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            routing: RoutingSection {
                affinity_mode: mode.to_string(),
                affinity_boost_factor: boost,
                affinity_tenant_scoped: tenant_scoped,
                ..Default::default()
            },
            ..common::test_config(vec![make_vendor("vendor-a"), make_vendor("vendor-b")])
        }
    }

    fn make_affinity_router(config: Config) -> (Router, Arc<AffinityTable>) {
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
        (Router::new(cm, vm).with_affinity(table.clone()), table)
    }

    /// soft 模式：boost 后 StdRng 固定种子下高 boost 必选 bound
    ///（×11 权重对等权候选，固定种子选择序列确定）。
    #[tokio::test]
    async fn soft_boost_fixed_seed_selects_bound_vendor() {
        let config = make_config("soft", 1.0, false);
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm.clone(), vm);

        let a = make_vendor("vendor-a");
        let b = make_vendor("vendor-b");
        let candidates = vec![
            (a.clone(), a.models[0].clone()),
            (b.clone(), b.models[0].clone()),
        ];

        let mut weighted = router.calculate_weights(candidates).await;
        apply_affinity_boost(&mut weighted, "vendor-a", 10.0);
        let mut rng = StdRng::seed_from_u64(42);
        let d = router.weighted_select_with(weighted, RoutingStrategy::Abstract, &mut rng);
        assert_eq!(
            d.vendor.id, "vendor-a",
            "×11 boost 下固定种子必选 bound vendor"
        );
    }

    /// boost=0：无操作（权重逐位不变）→ 同种子选择与无亲和一致
    ///（结构性回滚语义锁定）。
    #[tokio::test]
    async fn boost_zero_is_noop_identical_to_no_affinity() {
        let config = make_config("soft", 0.0, false);
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm, vm);

        let a = make_vendor("vendor-a");
        let b = make_vendor("vendor-b");
        let candidates = vec![
            (a.clone(), a.models[0].clone()),
            (b.clone(), b.models[0].clone()),
        ];

        let weights = router.calculate_weights(candidates).await;
        let mut boosted = weights.clone();
        apply_affinity_boost(&mut boosted, "vendor-a", 0.0);

        // 权重逐位一致（factor<=0 提前返回）
        for ((w0, _), (w1, _)) in weights.iter().zip(boosted.iter()) {
            assert_eq!(w0, w1, "boost=0 不得改变任何权重");
        }
        // 同种子选择一致
        let d1 = router.weighted_select_with(
            weights.clone(),
            RoutingStrategy::Abstract,
            &mut StdRng::seed_from_u64(7),
        );
        let d2 = router.weighted_select_with(
            boosted,
            RoutingStrategy::Abstract,
            &mut StdRng::seed_from_u64(7),
        );
        assert_eq!(d1.vendor.id, d2.vendor.id, "boost=0 分布与无亲和一致");
    }

    /// soft 模式 E2E：绑定 vendor-a 后 route_with_session 走 soft 评分路径，
    /// 高 factor（×10001）下实际必选 bound（弱粘性的极限形态）；全程
    /// 不经 hard 的直接返回分支。
    #[tokio::test]
    async fn soft_mode_route_applies_boost_factor() {
        let (router, table) = make_affinity_router(make_config("soft", 10_000.0, false));
        table.bind(None, "sess-soft", "vendor-a").await;

        let d = router
            .route_with_session("shared-model", Some("sess-soft"))
            .await
            .expect("soft 路由");
        assert_eq!(
            d.vendor.id, "vendor-a",
            "×10001 boost 下 soft 模式实际必选 bound"
        );
    }

    /// hard 模式回归 + 未知值回滚：affinity_mode 未知值按 "hard"（命中
    /// 即返回，现状语义）——load_signal 同款回滚阀。
    #[tokio::test]
    async fn unknown_affinity_mode_falls_back_to_hard() {
        let (router, table) = make_affinity_router(make_config("fuzzy", 0.0, false));
        table.bind(None, "sess-hard", "vendor-a").await;

        let d = router
            .route_with_session("shared-model", Some("sess-hard"))
            .await
            .expect("hard 路由");
        assert_eq!(
            d.vendor.id, "vendor-a",
            "未知 affinity_mode 按 hard：命中即返回（现状）"
        );
    }

    /// 租户隔离 E2E：tenant_scoped 开启 → 路由 lookup 用租户键——
    /// hash-a 的绑定只对 hash-a 生效；关闭（默认）→ tenant 参数被忽略、
    /// legacy 键照常命中（默认路径回归）。
    #[tokio::test]
    async fn tenant_scoped_route_isolates_and_default_ignores_tenant() {
        // 开启：scoped lookup 命中本租户绑定
        let (router_scoped, table_scoped) = make_affinity_router(make_config("hard", 1.0, true));
        table_scoped
            .bind(Some("hash-a"), "sess-t", "vendor-a")
            .await;
        let d = router_scoped
            .route_weighted(
                "shared-model",
                Some("sess-t"),
                Some("hash-a"),
                &[],
                None,
                None,
                None,
            )
            .await
            .expect("scoped 路由");
        assert_eq!(d.vendor.id, "vendor-a", "本租户绑定必须命中");

        // 关闭（默认）：tenant 参数被门控丢弃 → legacy 键照常命中
        let (router_legacy, table_legacy) = make_affinity_router(make_config("hard", 1.0, false));
        table_legacy.bind(None, "sess-t", "vendor-a").await;
        let d = router_legacy
            .route_weighted(
                "shared-model",
                Some("sess-t"),
                Some("hash-a"),
                &[],
                None,
                None,
                None,
            )
            .await
            .expect("legacy 路由");
        assert_eq!(
            d.vendor.id, "vendor-a",
            "默认关闭时 tenant 参数被忽略，legacy 键命中（现状）"
        );
    }
}

// ============ recId=14：SLA/满意度虚拟队列打分项（SLARouter 零模型版）============

#[cfg(test)]
mod sla_virtual_queue {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, PricingConfig, RoutingSection, ServerConfig,
        VendorConfig,
    };
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 基线权重：model.weight=1.0、queue=0、无窗口数据
    /// = 1.0×0.4 + 1.0×0.3 + 0.5×0.3 = 0.85
    const BASELINE: f64 = 0.85;

    fn make_vendor(id: &str) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：shared-model 单模型与
        // "Vendor {id}" 命名为本模块历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.models = vec![ModelConfig {
            model_id: "shared-model".to_string(),
            abstract_id: Some("shared-model".to_string()),
            weight: 1.0,
            enabled: true,
        }];
        v.concurrency.requests_per_second = 100;
        v
    }

    fn make_config(
        routing: RoutingSection,
        vendors: Vec<VendorConfig>,
        pricing: Vec<PricingConfig>,
    ) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            pricing,
            routing,
            ..common::test_config(vendors)
        }
    }

    async fn make_router(config: Config) -> (Arc<Router>, Arc<VendorManager>, Arc<ConfigManager>) {
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        (Arc::new(Router::new(cm.clone(), vm.clone())), vm, cm)
    }

    /// pricing 单条（(input+output)/2 的均值语义在实现内计算）
    fn price(vendor: &str, model: &str, avg: f64) -> PricingConfig {
        PricingConfig {
            vendor_id: vendor.to_string(),
            model_id: model.to_string(),
            input_per_1k_tokens: avg,
            output_per_1k_tokens: avg,
        }
    }

    /// 灌「全失败」观测：record_request 喂错误率道（首样本直接种子 1.0），
    /// record_e2e 喂 e2e 道（vendor_ewma 的 Some/None 判定口径）→ ŝ = 1−1 = 0。
    fn seed_failing_vendor(vm: &VendorManager, vendor: &str) {
        vm.metrics()
            .record_request(vendor, false, Duration::from_millis(10));
        vm.metrics()
            .record_e2e(vendor, Duration::from_millis(10), Instant::now());
    }

    fn sla_on(v: f64) -> RoutingSection {
        RoutingSection {
            sla_v: v,
            ..Default::default()
        }
    }

    async fn weight_of(
        router: &Router,
        vendor_id: &str,
        candidates: Vec<(VendorConfig, ModelConfig)>,
    ) -> f64 {
        router
            .calculate_weights(candidates)
            .await
            .into_iter()
            .find(|(_, (v, _))| v.id == vendor_id)
            .map(|(w, _)| w)
            .unwrap()
    }

    /// sla_v=0（默认）关闭等价：有定价 + 有失败 EWMA 数据（若开启必被
    /// 惩罚的输入）下权重与基线逐位相等；Q 表不产生任何键。
    #[tokio::test]
    async fn sla_off_keeps_weights_bit_identical_and_queue_untouched() {
        assert_eq!(RoutingSection::default().sla_v, 0.0, "结构性：默认关闭");
        let (router, vm, _cm) = make_router(make_config(
            RoutingSection::default(),
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        ))
        .await;
        seed_failing_vendor(&vm, "v1");

        let vendor = make_vendor("v1");
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];
        let w = weight_of(&router, "v1", candidates).await;
        assert_eq!(
            w, BASELINE,
            "sla_v=0（默认）时权重必须与基线逐位相等（Q/成本项均不参与）"
        );

        // 路由若干次（含租户键）→ Q 表不得被触碰
        router
            .route_weighted("shared-model", None, None, &[], None, None, None)
            .await
            .unwrap();
        router
            .route_weighted("shared-model", None, Some("hash-a"), &[], None, None, None)
            .await
            .unwrap();
        assert!(
            router.sla_queue_snapshot().is_empty(),
            "sla_v=0（默认）时虚拟队列 Q 不得更新"
        );
    }

    /// sla_v>0：第一轮 Q=0 仅成本惩罚（0.85/1.05）；route 一次后对偶更新
    /// Q=0.95，第二轮权重进一步被压（0.85/(1+0.05+0.95×0.95)）。
    #[tokio::test]
    async fn sla_on_penalizes_unmet_sla_and_queue_accumulates() {
        let (router, vm, _cm) = make_router(make_config(
            sla_on(1.0),
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        ))
        .await;
        seed_failing_vendor(&vm, "v1"); // ŝ=0 < α=0.95 → 欠账累积

        let vendor = make_vendor("v1");
        let candidates = || vec![(vendor.clone(), vendor.models[0].clone())];

        let w1 = weight_of(&router, "v1", candidates()).await;
        assert!(
            (w1 - BASELINE / 1.05).abs() < 1e-9,
            "第一轮 Q=0 仅成本项: {w1} vs {}",
            BASELINE / 1.05
        );

        router
            .route_weighted("shared-model", None, None, &[], None, None, None)
            .await
            .unwrap();
        let q = router.sla_queue_snapshot();
        assert!((q.get("global").copied().unwrap_or(f64::NAN) - 0.95).abs() < 1e-9);

        let w2 = weight_of(&router, "v1", candidates()).await;
        assert!(
            (w2 - BASELINE / (1.0 + 0.05 + 0.95 * 0.95)).abs() < 1e-9,
            "Q 累积后高错误率 vendor 权重被压: {w2}"
        );
        assert!(w2 < w1, "欠账累积必须单调压低权重: {w2} !< {w1}");
    }

    /// 无观测数据 vendor：ŝ=1 ≥ α → Q 更新发生但钳 0（不累积），
    /// 权重只含成本项。
    #[tokio::test]
    async fn sla_met_sla_clamps_queue_at_zero() {
        let (router, _vm, _cm) = make_router(make_config(
            sla_on(1.0),
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        ))
        .await;
        // 无任何 EWMA 数据 → ŝ=1.0（冷启动乐观）
        router
            .route_weighted("shared-model", None, None, &[], None, None, None)
            .await
            .unwrap();
        let q = router.sla_queue_snapshot();
        assert_eq!(
            q.get("global").copied(),
            Some(0.0),
            "ŝ≥α 时对偶更新必须钳 0（键存在、值为 0）"
        );

        let vendor = make_vendor("v1");
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];
        let w = weight_of(&router, "v1", candidates).await;
        assert!((w - BASELINE / 1.05).abs() < 1e-9, "Q=0 时只含成本项: {w}");
    }

    /// 缺价方向性（评审 issue#16）：缺价 vendor 的 Ĉ = 候选集已知单价中位数
    ///（保守不奖励，不得高于同质量有定价 vendor）——非记 0（那会反向奖励）。
    #[tokio::test]
    async fn sla_missing_price_uses_candidate_median() {
        let vendors = vec![
            make_vendor("cheap"),
            make_vendor("unpriced"),
            make_vendor("costly"),
        ];
        let pricing = vec![
            price("cheap", "shared-model", 0.01),
            price("costly", "shared-model", 0.09),
        ];
        let (router, _vm, _cm) = make_router(make_config(sla_on(1.0), vendors, pricing)).await;

        let candidates: Vec<_> = ["cheap", "unpriced", "costly"]
            .iter()
            .map(|id| {
                let v = make_vendor(id);
                (v.clone(), v.models[0].clone())
            })
            .collect();

        // 无 EWMA 数据 → ŝ=1 → Q 项 0；中位数(0.01,0.09)=0.05
        let w_cheap = weight_of(&router, "cheap", candidates.clone()).await;
        let w_unpriced = weight_of(&router, "unpriced", candidates.clone()).await;
        let w_costly = weight_of(&router, "costly", candidates).await;

        assert!(
            (w_unpriced - BASELINE / 1.05).abs() < 1e-9,
            "缺价 Ĉ 必须取候选集中位数 0.05: {w_unpriced}"
        );
        assert!(
            w_unpriced < w_cheap,
            "缺价 vendor 权重不得高于同质量更便宜的有价 vendor: {w_unpriced} !< {w_cheap}"
        );
        assert!(
            w_unpriced > w_costly,
            "中位数兜底应比最贵者惩罚轻: {w_unpriced} !> {w_costly}"
        );
    }

    /// 候选集全无定价：成本项整体置中性 0（退化为纯 Q 项）——无观测数据时
    /// Q 项也为 0，权重与基线逐位相等（不得出现负/放大惩罚）。
    #[tokio::test]
    async fn sla_all_unpriced_neutralizes_cost_term() {
        let (router, _vm, _cm) = make_router(make_config(
            sla_on(1.0),
            vec![make_vendor("v1")],
            vec![], // 无 [[pricing]]
        ))
        .await;
        let vendor = make_vendor("v1");
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];
        let w = weight_of(&router, "v1", candidates).await;
        assert_eq!(
            w, BASELINE,
            "全无定价时成本项置 0，ŝ=1 时 Q 项也为 0 → 与基线逐位相等"
        );
    }

    /// 虚拟队列按租户分桶：不同租户（API key 哈希）的 Q 独立累积，
    /// 无租户上下文归 "global"。
    #[tokio::test]
    async fn sla_queue_is_scoped_per_tenant() {
        let (router, vm, _cm) = make_router(make_config(
            sla_on(1.0),
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        ))
        .await;
        seed_failing_vendor(&vm, "v1"); // ŝ=0 → 每轮 Q += 0.95

        router
            .route_weighted("shared-model", None, Some("hash-a"), &[], None, None, None)
            .await
            .unwrap();
        router
            .route_weighted("shared-model", None, Some("hash-a"), &[], None, None, None)
            .await
            .unwrap();
        router
            .route_weighted("shared-model", None, Some("hash-b"), &[], None, None, None)
            .await
            .unwrap();
        router
            .route_weighted("shared-model", None, None, &[], None, None, None)
            .await
            .unwrap();

        let q = router.sla_queue_snapshot();
        assert!((q.get("hash-a").copied().unwrap_or(f64::NAN) - 1.9).abs() < 1e-9);
        assert!((q.get("hash-b").copied().unwrap_or(f64::NAN) - 0.95).abs() < 1e-9);
        assert!((q.get("global").copied().unwrap_or(f64::NAN) - 0.95).abs() < 1e-9);
    }

    /// 热更立即生效（DynamicField 既有模式）：POST /admin/config 等价的
    /// update_config 把 sla_v 0→1.0 后，同一 Router 下一轮权重立即变化。
    #[tokio::test]
    async fn sla_hot_update_takes_effect_immediately() {
        let (router, _vm, cm) = make_router(make_config(
            RoutingSection::default(), // sla_v=0
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        ))
        .await;
        let vendor = make_vendor("v1");
        let candidates = || vec![(vendor.clone(), vendor.models[0].clone())];
        assert_eq!(weight_of(&router, "v1", candidates()).await, BASELINE);

        let dir = std::env::temp_dir().join("nexusflow-sla-hot-reload");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "seed").unwrap();
        cm.set_path(path.clone());

        let mut updated = make_config(
            sla_on(1.0),
            vec![make_vendor("v1")],
            vec![price("v1", "shared-model", 0.05)],
        );
        updated.routing.sla_v = 1.0;
        cm.update_config(updated).expect("hot update ok");

        let w = weight_of(&router, "v1", candidates()).await;
        assert!(
            (w - BASELINE / 1.05).abs() < 1e-9,
            "热更 sla_v 后下一轮必须立即生效: {w}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

// ============ recId=15：LinUCB 选择档（selection 第三档）============

#[cfg(test)]
mod linucb_selection {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, RoutingSection, ServerConfig, VendorConfig,
    };
    use nexusflow::router::bandit::{linucb_features, BanditState};
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    /// 单位特征（截距 1、其余 0）——选择与奖励灌账共用。
    fn x() -> [f64; 5] {
        linucb_features(0, 0, false, false)
    }

    fn make_vendor(id: &str) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：shared-model 单模型与
        // "Vendor {id}" 命名为本模块历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.models = vec![ModelConfig {
            model_id: "shared-model".to_string(),
            abstract_id: Some("shared-model".to_string()),
            weight: 1.0,
            enabled: true,
        }];
        v.concurrency.requests_per_second = 100;
        v
    }

    fn make_config(routing: RoutingSection, vendors: Vec<VendorConfig>) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            routing,
            ..common::test_config(vendors)
        }
    }

    /// 接线形态：Router.with_bandit + 测试侧持有同一 BanditState Arc
    ///（状态可观察）。
    async fn make_wired(
        routing: RoutingSection,
        vendors: Vec<VendorConfig>,
    ) -> (Router, Arc<BanditState>, Arc<ConfigManager>) {
        let cm = Arc::new(ConfigManager::new(make_config(routing, vendors)));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let bandit = Arc::new(BanditState::new(1.0));
        let router = Router::new(cm.clone(), vm).with_bandit(bandit.clone());
        (router, bandit, cm)
    }

    /// 未接线（with_bandit 未调用）：selection="linucb" 时回退 weighted
    ///（安全语义，对齐 affinity 的 None）——路由照常成功。
    #[tokio::test]
    async fn linucb_without_wiring_falls_back_to_weighted() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("only-vendor")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm, vm); // 不接线 bandit
        let d = router
            .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
            .await
            .expect("未接线必须回退 weighted 而非失败");
        assert_eq!(d.vendor.id, "only-vendor");
    }

    /// 已接线：冷启动探索 — bad 臂多次失败后 UCB 收缩，未采样臂 argmax
    /// 胜出（20 次全选 good；UCB 差距大、无平局，与 rng 无关）。
    #[tokio::test]
    async fn linucb_wired_selects_argmax_explores_fresh_arm() {
        let (router, bandit, _cm) = make_wired(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;
        for _ in 0..5 {
            bandit.update("shared-model", "bad", &x(), 0.0);
        }
        for _ in 0..20 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
                .await
                .expect("route");
            assert_eq!(d.vendor.id, "good", "未采样臂必须被探索选中");
        }
    }

    /// 已接线：利用 — good 臂灌成功奖励后 mean 项压过未采样臂的纯探索项
    ///（ucb_good = 0.75+0.5 = 1.25 > ucb_bad = 1.0），选择从先验翻转为
    /// 定向（奖励回传驱动 UCB 变化的端到端等价断言）。
    #[tokio::test]
    async fn linucb_reward_flips_selection() {
        let (router, bandit, _cm) = make_wired(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;
        // 先验平局（两臂同 UCB）：20 次两臂都可能出现
        let mut saw_bad = false;
        for _ in 0..40 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
                .await
                .unwrap();
            saw_bad |= d.vendor.id == "bad";
        }
        assert!(saw_bad, "先验平局下探索应让两臂都获得流量");
        // 灌成功奖励 → good 的 UCB 越过 bad
        for _ in 0..3 {
            bandit.update("shared-model", "good", &x(), 1.0);
        }
        for _ in 0..20 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
                .await
                .unwrap();
            assert_eq!(d.vendor.id, "good", "成功奖励后必须定向 good");
        }
    }

    /// 默认档回归：selection="weighted"（默认）时 bandit 不被咨询——
    /// 灌同样的失败奖励，200 次路由两 vendor 都出现（若 linucb 泄漏，
    /// bad 臂 UCB 0.41 < good 1.0 会 100% 偏向 good）。
    #[tokio::test]
    async fn default_weighted_ignores_bandit_state() {
        let (router, bandit, _cm) = make_wired(
            RoutingSection::default(),
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;
        for _ in 0..5 {
            bandit.update("shared-model", "bad", &x(), 0.0);
        }
        let (mut good, mut bad) = (0usize, 0usize);
        for _ in 0..200 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
                .await
                .unwrap();
            match d.vendor.id.as_str() {
                "good" => good += 1,
                "bad" => bad += 1,
                _ => panic!("unexpected vendor"),
            }
        }
        assert!(
            good > 0 && bad > 0,
            "默认 weighted 档不受 bandit 影响: {good}/{bad}"
        );
        assert_eq!(bandit.arm_count(), 1, "Router 读侧不得创建新臂");
    }

    /// selection="linucb" 但特征缺失（None）→ 回退 weighted（行为与
    /// 未接线同款安全语义）。
    #[tokio::test]
    async fn linucb_without_features_falls_back_to_weighted() {
        let (router, bandit, _cm) = make_wired(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;
        for _ in 0..5 {
            bandit.update("shared-model", "good", &x(), 1.0);
        }
        let (mut good, mut bad) = (0usize, 0usize);
        for _ in 0..200 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, None, None)
                .await
                .unwrap();
            match d.vendor.id.as_str() {
                "good" => good += 1,
                "bad" => bad += 1,
                _ => panic!("unexpected vendor"),
            }
        }
        assert!(
            good > 0 && bad > 0,
            "特征缺失必须回退 weighted: {good}/{bad}"
        );
    }

    /// 评审修复 major：臂键对齐 — auto 路径（global 路由）选择咨询
    /// ("__global__", vendor) 臂而非 ("auto", vendor)；大写 model 归一
    /// 小写键。奖励侧（handler 经 record_linucb_reward）使用同一
    /// linucb_arm_key 归一，两端 consulted/updated 恒同键。
    #[tokio::test]
    async fn linucb_arm_key_aligned_between_selection_and_reward() {
        let (router, bandit, _cm) = make_wired(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;

        // auto 路径 → try_global_route → 臂键 "__global__"
        let d = router
            .route_weighted("auto", None, None, &[], None, Some(&x()), None)
            .await
            .expect("auto 路由");
        assert!(
            bandit.has_arm("__global__", &d.vendor.id),
            "auto 路径必须咨询 __global__ 臂"
        );
        assert!(
            !bandit.has_arm("auto", &d.vendor.id),
            "不得产生未归一的 auto 臂"
        );

        // 大写 model → 归一小写键
        let d = router
            .route_weighted("SHARED-MODEL", None, None, &[], None, Some(&x()), None)
            .await
            .expect("大写路由");
        assert!(
            bandit.has_arm("shared-model", &d.vendor.id),
            "选择键必须小写归一"
        );
        assert!(!bandit.has_arm("SHARED-MODEL", &d.vendor.id));
    }

    /// 热更立即生效：selection 从默认 weighted 热切 linucb 后，下一请求
    /// 即按 UCB argmax 选择（DynamicField 既有模式）。
    #[tokio::test]
    async fn linucb_hot_switch_takes_effect_immediately() {
        let (router, bandit, cm) = make_wired(
            RoutingSection::default(),
            vec![make_vendor("good"), make_vendor("bad")],
        )
        .await;
        for _ in 0..5 {
            bandit.update("shared-model", "bad", &x(), 0.0);
        }
        // 热切 linucb
        let dir = std::env::temp_dir().join("nexusflow-linucb-hot-reload");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "seed").unwrap();
        cm.set_path(path.clone());
        cm.update_config(make_config(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![make_vendor("good"), make_vendor("bad")],
        ))
        .expect("hot update ok");

        for _ in 0..10 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x()), None)
                .await
                .unwrap();
            assert_eq!(d.vendor.id, "good", "热切后下一请求必须按 UCB argmax");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

// ============ recId=17：LPS 排队论延迟前瞻（软约束 + deadline 硬约束）============

#[cfg(test)]
mod lps_lookahead {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, RoutingSection, ServerConfig, VendorConfig,
    };
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;
    use std::time::Instant;

    /// 基线权重（model.weight=1.0、queue=0、无窗口数据）= 0.85
    const BASELINE: f64 = 0.85;

    fn make_vendor(id: &str, max_concurrent: Option<usize>) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：max_concurrent 参数决定 Ŵ 的
        // 可区分量级（LPS 惩罚断言语义主体）；shared-model 单模型为历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.models = vec![ModelConfig {
            model_id: "shared-model".to_string(),
            abstract_id: Some("shared-model".to_string()),
            weight: 1.0,
            enabled: true,
        }];
        v.concurrency.requests_per_second = 100;
        v.concurrency.max_concurrent_requests = max_concurrent;
        v
    }

    fn make_config(routing: RoutingSection, vendors: Vec<VendorConfig>) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            routing,
            ..common::test_config(vendors)
        }
    }

    async fn make_router(
        routing: RoutingSection,
    ) -> (Arc<Router>, Arc<VendorManager>, Arc<ConfigManager>) {
        let cm = Arc::new(ConfigManager::new(make_config(
            routing,
            vec![make_vendor("busy", Some(2)), make_vendor("idle", Some(2))],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        (Arc::new(Router::new(cm.clone(), vm.clone())), vm, cm)
    }

    /// 灌观测：两 vendor 同服务率（e2e 8ms → μ=125/s），busy 灌 600 个
    /// 流量样本（α=10/s），idle 零流量 → Ŵ_busy ≈ (0.08)²/115 > 0 = Ŵ_idle。
    fn seed_observations(vm: &VendorManager) {
        for v in ["busy", "idle"] {
            vm.metrics()
                .record_e2e(v, std::time::Duration::from_millis(8), Instant::now());
        }
        for _ in 0..600 {
            vm.metrics().record_traffic_at("busy", Instant::now());
        }
    }

    async fn weight_of(router: &Router, id: &str) -> f64 {
        let vendor = make_vendor(id, Some(2));
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];
        router
            .calculate_weights(candidates)
            .await
            .into_iter()
            .next()
            .map(|(w, _)| w)
            .unwrap()
    }

    /// δ>0：逼近并发上限（高到达率）vendor 的权重显著低于空闲者
    ///（Ŵ_busy>0 → 因子 1/(1+δ·Ŵ/Ŵ_ref)<1；Ŵ_idle=0 → 因子 1）。
    #[tokio::test]
    async fn lps_penalty_scales_down_congested_vendor() {
        let (router, vm, _cm) = make_router(RoutingSection {
            lps_delta: 1.0,
            ..Default::default()
        })
        .await;
        seed_observations(&vm);

        let w_busy = weight_of(&router, "busy").await;
        let w_idle = weight_of(&router, "idle").await;
        assert!(
            w_busy < w_idle,
            "高到达率 vendor 必须被 LPS 前瞻压低: busy={w_busy} idle={w_idle}"
        );
        // 精确值（单候选集，Ŵ_ref = 中位数 = ŝ_busy 自身）：
        // Ŵ_busy = (10/125)²/115 → 因子 = 1/(1+1·1) = 1/2 → 0.425
        let w_busy_hat = 0.08_f64.powi(2) / 115.0;
        assert!(
            (w_busy - BASELINE / (1.0 + w_busy_hat / w_busy_hat)).abs() < 1e-9,
            "busy 权重 = 0.85×1/2: {w_busy}"
        );
        assert_eq!(w_idle, BASELINE, "空闲者不受惩罚");
    }

    /// δ=0（默认）关闭等价：同样的观测数据下权重与基线逐位相等，
    /// 两 vendor 一致。
    #[tokio::test]
    async fn lps_delta_zero_matches_baseline_bitwise() {
        let (router, vm, _cm) = make_router(RoutingSection::default()).await;
        seed_observations(&vm);
        assert_eq!(weight_of(&router, "busy").await, BASELINE);
        assert_eq!(weight_of(&router, "idle").await, BASELINE);
    }

    /// δ 热更立即生效：默认 0 基线 → 热改 1.0 → 同一 Router 下一轮权重
    /// 立即被压（DynamicField 既有模式）。
    #[tokio::test]
    async fn lps_delta_hot_update_takes_effect() {
        let (router, vm, cm) = make_router(RoutingSection::default()).await;
        seed_observations(&vm);
        assert_eq!(weight_of(&router, "busy").await, BASELINE);

        let dir = std::env::temp_dir().join("nexusflow-lps-hot-reload");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "seed").unwrap();
        cm.set_path(path.clone());
        cm.update_config(make_config(
            RoutingSection {
                lps_delta: 1.0,
                ..Default::default()
            },
            vec![make_vendor("busy", Some(2)), make_vendor("idle", Some(2))],
        ))
        .expect("hot update ok");

        let w = weight_of(&router, "busy").await;
        assert!(w < BASELINE, "热改 δ 后下一轮必须立即生效: {w}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// deadline 门控（硬约束）：全部候选 Ŵ 超时（头 x-deadline-ms=0.001ms）
    /// → 空集规则改选 Ŵ 最小者（idle，Ŵ=0）。gate 关时同头被忽略
    ///（正常权重路径返回，非空集回退语义可区分——此处断言 gate 关时不
    /// 触发回退即可）。
    #[tokio::test]
    async fn deadline_gate_empty_feasible_set_picks_min_wait() {
        let (router, vm, _cm) = make_router(RoutingSection {
            lps_deadline_gate: true,
            ..Default::default()
        })
        .await;
        seed_observations(&vm);

        let d = router
            .route_weighted(
                "shared-model",
                None,
                None,
                &[],
                None,
                None,
                Some(0.001), // deadline 1μs：busy(Ŵ>0) 与 idle(Ŵ=0→0ms≤0.001ms)……
            )
            .await
            .expect("route");
        // idle Ŵ=0 → 0ms ≤ 0.001ms 可行 → 非空集，走权重路径（idle 唯一可行）
        assert_eq!(d.vendor.id, "idle", "可行集应只剩 idle");

        // 更极端：把 idle 也灌上更高到达率 → 全候选超 deadline → 空集回退
        for _ in 0..1200 {
            vm.metrics().record_traffic_at("idle", Instant::now());
        }
        let d = router
            .route_weighted("shared-model", None, None, &[], None, None, Some(0.001))
            .await
            .expect("route");
        // 空集规则：改选 Ŵ 最小者（busy α=10 vs idle α=20 → busy 更小）
        assert_eq!(
            d.vendor.id, "busy",
            "全候选超时必须回退 Ŵ 最小者（JITServe 软信号语义）"
        );
    }

    /// gate=false（默认）：deadline 头被忽略——同样的极端数据下不触发
    /// 空集回退（正常返回路由决策，且不 panic）。
    #[tokio::test]
    async fn deadline_gate_off_ignores_header() {
        let (router, vm, _cm) = make_router(RoutingSection::default()).await;
        seed_observations(&vm);
        let d = router
            .route_weighted("shared-model", None, None, &[], None, None, Some(0.001))
            .await
            .expect("gate 关时头被忽略，正常路由");
        assert!(d.vendor.id == "busy" || d.vendor.id == "idle");
    }
}

// ============ 覆盖缺口补测：路由回退链 / fail-open / Po2C 平局 / 温度平滑 ============

#[cfg(test)]
mod route_gap_coverage {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, ModelConfig, PricingConfig, ResilienceSection, RoutingSection,
        ServerConfig, VendorConfig,
    };
    use nexusflow::error::NexusFlowError;
    use nexusflow::router::affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
    use nexusflow::router::bandit::{linucb_features, BanditState};
    use nexusflow::router::{compute_weight, Router, RoutingStrategy};
    use nexusflow::vendor::{VendorManager, VendorStatus};
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::sync::Arc;
    use std::time::Instant;

    const BASELINE: f64 = 0.85;

    fn vendor_with(
        id: &str,
        enabled: bool,
        models: Vec<(&str, Option<&str>, f64, bool)>,
    ) -> VendorConfig {
        common::ensure_test_api_key();
        // 与共享基座的差异字段（显式保留原值）：enabled 与模型列表是回退链
        // 剪枝用例的语义主体；rps 100 与 "Vendor {id}" 命名为本模块历史形态
        let mut v = common::base_vendor(id, String::new());
        v.name = format!("Vendor {id}");
        v.enabled = enabled;
        v.models = models
            .into_iter()
            .map(
                |(model_id, abstract_id, weight, model_enabled)| ModelConfig {
                    model_id: model_id.to_string(),
                    abstract_id: abstract_id.map(|a| a.to_string()),
                    weight,
                    enabled: model_enabled,
                },
            )
            .collect();
        v.concurrency.requests_per_second = 100;
        v
    }

    fn single_shared(id: &str) -> VendorConfig {
        vendor_with(
            id,
            true,
            vec![("shared-model", Some("shared-model"), 1.0, true)],
        )
    }

    fn make_config(routing: RoutingSection, vendors: Vec<VendorConfig>) -> Config {
        make_config_with_pricing(routing, vendors, vec![])
    }

    fn make_config_with_pricing(
        routing: RoutingSection,
        vendors: Vec<VendorConfig>,
        pricing: Vec<PricingConfig>,
    ) -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            pricing,
            routing,
            ..common::test_config(vendors)
        }
    }

    async fn make_router(
        routing: RoutingSection,
        vendors: Vec<VendorConfig>,
    ) -> (Arc<Router>, Arc<VendorManager>, Arc<ConfigManager>) {
        let cm = Arc::new(ConfigManager::new(make_config(routing, vendors)));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        (Arc::new(Router::new(cm.clone(), vm.clone())), vm, cm)
    }

    /// 单候选 vendor 的 (VendorConfig, ModelConfig) 候选对
    fn pair_of(id: &str) -> (VendorConfig, ModelConfig) {
        let v = single_shared(id);
        (v.clone(), v.models[0].clone())
    }

    // ---- auto（global 路由）回退链 ----

    /// 全部 vendor 禁用：auto 路由的 abstract/direct 都落空，global 路径
    /// 因 enabled 过滤产生空候选集且无 fail-open 回落集 → 收敛 Validation
    /// 错误（route_inner 的 is_auto 分支 fallthrough）。
    #[tokio::test]
    async fn auto_route_all_vendors_disabled_returns_validation_error() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-off",
                false,
                vec![("shared-model", Some("shared-model"), 1.0, true)],
            )],
        )
        .await;
        let err = router.route("auto").await.unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "全禁用下 auto 路由必须收敛 Validation 错误: {err}"
        );
    }

    /// 全部 vendor 运维置为 Unavailable 且 fail-open 关闭（默认）：global
    /// 路径把全部候选计入 status_dropped 后仍返回 None → Validation 错误
    ///（fail-open 不开启时不得把冷却 vendor 纳入流量）。
    #[tokio::test]
    async fn auto_route_all_unavailable_without_fail_open_errors() {
        let (router, vm, _cm) = make_router(
            RoutingSection::default(),
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )
        .await;
        vm.initialize().await.expect("vendor init");
        vm.update_status("gx-a", VendorStatus::Unavailable);
        vm.update_status("gx-b", VendorStatus::Unavailable);

        let err = router.route("auto").await.unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "全冷却且 fail-open 关闭必须报 Validation: {err}"
        );
    }

    /// global 路径的三类剪枝：禁用 vendor、被排除 vendor、禁用模型各自
    /// 被跳过，唯一健康候选胜出且 strategy == Global。
    #[tokio::test]
    async fn global_route_skips_disabled_excluded_and_disabled_models() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![
                vendor_with(
                    "gx-dis",
                    false,
                    vec![("shared-model", Some("shared-model"), 1.0, true)],
                ),
                single_shared("gx-ex"),
                vendor_with(
                    "gx-md",
                    true,
                    vec![
                        ("shared-model", Some("shared-model"), 1.0, false),
                        ("shared-model", Some("shared-model"), 1.0, true),
                    ],
                ),
                single_shared("gx-ok"),
            ],
        )
        .await;
        let d = router
            .route_with_session_excluding("auto", None, &["gx-ex".to_string()])
            .await
            .expect("route");
        // 合格集 = gx-md 的 enabled model + gx-ok；禁用 vendor 与被排除
        // vendor（gx-dis/gx-ex）绝不出现，gx-md 的 disabled model 被剪枝。
        assert_ne!(d.vendor.id, "gx-dis", "禁用 vendor 必须被剪枝");
        assert_ne!(d.vendor.id, "gx-ex", "被排除 vendor 必须被剪枝");
        assert!(d.vendor.id == "gx-md" || d.vendor.id == "gx-ok");
        assert_eq!(d.strategy, RoutingStrategy::Global);
    }

    /// global 路径 fail-open：全部候选 Unavailable 且开关开启 → 回落
    /// status_dropped 集合加权选择（有意冒险的安全网语义）。
    #[tokio::test]
    async fn global_fail_open_engages_when_all_cooling() {
        let (router, vm, _cm) = make_router(
            RoutingSection {
                fail_open_when_all_cooling: true,
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )
        .await;
        vm.initialize().await.expect("vendor init");
        vm.update_status("gx-a", VendorStatus::Unavailable);
        vm.update_status("gx-b", VendorStatus::Unavailable);

        let d = router.route("auto").await.expect("fail-open 决策");
        assert!(
            d.vendor.id == "gx-a" || d.vendor.id == "gx-b",
            "回落候选集必须来自冷却集合: {}",
            d.vendor.id
        );
        assert_eq!(d.strategy, RoutingStrategy::Global);
    }

    // ---- route_with_hints：preferred_vendor 分支 ----

    /// 指定 vendor 不存在 → router-vendor-unavailable Validation 错误
    ///（try_vendor_route 的 get_vendor None 路径 + 显式 Err 收敛）。
    #[tokio::test]
    async fn hints_unknown_vendor_returns_unavailable_error() {
        let (router, _vm, _cm) =
            make_router(RoutingSection::default(), vec![single_shared("gx-a")]).await;
        let err = router
            .route_with_hints("shared-model", Some("ghost"), None, None, None, None, None)
            .await
            .unwrap_err();
        match err {
            NexusFlowError::Validation { field } => {
                assert!(field.contains("ghost"), "错误须指名缺失 vendor: {field}");
            }
            other => panic!("必须收敛 Validation 错误: {other:?}"),
        }
    }

    /// 指定 vendor 存在但配置禁用 → 不可用错误（enabled 门控先于状态查询）。
    #[tokio::test]
    async fn hints_disabled_vendor_returns_unavailable_error() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-off",
                false,
                vec![("shared-model", Some("shared-model"), 1.0, true)],
            )],
        )
        .await;
        let err = router
            .route_with_hints("shared-model", Some("gx-off"), None, None, None, None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "禁用 vendor 必须按不可用收敛: {err:?}"
        );
    }

    /// 指定 vendor 启用但运维置 Unavailable → 不可用错误（VendorStatus
    /// 门控路径，区别于 enabled 配置门控）。
    #[tokio::test]
    async fn hints_unavailable_status_vendor_returns_unavailable_error() {
        let (router, vm, _cm) =
            make_router(RoutingSection::default(), vec![single_shared("gx-a")]).await;
        vm.initialize().await.expect("vendor init");
        vm.update_status("gx-a", VendorStatus::Unavailable);

        let err = router
            .route_with_hints("shared-model", Some("gx-a"), None, None, None, None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "Unavailable 状态 vendor 必须按不可用收敛: {err:?}"
        );
    }

    /// 指定 vendor + model 提示命中：model_id 大写（经 to_lowercase 归一）
    /// 与 abstract_id 大写（第二匹配分支）都路由到 Direct 决策。
    #[tokio::test]
    async fn hints_vendor_model_hint_routes_direct_both_match_branches() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-alias",
                true,
                vec![("m-real", Some("m-alias"), 1.0, true)],
            )],
        )
        .await;

        // (a) model_id 匹配（大写归一）
        let d = router
            .route_with_hints(
                "shared-model",
                Some("gx-alias"),
                Some("M-REAL"),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("model_id 提示路由");
        assert_eq!(d.vendor.id, "gx-alias");
        assert_eq!(d.model.model_id, "m-real");
        assert_eq!(d.strategy, RoutingStrategy::Direct);

        // (b) abstract_id 匹配（find 闭包的 or 分支）
        let d = router
            .route_with_hints(
                "shared-model",
                Some("gx-alias"),
                Some("M-ALIAS"),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("abstract_id 提示路由");
        assert_eq!(d.vendor.id, "gx-alias");
        assert_eq!(d.model.model_id, "m-real", "abstract_id 命中同一 model");
        assert_eq!(d.strategy, RoutingStrategy::Direct);
    }

    /// model 提示无匹配 → hints 落到默认 route_weighted（abstract 路由
    /// 正常决策，而非报错）。
    #[tokio::test]
    async fn hints_model_hint_miss_falls_through_to_weighted_default() {
        let (router, _vm, _cm) =
            make_router(RoutingSection::default(), vec![single_shared("gx-a")]).await;
        let d = router
            .route_with_hints(
                "shared-model",
                None,
                Some("ghost-model"),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("未命中提示必须落默认路由而非失败");
        assert_eq!(d.vendor.id, "gx-a");
        assert_eq!(d.strategy, RoutingStrategy::Abstract);
    }

    // ---- direct 形式（vendor.model）的排除与门控 ----

    /// direct 候选 vendor 在排除集中 → None → 收敛 Validation
    ///（失败迁移重路由的排除语义对 direct 形式同样生效）。
    #[tokio::test]
    async fn direct_route_excluded_vendor_errors() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-alias",
                true,
                vec![("m-real", Some("m-alias"), 1.0, true)],
            )],
        )
        .await;
        let err = router
            .route_with_session_excluding("gx-alias.m-real", None, &["gx-alias".to_string()])
            .await
            .unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "被排除的 direct vendor 必须收敛 Validation: {err:?}"
        );
    }

    /// direct 候选 vendor 配置禁用 → None → Validation（direct 路径不复用
    /// resolve_model 的 enabled 过滤，独立门控）。
    #[tokio::test]
    async fn direct_route_disabled_vendor_errors() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-off",
                false,
                vec![("m-real", Some("m-alias"), 1.0, true)],
            )],
        )
        .await;
        let err = router.route("gx-off.m-real").await.unwrap_err();
        assert!(
            matches!(err, NexusFlowError::Validation { .. }),
            "禁用 vendor 的 direct 形式必须收敛 Validation: {err:?}"
        );
    }

    /// direct 的模型段按 abstract_id 匹配（find 闭包 or 分支）→
    /// Direct 决策落到同一 model。
    #[tokio::test]
    async fn direct_route_matches_abstract_id_suffix() {
        let (router, _vm, _cm) = make_router(
            RoutingSection::default(),
            vec![vendor_with(
                "gx-alias",
                true,
                vec![("m-real", Some("m-alias"), 1.0, true)],
            )],
        )
        .await;
        let d = router.route("gx-alias.m-alias").await.expect("direct 路由");
        assert_eq!(d.vendor.id, "gx-alias");
        assert_eq!(d.model.model_id, "m-real");
        assert_eq!(d.strategy, RoutingStrategy::Direct);
    }

    // ---- LinUCB 选择档的回退与 α 钳制 ----

    /// 特征长度 != LINUCB_DIM → linucb_route 返回 None 回退 weighted
    ///（脏特征不得参与选择，也不得 panic）。
    #[tokio::test]
    async fn linucb_wrong_feature_length_falls_back_to_weighted() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection {
                selection: "linucb".into(),
                ..Default::default()
            },
            vec![single_shared("gx-good"), single_shared("gx-bad")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let bandit = Arc::new(BanditState::new(1.0));
        let router = Router::new(cm, vm).with_bandit(bandit.clone());

        let x_unit = linucb_features(0, 0, false, false);
        for _ in 0..5 {
            bandit.update("shared-model", "gx-bad", &x_unit, 0.0);
        }
        // 长度 3 的非法特征
        let bad_len = [1.0, 2.0, 3.0];
        let (mut good, mut bad) = (0usize, 0usize);
        for _ in 0..200 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&bad_len), None)
                .await
                .unwrap();
            match d.vendor.id.as_str() {
                "gx-good" => good += 1,
                "gx-bad" => bad += 1,
                other => panic!("unexpected vendor {other}"),
            }
        }
        assert!(
            good > 0 && bad > 0,
            "长度非法特征必须回退 weighted（两臂都出现）: {good}/{bad}"
        );
    }

    /// ucb_alpha 非法（负值）→ 钳 0 纯贪心：成功臂 mean 项定向、失败臂
    /// 无探索补偿 → 恒选成功臂。
    #[tokio::test]
    async fn linucb_negative_alpha_clamped_to_zero_still_routes() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection {
                selection: "linucb".into(),
                ucb_alpha: -1.0,
                ..Default::default()
            },
            vec![single_shared("gx-good"), single_shared("gx-bad")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let bandit = Arc::new(BanditState::new(1.0));
        let router = Router::new(cm, vm).with_bandit(bandit.clone());

        let x = linucb_features(0, 0, false, false);
        for _ in 0..5 {
            bandit.update("shared-model", "gx-bad", &x, 0.0);
        }
        for _ in 0..3 {
            bandit.update("shared-model", "gx-good", &x, 1.0);
        }
        for _ in 0..20 {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, Some(&x), None)
                .await
                .unwrap();
            assert_eq!(
                d.vendor.id, "gx-good",
                "α 钳 0 后纯贪心必须定向成功臂（mean 0.75 > 0）"
            );
        }
    }

    // ---- weighted_select_with 的权重退化分支（pub 确定性入口）----

    /// 全零权重：轮盘赌退化均匀随机（防除零语义），固定 seed 可复现、
    /// 多 seed 下两候选都可出现。
    #[test]
    fn zero_total_weight_selects_uniformly() {
        let (router, _vm, _cm) = {
            let cm = Arc::new(ConfigManager::new(make_config(
                RoutingSection::default(),
                vec![single_shared("gx-a"), single_shared("gx-b")],
            )));
            let vm = Arc::new(VendorManager::new(cm.clone()));
            (Router::new(cm.clone(), vm.clone()), vm, cm)
        };
        let (a, b) = (pair_of("gx-a"), pair_of("gx-b"));
        let weighted = vec![(0.0, a.clone()), (0.0, b.clone())];

        let mut rng = StdRng::seed_from_u64(42);
        let d = router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
        assert!(
            d.vendor.id == "gx-a" || d.vendor.id == "gx-b",
            "零总权重必须仍在候选内选择: {}",
            d.vendor.id
        );

        // 同 seed 复现
        let mut rng_a = StdRng::seed_from_u64(7);
        let mut rng_b = StdRng::seed_from_u64(7);
        for _ in 0..10 {
            let x = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_a,
            );
            let y = router.weighted_select_with(
                weighted.clone(),
                RoutingStrategy::Abstract,
                &mut rng_b,
            );
            assert_eq!(x.vendor.id, y.vendor.id);
        }

        // 多 seed 两候选都出现（均匀而非恒首/恒尾）
        let mut saw_a = false;
        let mut saw_b = false;
        for seed in 0..40u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            saw_a |= d.vendor.id == "gx-a";
            saw_b |= d.vendor.id == "gx-b";
        }
        assert!(saw_a && saw_b, "均匀随机下两候选都必须出现");
    }

    /// NaN 权重污染：轮盘赌 target/cumulative 恒不命中 → 兜底返回最后一个
    /// 候选（防御分支的行为锚定）。
    #[test]
    fn nan_weights_fall_back_to_last_candidate() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection::default(),
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm, vm);
        let (a, b) = (pair_of("gx-a"), pair_of("gx-b"));
        let weighted = vec![(f64::NAN, a), (2.0, b)];

        for seed in [1u64, 7, 42, 12345] {
            let mut rng = StdRng::seed_from_u64(seed);
            let d =
                router.weighted_select_with(weighted.clone(), RoutingStrategy::Abstract, &mut rng);
            assert_eq!(
                d.vendor.id, "gx-b",
                "NaN 污染下必须走兜底（最后一个候选）: seed={seed}"
            );
        }
    }

    /// 非正权重经温度平滑（T=2）不得变成 NaN：负权重保留原值 → 总和非正
    /// → 走均匀随机分支（多 seed 可选中首候选），而非 NaN 兜底恒选尾部。
    #[test]
    fn non_positive_weights_survive_temperature_smoothing() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection {
                temperature: 2.0,
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm, vm);
        let (a, b) = (pair_of("gx-a"), pair_of("gx-b"));
        // 全零与全负两组输入
        for weighted in [
            vec![(0.0, a.clone()), (0.0, b.clone())],
            vec![(-1.0, a.clone()), (-1.0, b.clone())],
        ] {
            let mut saw_a = false;
            for seed in 0..40u64 {
                let mut rng = StdRng::seed_from_u64(seed);
                let d = router.weighted_select_with(
                    weighted.clone(),
                    RoutingStrategy::Abstract,
                    &mut rng,
                );
                assert!(
                    d.vendor.id == "gx-a" || d.vendor.id == "gx-b",
                    "非正权重平滑后仍须在候选内: {}",
                    d.vendor.id
                );
                saw_a |= d.vendor.id == "gx-a";
            }
            assert!(
                saw_a,
                "温度平滑不得把非正权重变成 NaN（否则恒兜底尾部、首候选永不出现）"
            );
        }
    }

    // ---- Po2C 平局分支 ----

    /// 两候选均无延迟数据 → latency_score 平局（都是 2×ref）→ 回退权重
    /// 高者；两种权重排布下都是重者胜出（平局回退的双向锚定）。
    #[test]
    fn power_of_two_tie_prefers_heavier_weight_both_orders() {
        let cm = Arc::new(ConfigManager::new(make_config(
            RoutingSection {
                selection: "power_of_two".into(),
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm, vm);
        let (a, b) = (pair_of("gx-a"), pair_of("gx-b"));

        // 权重 2:1 与 1:2 各跑一次（同 seed，i1/i2 采样一致）
        let heavy_a = vec![(2.0, a.clone()), (1.0, b.clone())];
        let heavy_b = vec![(1.0, a.clone()), (2.0, b.clone())];
        let mut rng = StdRng::seed_from_u64(7);
        let d1 = router.weighted_select_with(heavy_a, RoutingStrategy::Abstract, &mut rng);
        let mut rng = StdRng::seed_from_u64(7);
        let d2 = router.weighted_select_with(heavy_b, RoutingStrategy::Abstract, &mut rng);

        assert_eq!(d1.vendor.id, "gx-a", "平局时权重高者（v-a）胜出");
        assert_eq!(d2.vendor.id, "gx-b", "平局时权重高者（v-b）胜出");
    }

    // ---- 失败率软降权：未超阈分支 ----

    /// gate=true、样本达标（5 ≥ min 5）但失败率 0.4 ≤ 0.5 → 不惩罚，
    /// 劣化 vendor 与健康 vendor 权重相等。
    #[tokio::test]
    async fn failure_rate_below_threshold_keeps_weight() {
        let mut config = make_config(
            RoutingSection::default(),
            vec![single_shared("gx-degraded"), single_shared("gx-healthy")],
        );
        config.resilience = ResilienceSection {
            failure_rate_gate: true,
            ..ResilienceSection::default()
        };
        let cm = Arc::new(ConfigManager::new(config));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Router::new(cm.clone(), vm.clone());

        let t0 = Instant::now();
        for _ in 0..2 {
            vm.metrics()
                .record_vendor_outcome_at("gx-degraded", false, t0);
        }
        for _ in 0..3 {
            vm.metrics()
                .record_vendor_outcome_at("gx-degraded", true, t0);
        }

        let candidates = cm.resolve_model("shared-model");
        let weighted = router.calculate_weights(candidates).await;
        let weight_of = |id: &str| {
            weighted
                .iter()
                .find(|(_, (v, _))| v.id == id)
                .map(|(w, _)| *w)
                .unwrap()
        };
        assert_eq!(
            weight_of("gx-degraded"),
            weight_of("gx-healthy"),
            "失败率未超阈不得惩罚（0.4 ≤ 0.5）"
        );
    }

    // ---- SLA：α 非有限钳 0 / global 路径全无定价 ----

    /// sla_alpha=NaN → 钳 0：Q 更新恒 0（不累积），权重只含成本项。
    #[tokio::test]
    async fn sla_nan_alpha_treated_as_zero_queue_never_grows() {
        let cm = Arc::new(ConfigManager::new(make_config_with_pricing(
            RoutingSection {
                sla_v: 1.0,
                sla_alpha: f64::NAN,
                ..Default::default()
            },
            vec![single_shared("gx-a")],
            vec![nexusflow::config::PricingConfig {
                vendor_id: "gx-a".into(),
                model_id: "shared-model".into(),
                input_per_1k_tokens: 0.05,
                output_per_1k_tokens: 0.05,
            }],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let router = Arc::new(Router::new(cm.clone(), vm.clone()));

        // 灌满失败观测（ŝ=0 < 默认 α=0.95 会累积；α 钳 0 后不得累积）
        vm.metrics()
            .record_request("gx-a", false, std::time::Duration::from_millis(10));
        vm.metrics()
            .record_e2e("gx-a", std::time::Duration::from_millis(10), Instant::now());

        router
            .route_weighted("shared-model", None, None, &[], None, None, None)
            .await
            .unwrap();
        let q = router.sla_queue_snapshot();
        assert_eq!(
            q.get("global").copied(),
            Some(0.0),
            "α 钳 0 时对偶更新必须恒 0（键存在、值 0）"
        );

        let vendor = single_shared("gx-a");
        let candidates = vec![(vendor.clone(), vendor.models[0].clone())];
        let w = router
            .calculate_weights(candidates)
            .await
            .into_iter()
            .next()
            .map(|(w, _)| w)
            .unwrap();
        assert!(
            (w - BASELINE / 1.05).abs() < 1e-9,
            "Q 恒 0 时权重只含成本项: {w}"
        );
    }

    /// global 路径（auto）+ 全候选无定价：成本项整体置中性 0（warn 一次），
    /// 路由成功且虚拟队列记账存在。
    #[tokio::test]
    async fn sla_unpriced_global_route_neutralizes_cost() {
        let (router, _vm, _cm) = make_router(
            RoutingSection {
                sla_v: 1.0,
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )
        .await;
        let d = router.route("auto").await.expect("global 路由");
        assert!(d.vendor.id == "gx-a" || d.vendor.id == "gx-b");
        // 选择完成 → 对偶更新记账（成本中性后纯 Q 项，ŝ=1 → Q 钳 0）
        let q = router.sla_queue_snapshot();
        assert_eq!(q.get("global").copied(), Some(0.0), "ŝ=1 ≥ α 时 Q 钳 0");
    }

    // ---- LPS：零 EWMA、非法 deadline、全可行、global 路径 ----

    /// e2e EWMA == 0（记录了 0ms 样本）按无服务率处理 → Ŵ=0 中性不惩罚；
    /// 无数据 vendor 同样中性 → δ>0 下两者权重都与基线逐位相等。
    #[tokio::test]
    async fn lps_zero_e2e_ewma_treated_as_no_queue_data() {
        let (router, vm, _cm) = make_router(
            RoutingSection {
                lps_delta: 1.0,
                ..Default::default()
            },
            vec![single_shared("gx-zero"), single_shared("gx-none")],
        )
        .await;
        vm.metrics().record_e2e(
            "gx-zero",
            std::time::Duration::from_millis(0),
            Instant::now(),
        );

        let w_zero = {
            let v = single_shared("gx-zero");
            router
                .calculate_weights(vec![(v.clone(), v.models[0].clone())])
                .await
                .into_iter()
                .next()
                .map(|(w, _)| w)
                .unwrap()
        };
        let w_none = {
            let v = single_shared("gx-none");
            router
                .calculate_weights(vec![(v.clone(), v.models[0].clone())])
                .await
                .into_iter()
                .next()
                .map(|(w, _)| w)
                .unwrap()
        };
        assert_eq!(w_zero, BASELINE, "0ms e2e EWMA 必须按无数据中性处理");
        assert_eq!(w_none, BASELINE, "无数据 vendor 不受 LPS 惩罚");
    }

    /// deadline 非法（NaN / 负值）→ 不设限（不得误判全超时），正常路由。
    #[tokio::test]
    async fn lps_invalid_deadline_means_no_filter() {
        let (router, _vm, _cm) = make_router(
            RoutingSection {
                lps_deadline_gate: true,
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )
        .await;
        for deadline in [f64::NAN, -1.0, 0.0] {
            let d = router
                .route_weighted("shared-model", None, None, &[], None, None, Some(deadline))
                .await
                .unwrap_or_else(|e| panic!("deadline {deadline} 不得拒绝服务: {e}"));
            assert!(d.vendor.id == "gx-a" || d.vendor.id == "gx-b");
        }
    }

    /// 全候选 Ŵ 可行 → 不筛（保持原候选与权重路径），正常路由。
    #[tokio::test]
    async fn lps_all_candidates_feasible_keeps_full_set() {
        let (router, vm, _cm) = make_router(
            RoutingSection {
                lps_deadline_gate: true,
                ..Default::default()
            },
            vec![single_shared("gx-a"), single_shared("gx-b")],
        )
        .await;
        // 有 e2e 数据但零到达率 → Ŵ=0 → 全可行（deadline 1e9ms）
        let now = Instant::now();
        vm.metrics()
            .record_e2e("gx-a", std::time::Duration::from_millis(8), now);
        vm.metrics()
            .record_e2e("gx-b", std::time::Duration::from_millis(8), now);

        let d = router
            .route_weighted("shared-model", None, None, &[], None, None, Some(1e9))
            .await
            .expect("全可行不得拒绝服务");
        assert!(d.vendor.id == "gx-a" || d.vendor.id == "gx-b");
    }

    /// global 路径 deadline 门控：部分候选超时 → 可行集筛选后只剩 idle
    ///（Feasible 分支作用于 global 候选集）。并发上限 Some(2) 使 Ŵ 落在
    /// 可区分量级（缺省 100 时 Ŵ 趋零、全候选可行，筛选不触发）。
    #[tokio::test]
    async fn global_lps_deadline_feasible_subset() {
        let vendor = |id: &str| {
            let mut v = single_shared(id);
            v.concurrency.max_concurrent_requests = Some(2);
            v
        };
        let (router, vm, _cm) = make_router(
            RoutingSection {
                lps_deadline_gate: true,
                ..Default::default()
            },
            vec![vendor("gx-busy"), vendor("gx-idle")],
        )
        .await;
        let now = Instant::now();
        for v in ["gx-busy", "gx-idle"] {
            vm.metrics()
                .record_e2e(v, std::time::Duration::from_millis(8), now);
        }
        for _ in 0..600 {
            vm.metrics().record_traffic_at("gx-busy", Instant::now());
        }

        let d = router
            .route_weighted("auto", None, None, &[], None, None, Some(0.001))
            .await
            .expect("route");
        assert_eq!(d.vendor.id, "gx-idle", "可行集应只剩 idle（global 路径）");
        assert_eq!(d.strategy, RoutingStrategy::Global);
    }

    /// global 路径全候选超时 → 空集规则改选 Ŵ 最小者（busy：α=10 < idle α=20）。
    #[tokio::test]
    async fn global_lps_deadline_empty_feasible_picks_min_wait() {
        let vendor = |id: &str| {
            let mut v = single_shared(id);
            v.concurrency.max_concurrent_requests = Some(2);
            v
        };
        let (router, vm, _cm) = make_router(
            RoutingSection {
                lps_deadline_gate: true,
                ..Default::default()
            },
            vec![vendor("gx-busy"), vendor("gx-idle")],
        )
        .await;
        let now = Instant::now();
        for v in ["gx-busy", "gx-idle"] {
            vm.metrics()
                .record_e2e(v, std::time::Duration::from_millis(8), now);
        }
        for _ in 0..600 {
            vm.metrics().record_traffic_at("gx-busy", Instant::now());
        }
        for _ in 0..1200 {
            vm.metrics().record_traffic_at("gx-idle", Instant::now());
        }

        let d = router
            .route_weighted("auto", None, None, &[], None, None, Some(0.001))
            .await
            .expect("route");
        assert_eq!(
            d.vendor.id, "gx-busy",
            "全候选超时必须回退 Ŵ 最小者（global 路径）"
        );
    }

    // ---- global 路径的会话亲和（hard 命中 / soft boost）----

    async fn make_affinity_router(
        routing: RoutingSection,
    ) -> (Arc<Router>, Arc<AffinityTable>, Arc<VendorManager>) {
        let cm = Arc::new(ConfigManager::new(make_config(
            routing,
            vec![single_shared("gx-va"), single_shared("gx-vb")],
        )));
        let vm = Arc::new(VendorManager::new(cm.clone()));
        let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
        let router = Arc::new(Router::new(cm, vm.clone()).with_affinity(table.clone()));
        (router, table, vm)
    }

    /// auto（global 路由）+ hard 亲和：绑定 vendor 在合格集合内 → 命中即
    /// 返回，strategy == Global。
    #[tokio::test]
    async fn global_affinity_hard_hit_returns_bound_vendor() {
        let (router, table, _vm) = make_affinity_router(RoutingSection::default()).await;
        table.bind(None, "g-sess", "gx-vb").await;

        let d = router
            .route_with_session("auto", Some("g-sess"))
            .await
            .expect("global 亲和路由");
        assert_eq!(d.vendor.id, "gx-vb", "hard 命中必须粘住绑定 vendor");
        assert_eq!(d.strategy, RoutingStrategy::Global);
    }

    /// auto（global 路由）+ soft 亲和：命中软化为权重乘子（×(1+factor)）
    /// 而非硬粘——boost 后绑定 vendor 占多数但另一候选仍有流量。
    #[tokio::test]
    async fn global_affinity_soft_boosts_bound_vendor_distribution() {
        let (router, table, _vm) = make_affinity_router(RoutingSection {
            affinity_mode: "soft".into(),
            affinity_boost_factor: 3.0, // 绑定 vendor 权重 1 → 4，P≈0.8
            ..Default::default()
        })
        .await;
        table.bind(None, "g-soft", "gx-vb").await;

        let (mut a, mut b) = (0usize, 0usize);
        for _ in 0..400 {
            let d = router
                .route_with_session("auto", Some("g-soft"))
                .await
                .expect("soft 亲和路由");
            assert_eq!(d.strategy, RoutingStrategy::Global);
            match d.vendor.id.as_str() {
                "gx-va" => a += 1,
                "gx-vb" => b += 1,
                other => panic!("unexpected vendor {other}"),
            }
        }
        assert!(b > a, "soft boost 后绑定 vendor 必须占多数: a={a} b={b}");
        assert!(a > 0, "soft 是 boost 不是硬粘，另一候选必须有流量: a={a}");
    }

    // ---- 纯函数：latency_factor 的非法 ref 防御 ----

    /// latency_ref_ms 配置非法（≤0）时延迟因子走中性 0.5，而非
    /// ref/(ref+ewma) 的退化值。
    #[test]
    fn latency_factor_neutral_when_ref_invalid() {
        let routing = RoutingSection {
            w_latency: 0.3,
            latency_ref_ms: 0.0,
            ..Default::default()
        };
        let w = compute_weight(1.0, 0, None, &routing, Some((100.0, 0.0)));
        // model 0.4 + queue 0.3 + window 0.15 + latency 0.5×0.3 + error 0 = 1.0
        //（若非法 ref 未走中性分支，factor=0/(0+100)=0 → 权重 0.85）
        assert!(
            (w - 1.0).abs() < 1e-9,
            "非法 ref 必须走中性 0.5（权重 1.0 而非退化的 0.85）: {w}"
        );
    }
}
