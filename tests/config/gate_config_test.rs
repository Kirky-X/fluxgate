// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

#[path = "../common/mod.rs"]
mod common;

/// 配置热更新地基集成测试
#[cfg(test)]
mod hot_reload {
    use crate::common;
    use fluxgate::config::{
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

    /// 建立带路径记忆的 ConfigManager + 已存在的磁盘配置文件。
    /// tempfile RAII 唯一目录：并发跑测试不碰撞，也免固定路径被预置
    fn setup(dir_name: &str, config: &Config) -> (Arc<ConfigManager>, PathBuf, tempfile::TempDir) {
        let dir = tempfile::Builder::new()
            .prefix(dir_name)
            .tempdir()
            .expect("create temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(config).unwrap()).expect("seed file");

        let manager = Arc::new(ConfigManager::new(config.clone()));
        manager.set_path(path.clone());
        (manager, path, dir)
    }

    /// ① 含重复 vendor id 的 Config 调 update_config → Err 且磁盘文件字节不变
    #[test]
    fn test_update_rejected_leaves_file_untouched() {
        let (manager, path, _dir) = setup("nx-hot-reject", &sample_config("v1", "KEY_A"));
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
        let (manager, path, _dir) = setup("nx-hot-persist", &sample_config("v1", "KEY_A"));

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

    /// ③ [resilience] 段经热更全量写回后保留且取值不丢（全量写回用
    /// toml::to_string_pretty，未进 Config 的段会从磁盘消失）。
    #[test]
    fn test_update_persists_resilience_section() {
        let mut seed = sample_config("v1", "KEY_A");
        // 预置非默认值：只有 [resilience] 进了 Config 结构体，写回才会带值
        seed.resilience.cover_all_paths = true;
        let (manager, path, _dir) = setup("nx-hot-resilience", &seed);

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
        let (manager, path, _dir) = setup("nx-hot-snapshot", &sample_config("v1", "KEY_A"));

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
        let (manager, path, _dir) = setup("nx-hot-prune", &sample_config("v1", "KEY_A"));
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
        let (manager, path, _dir) = setup("nx-hot-visible", &sample_config("v1", "KEY_A"));

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
    /// get_vendor 入口在 update 后立即返回新 api_key_env。
    #[tokio::test]
    async fn test_execute_path_reads_fresh_config_after_update() {
        let (manager, path, _dir) = setup("nx-hot-execute", &sample_config("v1", "KEY_A"));

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
    use fluxgate::config::{
        validate_config, Config, ConfigManager, DatabaseConfig, GlobalModelConfig,
        ModelCatalogEntry, ModelConfig, PricingConfig, PricingCurrency, ServerConfig, VendorConfig,
    };
    use fluxgate::error::GateError;

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
                url: String::new(),
            },
            vendors: vec![
                VendorConfig {
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    chat_path: None,
                    supports_batch: false,
                    id: "openai_main".to_string(),
                    name: "OpenAI Main".to_string(),
                    type_: "openai".to_string(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_key_envs: Vec::new(),
                    extra_headers: Default::default(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
                    models: vec![
                        ModelConfig {
                            model_id: "gpt-4".to_string(),
                            abstract_id: Some("gpt-4".to_string()),
                            weight: 1.0,
                            enabled: true,

                            dimensions: None,
                            compat_group: None,
                            model_type: None,
                        },
                        ModelConfig {
                            model_id: "gpt-3.5-turbo".to_string(),
                            abstract_id: Some("gpt-3.5".to_string()),
                            weight: 2.0,
                            enabled: true,

                            dimensions: None,
                            compat_group: None,
                            model_type: None,
                        },
                    ],
                    rate_limit: fluxgate::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: fluxgate::config::ConcurrencyConfig {
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
                    chat_path: None,
                    supports_batch: false,
                    id: "anthropic_main".to_string(),
                    name: "Anthropic Main".to_string(),
                    type_: "anthropic".to_string(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_key_envs: Vec::new(),
                    extra_headers: Default::default(),
                    api_base: "".to_string(),
                    enabled: true,
                    priority: 1,
                    models: vec![ModelConfig {
                        model_id: "claude-3-5-sonnet-20241022".to_string(),
                        abstract_id: Some("claude-3.5-sonnet".to_string()),
                        weight: 1.0,
                        enabled: true,

                        dimensions: None,
                        compat_group: None,
                        model_type: None,
                    }],
                    rate_limit: fluxgate::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: fluxgate::config::ConcurrencyConfig {
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
    fn test_database_url_defaults_to_empty_when_absent() {
        // db-backend-selection：旧配置（[database] 无 url 键）反序列化后
        // url 必须为空串（serde default），由使用方按 path 走 sqlite 回退
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
api_base = "https://api.openai.com/v1"
enabled = true
priority = 1
models = []
rate_limit = { type = "token", max_tokens = 1000000, window_hours = 24 }
concurrency = { requests_per_second = 10 }
"#;
        let config: Config = toml::from_str(toml_str).expect("TOML parse should succeed");
        assert_eq!(
            config.database.url, "",
            "missing url key should default to empty String"
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

    // ======== [[model_catalog]] 校验（validate_config） ========

    /// 登记条目 model_id 去重（与 vendor/model id 同级唯一性约束）
    #[test]
    fn catalog_rejects_duplicate_model_id() {
        let mut config = create_test_config();
        config.model_catalog = vec![
            ModelCatalogEntry {
                model_id: "claude-sonnet-4-20250514".into(),
                context_window: 200_000,
                rerank_price_per_1k_docs: 0.0,
                ..Default::default()
            },
            ModelCatalogEntry {
                model_id: "claude-sonnet-4-20250514".into(),
                context_window: 100_000,
                rerank_price_per_1k_docs: 0.0,
                ..Default::default()
            },
        ];
        let err = validate_config(&config).expect_err("duplicate catalog id must reject");
        match err {
            GateError::Config { message } => {
                assert!(
                    message.contains("claude-sonnet-4-20250514"),
                    "错误携带冲突 model_id: {message}"
                );
            }
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    /// 登记条目 context_window 必须 > 0：0 会让 agentstem TUI auto-compact
    /// 静默不装配（agent_session.rs 语义），登记面强制真实值。
    #[test]
    fn catalog_rejects_zero_context_window() {
        let mut config = create_test_config();
        config.model_catalog = vec![ModelCatalogEntry {
            model_id: "bad-model".into(),
            context_window: 0,
            rerank_price_per_1k_docs: 0.0,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("zero window must reject");
        match err {
            GateError::Config { message } => {
                assert!(
                    message.contains("bad-model"),
                    "错误携带 model_id: {message}"
                );
            }
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    /// 合法条目（去重 + 非零窗口）放行
    #[test]
    fn catalog_valid_entries_pass() {
        let mut config = create_test_config();
        config.model_catalog = vec![
            ModelCatalogEntry {
                model_id: "claude-sonnet-4-20250514".into(),
                context_window: 200_000,
                rerank_price_per_1k_docs: 0.0,
                input_price: 3.0,
                output_price: 15.0,
                cache_read_price: 0.3,
                cache_creation_price: 3.75,
                long_context_input_price: 0.0,
                long_context_threshold_tokens: 0,
                currency: PricingCurrency::Usd,
                max_output_tokens: Some(8192),
                supports_tools: true,
                supports_thinking: true,
                cache_breakpoints: None,
            },
            ModelCatalogEntry {
                model_id: "gpt-4o".into(),
                context_window: 128_000,
                rerank_price_per_1k_docs: 0.0,
                ..Default::default()
            },
        ];
        validate_config(&config).expect("valid catalog must pass");
    }

    /// 缺段兼容：model_catalog 为空时校验照常通过（旧配置逐位可用）
    #[test]
    fn catalog_empty_section_passes() {
        let config = create_test_config();
        assert!(config.model_catalog.is_empty());
        validate_config(&config).expect("empty catalog must pass");
    }

    // ======== 价格/空白 id/条目上限校验 ========

    /// 五个价格字段（含 rerank 按文档计价）必须有限（拒 NaN/Inf）且 ≥ 0
    ///（加载与热更新两路同拒）。
    #[test]
    fn catalog_rejects_non_finite_and_negative_prices() {
        let cases: Vec<(&str, f64)> = vec![
            ("input_price", f64::NAN),
            ("input_price", f64::INFINITY),
            ("input_price", -0.1),
            ("output_price", f64::NAN),
            ("cache_read_price", f64::NEG_INFINITY),
            ("cache_creation_price", -1.0),
            ("rerank_price_per_1k_docs", f64::NAN),
            ("rerank_price_per_1k_docs", -0.5),
        ];
        for (field, value) in cases {
            let mut config = create_test_config();
            config.model_catalog = vec![ModelCatalogEntry {
                model_id: "m".into(),
                context_window: 128_000,
                rerank_price_per_1k_docs: if field == "rerank_price_per_1k_docs" {
                    value
                } else {
                    0.0
                },
                input_price: if field == "input_price" { value } else { 0.0 },
                output_price: if field == "output_price" { value } else { 0.0 },
                cache_read_price: if field == "cache_read_price" {
                    value
                } else {
                    0.0
                },
                cache_creation_price: if field == "cache_creation_price" {
                    value
                } else {
                    0.0
                },
                ..Default::default()
            }];
            let err =
                validate_config(&config).expect_err(&format!("{field} = {value} must reject"));
            match err {
                GateError::Config { message } => {
                    assert!(
                        message.contains("m") && message.contains(field),
                        "错误携带 model 与字段名: {message}"
                    );
                }
                other => panic!("expected Config error, got {:?}", other),
            }
        }
        // 边界：0.0 与正有限值放行
        let mut config = create_test_config();
        config.model_catalog = vec![ModelCatalogEntry {
            model_id: "m".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 0.0,
            input_price: 0.0,
            output_price: 15.5,
            cache_read_price: 0.3,
            cache_creation_price: 0.0,
            ..Default::default()
        }];
        validate_config(&config).expect("zero and finite positive prices must pass");
    }

    /// model_id trim 后非空（全空白 id 拒绝）。
    #[test]
    fn catalog_rejects_blank_model_id() {
        let mut config = create_test_config();
        config.model_catalog = vec![ModelCatalogEntry {
            model_id: "   ".into(),
            context_window: 128_000,
            rerank_price_per_1k_docs: 0.0,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("blank id must reject");
        assert!(matches!(err, GateError::Config { .. }));
    }

    /// 目录条目上限 1024：超限拒绝、恰在限额放行。
    #[test]
    fn catalog_rejects_over_cap_and_admits_at_cap() {
        let build_entry = |i: usize| ModelCatalogEntry {
            model_id: format!("model-{i}"),
            context_window: 128_000,
            rerank_price_per_1k_docs: 0.0,
            ..Default::default()
        };
        let mut config = create_test_config();
        config.model_catalog = (0..=1024usize).map(build_entry).collect();
        let err = validate_config(&config).expect_err("1025 entries must reject");
        match err {
            GateError::Config { message } => {
                assert!(message.contains("1024"), "错误携带上限: {message}");
            }
            other => panic!("expected Config error, got {:?}", other),
        }

        let mut config = create_test_config();
        config.model_catalog = (0..1024usize).map(build_entry).collect();
        validate_config(&config).expect("exactly 1024 entries must pass");
    }
}
