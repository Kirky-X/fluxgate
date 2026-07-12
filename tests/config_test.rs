// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, ServerConfig,
        VendorConfig,
    };

    fn create_test_config() -> Config {
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
                    id: "openai_main".to_string(),
                    name: "OpenAI Main".to_string(),
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
                    id: "anthropic_main".to_string(),
                    name: "Anthropic Main".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.0,
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

    #[test]
    fn test_config_manager_new() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        assert!(manager.read().server.port == 8080);
    }

    #[test]
    fn test_get_vendor() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let vendor = manager.get_vendor("openai_main");
        assert!(vendor.is_some());
        let vendor = vendor.unwrap();
        assert_eq!(vendor.id, "openai_main");
        assert_eq!(vendor.type_, "openai");
    }

    #[test]
    fn test_get_vendor_not_found() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let vendor = manager.get_vendor("nonexistent");
        assert!(vendor.is_none());
    }

    #[test]
    fn test_resolve_model() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let results = manager.resolve_model("gpt-4");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, "openai_main");
        assert_eq!(results[0].1.model_id, "gpt-4");
    }

    #[test]
    fn test_resolve_model_by_abstract_id() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let results = manager.resolve_model("gpt-3.5");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.abstract_id, Some("gpt-3.5".to_string()));
    }

    #[test]
    fn test_resolve_model_not_found() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let results = manager.resolve_model("nonexistent-model");
        assert!(results.is_empty());
    }

    #[test]
    fn test_get_enabled_vendors() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let vendors = manager.get_enabled_vendors();
        assert_eq!(vendors.len(), 2);
    }

    #[test]
    fn test_is_global_model_enabled() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        assert!(manager.is_global_model_enabled());
    }

    #[test]
    fn test_get_all_model_mappings() {
        let config = create_test_config();
        let manager = ConfigManager::new(config);

        let mappings = manager.get_all_model_mappings();

        assert!(mappings.contains_key("gpt-4"));
        assert!(mappings.contains_key("gpt-3.5"));
        assert!(mappings.contains_key("claude-3.5-sonnet"));

        if let Some(gpt4_mappings) = mappings.get("gpt-4") {
            assert_eq!(gpt4_mappings.len(), 1);
            assert_eq!(
                gpt4_mappings[0],
                ("openai_main".to_string(), "gpt-4".to_string())
            );
        }
    }

    #[test]
    fn test_config_clone() {
        let config = create_test_config();
        let manager = ConfigManager::new(config.clone());

        // Clone should work since ConfigManager uses Arc
        let manager2 = manager.clone();
        assert!(manager2.read().server.port == 8080);
    }
}
