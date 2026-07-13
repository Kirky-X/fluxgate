// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, PricingConfig,
        ServerConfig, VendorConfig,
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
            pricing: vec![],
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

    #[test]
    fn test_pricing_config_deserialize_from_toml() {
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 5

[[vendors]]
id = "openai_main"
name = "OpenAI"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"
enabled = true
concurrency = { requests_per_second = 100 }
rate_limit = { type = "token", max_tokens = 1000000 }

[[vendors.models]]
model_id = "gpt-4"
enabled = true
weight = 1.0

[[pricing]]
vendor_id = "openai_main"
model_id = "gpt-4"
input_per_1k_tokens = 0.03
output_per_1k_tokens = 0.06

[[pricing]]
vendor_id = "openai_main"
model_id = "gpt-3.5-turbo"
input_per_1k_tokens = 0.001
output_per_1k_tokens = 0.002
"#;
        let config: Config = toml::from_str(toml_str).expect("TOML parse should succeed");
        assert_eq!(config.pricing.len(), 2, "should parse 2 pricing entries");

        let entry = &config.pricing[0];
        assert_eq!(entry.vendor_id, "openai_main");
        assert_eq!(entry.model_id, "gpt-4");
        assert!((entry.input_per_1k_tokens - 0.03).abs() < f64::EPSILON);
        assert!((entry.output_per_1k_tokens - 0.06).abs() < f64::EPSILON);

        let entry2 = &config.pricing[1];
        assert_eq!(entry2.model_id, "gpt-3.5-turbo");
        assert!((entry2.input_per_1k_tokens - 0.001).abs() < f64::EPSILON);
    }

    #[test]
    fn test_pricing_defaults_to_empty_when_absent() {
        // 关键属性：未配置 [[pricing]] 时 pricing 应为空 Vec（serde(default)）
        let toml_str = r#"
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 5

[[vendors]]
id = "openai_main"
name = "OpenAI"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"
enabled = true
concurrency = { requests_per_second = 100 }
rate_limit = { type = "token", max_tokens = 1000000 }

[[vendors.models]]
model_id = "gpt-4"
enabled = true
weight = 1.0
"#;
        let config: Config = toml::from_str(toml_str).expect("TOML parse should succeed");
        assert!(
            config.pricing.is_empty(),
            "missing [[pricing]] section should default to empty Vec"
        );
    }

    #[test]
    fn test_pricing_config_struct_construction() {
        // 关键属性：PricingConfig 可独立构造与序列化
        let entry = PricingConfig {
            vendor_id: "anthropic".into(),
            model_id: "claude-3-opus".into(),
            input_per_1k_tokens: 0.015,
            output_per_1k_tokens: 0.075,
        };
        assert_eq!(entry.vendor_id, "anthropic");
        assert_eq!(entry.model_id, "claude-3-opus");
        assert!((entry.input_per_1k_tokens - 0.015).abs() < f64::EPSILON);
        assert!((entry.output_per_1k_tokens - 0.075).abs() < f64::EPSILON);
    }
}
