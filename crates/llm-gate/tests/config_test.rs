// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod common;

/// 配置热更新地基集成测试（recId=13）
#[cfg(test)]
mod hot_reload {
    use crate::common;
    use llm_gate::config::{
        AuthSection, Config, ConfigManager, CorsSection, RateLimitConfig, ServerConfig,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    fn sample_config(vendor_id: &str, api_key_env: &str) -> Config {
        // 与共享基座的差异字段（显式保留原值）：本模块对 TOML 序列化往返做
        // 子串断言，字段值必须与历史字面量逐位一致，禁止静默改值
        let mut vendor = common::base_vendor(vendor_id, String::new());
        vendor.name = "Hot Reload Test".into();
        vendor.api_key_env = api_key_env.into();
        vendor.rate_limit = RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        };
        vendor.concurrency.requests_per_second = 10;
        Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 8080,
            },
            auth: Some(AuthSection {
                enabled: true,
                api_keys: vec!["a".repeat(64)],
                keys: vec![],
                header: Default::default(),
            }),
            cors: Some(CorsSection {
                enabled: true,
                allowed_origins: vec!["https://console.example.com".into()],
                allowed_methods: vec!["GET".into(), "POST".into()],
                allowed_headers: vec!["Authorization".into()],
                allow_credentials: false,
                max_age: 3600,
            }),
            ..common::test_config(vec![vendor])
        }
    }

    /// 建立带路径记忆的 ConfigManager + 已存在的磁盘配置文件
    fn setup(dir_name: &str, config: &Config) -> (Arc<ConfigManager>, PathBuf) {
        let dir = std::env::temp_dir().join(dir_name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(config).unwrap()).expect("seed file");

        let manager = Arc::new(ConfigManager::new(config.clone()));
        manager.set_path(path.clone());
        (manager, path)
    }

    /// ① 含重复 vendor id 的 Config 调 update_config → Err 且磁盘文件字节不变
    #[test]
    fn test_update_rejected_leaves_file_untouched() {
        let (manager, path) = setup("nx-hot-reject", &sample_config("v1", "KEY_A"));
        let before = std::fs::read(&path).unwrap();

        let mut bad = sample_config("v2", "KEY_B");
        bad.vendors.push(bad.vendors[0].clone()); // 重复 id "v2"
        let result = manager.update_config(bad);

        assert!(result.is_err(), "重复 vendor id 必须被共享校验拒绝");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            before, after,
            "被拒绝的 update 不得改动磁盘文件（回归先写后换内存的非事务行为）"
        );
        // 内存态也保持原值
        assert!(manager.get_vendor("v1").is_some());
        assert!(manager.get_vendor("v2").is_none());
    }

    /// ② 合法 update 后磁盘文件含新值且 [auth]/[cors] 段保留（回归"热更新抹段"）
    #[test]
    fn test_update_persists_and_keeps_auth_cors_sections() {
        let (manager, path) = setup("nx-hot-persist", &sample_config("v1", "KEY_A"));

        // 起点文件应含 auth 段（Config 有 auth 字段后 serde 序列化包含它）
        let seed = std::fs::read_to_string(&path).unwrap();
        assert!(seed.contains("[auth]"), "种子文件应含 [auth] 段");

        manager
            .update_config(sample_config("v2", "KEY_B"))
            .expect("valid update should succeed");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("v2"), "磁盘文件应含新 vendor id");
        assert!(on_disk.contains("KEY_B"), "磁盘文件应含新 api_key_env");
        assert!(
            on_disk.contains("[auth]"),
            "回写不得抹掉 [auth] 段（serde 未知段丢失问题的回归）"
        );
        assert!(on_disk.contains("[cors]"), "回写不得抹掉 [cors] 段");
        assert!(
            on_disk.contains("console.example.com"),
            "auth/cors 段内容应原样保留"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// ③ recId=2：[resilience] 段经热更全量写回后保留且取值不丢 ——
    /// Section 字段落地的直接验证（persist_and_swap 以 toml::to_string_pretty
    /// 全量写回，未进 Config 的段会在首次 POST /admin/config 时从磁盘消失）。
    #[test]
    fn test_update_persists_resilience_section() {
        let mut seed = sample_config("v1", "KEY_A");
        // 预置非默认值：只有 [resilience] 进了 Config 结构体，写回才会带值
        seed.resilience.cover_all_paths = true;
        let (manager, path) = setup("nx-hot-resilience", &seed);

        // 模拟 GET /admin/config → 改 vendor → POST 全量回写：客户端回传的
        // Config 携带读到的 resilience 值
        let mut payload = sample_config("v2", "KEY_B");
        payload.resilience.cover_all_paths = true;
        manager.update_config(payload).expect("valid update");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("[resilience]"),
            "回写不得抹掉 [resilience] 段: {on_disk}"
        );
        assert!(
            on_disk.contains("cover_all_paths = true"),
            "热更写回必须保留 cover_all_paths 的已设值: {on_disk}"
        );
        // 内存态与磁盘一致
        assert!(manager.read().resilience.cover_all_paths);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// ⑤ 更新前留 confers 快照：config-backups/ 出现旧值快照，且不含新值
    #[test]
    fn test_update_leaves_confers_snapshot_of_previous_config() {
        let (manager, path) = setup("nx-hot-snapshot", &sample_config("v1", "KEY_A"));

        manager
            .update_config(sample_config("v2", "KEY_B"))
            .expect("valid update should succeed");

        let backup_dir = path.parent().unwrap().join("config-backups");
        let mut snapshots: Vec<PathBuf> = std::fs::read_dir(&backup_dir)
            .expect("config-backups dir should exist after first update")
            .map(|e| e.expect("dir entry").path())
            .collect();
        assert!(!snapshots.is_empty(), "更新应留下至少一份快照");
        let content = std::fs::read_to_string(snapshots.remove(0)).unwrap();
        assert!(
            content.contains("v1"),
            "快照应是更新前的旧配置（含旧 vendor id）"
        );
        assert!(!content.contains("KEY_B"), "快照不得包含本次更新的新值");

        manager
            .update_config(sample_config("v3", "KEY_C"))
            .expect("second update should succeed");
        let count = std::fs::read_dir(&backup_dir).unwrap().count();
        assert_eq!(count, 2, "两次更新应恰好留两份快照");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// ⑥ 快照超量自动清理：11 次更新后仍只保留 max_snapshots=10 份
    #[test]
    fn test_snapshot_pruning_keeps_max() {
        let (manager, path) = setup("nx-hot-prune", &sample_config("v1", "KEY_A"));
        for i in 2..=12 {
            manager
                .update_config(sample_config(&format!("v{i}"), "KEY"))
                .expect("update should succeed");
        }
        let backup_dir = path.parent().unwrap().join("config-backups");
        let count = std::fs::read_dir(&backup_dir).unwrap().count();
        assert!(count <= 10, "快照数不得超过保留上限 10，实际 {count}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// ③ update 后 read() 立即可见（confers DynamicField 无锁热读语义）
    #[test]
    fn test_update_visible_immediately_via_read() {
        let (manager, path) = setup("nx-hot-visible", &sample_config("v1", "KEY_A"));

        manager
            .update_config(sample_config("v2", "KEY_B"))
            .expect("update ok");

        let snapshot = manager.read();
        assert_eq!(snapshot.vendors[0].id, "v2", "read() 立即反映更新");
        assert_eq!(snapshot.vendors[0].api_key_env, "KEY_B");
        // get_vendor 同一入口（worker 热读用的就是它）
        assert_eq!(manager.get_vendor("v2").unwrap().api_key_env, "KEY_B");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// ④ update 后执行路径拿到新 config：fresh_config/execute 读的
    /// get_vendor 入口在 update 后立即返回新 api_key_env
    /// （回归"改了配置没生效"）
    #[tokio::test]
    async fn test_execute_path_reads_fresh_config_after_update() {
        let (manager, path) = setup("nx-hot-execute", &sample_config("v1", "KEY_A"));

        manager
            .update_config(sample_config("v2", "KEY_B"))
            .expect("update ok");

        // vendor/manager.rs 的 worker 与三个直接执行路径的热读都走
        // ConfigManager::get_vendor —— 断言该入口已看到新配置
        let fresh = manager
            .get_vendor("v2")
            .expect("vendor should exist after update");
        assert_eq!(fresh.api_key_env, "KEY_B", "执行路径必须拿到热配置");
        assert!(manager.get_vendor("v1").is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use llm_gate::config::{
        Config, ConfigManager, DatabaseConfig, GlobalModelConfig, ModelConfig, PricingConfig,
        ServerConfig, VendorConfig,
    };

    fn create_test_config() -> Config {
        Config {
            rate_limit: Default::default(),
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
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    id: "openai_main".to_string(),
                    name: "OpenAI Main".to_string(),
                    type_: "openai".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
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
                    rate_limit: llm_gate::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: llm_gate::config::ConcurrencyConfig {
                        requests_per_second: 10,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                },
                VendorConfig {
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    id: "anthropic_main".to_string(),
                    name: "Anthropic Main".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.0,
                        enabled: true,
                    }],
                    rate_limit: llm_gate::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: llm_gate::config::ConcurrencyConfig {
                        requests_per_second: 5,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                },
            ],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".to_string(),
            }),
            pricing: vec![],
            auth: None,
            cors: None,
            retry: llm_gate::config::RetrySection::default(),
            routing: llm_gate::config::RoutingSection::default(),
            cache: llm_gate::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: llm_gate::config::ResilienceSection::default(),
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
