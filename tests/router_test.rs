// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
        VendorConfig,
    };
    use nexusflow::error::NexusFlowError;
    use nexusflow::router::{Router, RoutingDecision, RoutingStrategy};
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn create_router_test_config() -> Config {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
            },
            vendors: vec![
                VendorConfig {
                    id: "openai_primary".to_string(),
                    name: "OpenAI Primary".to_string(),
                    type_: "openai".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    models: vec![
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
                    ],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 10,
                    },
                },
                VendorConfig {
                    id: "anthropic_primary".to_string(),
                    name: "Anthropic Primary".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.5,
                        enabled: true,
                    }],
                    rate_limit: nexusflow::config::RateLimitConfig::Interval { hours: 1 },
                    concurrency: nexusflow::config::ConcurrencyConfig {
                        requests_per_second: 5,
                    },
                },
            ],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
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
            server: config.server.clone(),
            database: config.database.clone(),
            vendors,
            global_model: config.global_model.clone(),
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
            server: config.server.clone(),
            database: config.database.clone(),
            vendors,
            global_model: config.global_model.clone(),
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
