// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

#[path = "../common/mod.rs"]
mod common;

/// 配置热更新地基集成测试
#[cfg(test)]
mod hot_reload {
    use crate::common;
    use fluxweir::config::{
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
                oidc_rules: None,
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

    /// ③ update 后 read 立即可见（confers DynamicField 无锁热读语义）
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
    use fluxweir::config::{
        validate_config, Config, ConfigManager, DatabaseConfig, GlobalModelConfig,
        ModelCatalogEntry, ModelConfig, PricingConfig, PricingCurrency, ServerConfig, VendorConfig,
    };
    use fluxweir::error::GateError;

    fn create_test_config() -> Config {
        Config {
            masking: Default::default(),
            web_search: Default::default(),
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
                    cache_ttl_secs: 300,
                    silent_cache: false,
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    chat_path: None,
                    api_style: None,
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
                    rate_limit: fluxweir::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: fluxweir::config::ConcurrencyConfig {
                        requests_per_second: 10,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                    anthropic_version: None,
                    forward_beta_headers: false,
                    send_nonstandard_params: false,
                },
                VendorConfig {
                    cache_ttl_secs: 300,
                    silent_cache: false,
                    timeout_ms: 60_000,
                    stream_ttft_ms: 60_000,
                    stream_idle_ms: 30_000,
                    chat_path: None,
                    api_style: None,
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
                    rate_limit: fluxweir::config::RateLimitConfig::Interval {
                        hours: 1,
                        sliding: false,
                    },
                    concurrency: fluxweir::config::ConcurrencyConfig {
                        requests_per_second: 5,
                        max_concurrent_requests: None,
                        token_weighted: false,
                        token_ticket_min: 1,
                        token_ticket_max: 16384,
                        token_ticket_default: 4096,
                    },
                    anthropic_version: None,
                    forward_beta_headers: false,
                    send_nonstandard_params: false,
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
            retry: fluxweir::config::RetrySection::default(),
            routing: fluxweir::config::RoutingSection::default(),
            cache: fluxweir::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: fluxweir::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
            otlp: Default::default(),
            vendor_budgets: Vec::new(),
            key_budgets: Default::default(),
            usage_insights: Default::default(),
            catalog_sync: Default::default(),
            subscription: Default::default(),
            subscription_accounts: Vec::new(),
            metrics: Default::default(),
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

    // [vendors] D8 新键校验（validate_config）

    /// anthropic_version 格式校验：非空且 YYYY-MM-DD；缺省 None 合法
    /// （出站回退 "2023-06-01"）。
    #[test]
    fn vendor_anthropic_version_rejects_malformed_values() {
        let base = create_test_config();
        assert!(validate_config(&base).is_ok(), "缺省 None 必须合法");

        for bad in ["", "2024-1-2", "2024/10/22", "20241022", "2024-10-22x"] {
            let mut config = create_test_config();
            config.vendors[0].anthropic_version = Some(bad.to_string());
            let vendor_id = config.vendors[0].id.clone();
            let err = validate_config(&config).expect_err("非法 anthropic_version 必须拒绝");
            match err {
                GateError::Config { message } => {
                    assert!(
                        message.contains("anthropic_version"),
                        "错误必须点名字段: {message}"
                    );
                    assert!(message.contains(&vendor_id), "错误必须点名厂商: {message}");
                }
                other => panic!("expected Config error, got {:?}", other),
            }
        }

        // 合法形态：YYYY-MM-DD
        let mut config = create_test_config();
        config.vendors[0].anthropic_version = Some("2024-10-22".to_string());
        assert!(validate_config(&config).is_ok());
    }

    // [[model_catalog]] 校验（validate_config）

    /// 登记条目 model_id 去重（与 vendor/model id 同级唯一性约束）
    #[test]
    fn catalog_rejects_duplicate_model_id() {
        let mut config = create_test_config();
        config.model_catalog = vec![
            ModelCatalogEntry {
                model_id: "claude-sonnet-4-20250514".into(),
                context_window: 200_000,
                cache_creation_1h_price: 0.0,
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
            cache_creation_1h_price: 0.0,
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
                cache_creation_1h_price: 0.0,
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

    // 价格/空白 id/条目上限校验

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
                cache_creation_1h_price: 0.0,
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
            cache_creation_1h_price: 0.0,
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

    // ---- [vendors] validate_vendor_entries 拒绝臂特征化 ----
    // （codenexus-audit-remediation T006：拆分前置行为网；validate_difficulty
    // 各臂已由 loader.rs 内置 #[cfg(test)] 直测覆盖，不在此重复）

    /// 断言 validate_config 拒绝并返回 Config 错误；needles 为必须点名的插值
    /// 参数片段（沿用本文件既有风格：断言参数不依赖文案语言，i18n 注册与否
    /// 两态都稳定）。无参错误臂靠构造唯一可达来锁定分支。
    fn expect_config_err(config: &Config, needles: &[&str]) {
        match validate_config(config) {
            Err(GateError::Config { message }) => {
                for needle in needles {
                    assert!(message.contains(needle), "错误必须点名 {needle}: {message}");
                }
            }
            other => panic!("期望 Config 错误，实际 {other:?}"),
        }
    }

    /// 合法基线放行（拒绝臂的对照半边）
    #[test]
    fn vendor_entries_valid_baseline_passes() {
        assert!(validate_config(&create_test_config()).is_ok());
    }

    /// id trim 后为空拒绝（config-vendor-id-empty，无插值参数，构造唯一可达）
    #[test]
    fn vendor_rejects_blank_id() {
        let mut config = create_test_config();
        config.vendors[0].id = "   ".to_string();
        expect_config_err(&config, &[]);
    }

    /// 重复 id（ASCII 大小写折叠口径：仅大小写不同的变体也算重复——变体被拒
    /// 即折叠生效的证据）拒绝并点名厂商（消息带变体原文）
    #[test]
    fn vendor_rejects_duplicate_id() {
        let mut config = create_test_config();
        let mut dup = config.vendors[0].clone();
        dup.id = "OpenAI_MAIN".to_string();
        config.vendors.push(dup);
        expect_config_err(&config, &["OpenAI_MAIN"]);
    }

    /// 超长 id（>53 字符）拒绝并点名上限
    #[test]
    fn vendor_rejects_overlong_id() {
        let mut config = create_test_config();
        config.vendors[0].id = "x".repeat(54);
        expect_config_err(&config, &["53"]);
    }

    /// rerank 型无官方默认端点：api_base 空白拒绝并点名厂商
    #[test]
    fn vendor_rejects_rerank_without_api_base() {
        let mut config = create_test_config();
        config.vendors[0].type_ = "rerank".to_string();
        expect_config_err(&config, &["openai_main"]);
    }

    /// gemini 型端点由 model+action 动态拼接：chat_path 静态覆盖即冲突，拒绝并点名厂商
    #[test]
    fn vendor_rejects_gemini_chat_path_override() {
        let mut config = create_test_config();
        config.vendors[0].type_ = "gemini".to_string();
        config.vendors[0].chat_path = Some("/v1beta".to_string());
        expect_config_err(&config, &["openai_main"]);
    }

    /// api_style 白名单外值拒绝并点名厂商与原值
    #[test]
    fn vendor_rejects_unknown_api_style() {
        let mut config = create_test_config();
        config.vendors[0].api_style = Some("bogus-style".to_string());
        expect_config_err(&config, &["openai_main", "bogus-style"]);
    }

    /// responses 风格仅限 openai 型：anthropic 型声明即拒绝
    #[test]
    fn vendor_rejects_responses_style_on_non_openai() {
        let mut config = create_test_config();
        config.vendors[1].api_style = Some("responses".to_string());
        expect_config_err(&config, &["anthropic_main"]);
    }

    /// responses 风格与 chat_path 互斥：openai 型带 chat_path 拒绝
    #[test]
    fn vendor_rejects_responses_style_with_chat_path() {
        let mut config = create_test_config();
        config.vendors[0].api_style = Some("responses".to_string());
        config.vendors[0].chat_path = Some("/v1/chat/completions".to_string());
        expect_config_err(&config, &["openai_main"]);
    }

    /// 模型 model_id 空白拒绝并点名厂商
    #[test]
    fn vendor_rejects_blank_model_id() {
        let mut config = create_test_config();
        config.vendors[0].models[0].model_id = "  ".to_string();
        expect_config_err(&config, &["openai_main"]);
    }

    /// abstract_id 声明但空白拒绝并点名厂商与模型
    #[test]
    fn vendor_rejects_blank_abstract_id() {
        let mut config = create_test_config();
        config.vendors[0].models[0].abstract_id = Some("   ".to_string());
        expect_config_err(&config, &["openai_main", "gpt-4"]);
    }

    /// 同厂商内 model_id 重复（ASCII 折叠口径）拒绝并点名厂商与模型
    #[test]
    fn vendor_rejects_duplicate_model_id() {
        let mut config = create_test_config();
        let mut dup = config.vendors[0].models[0].clone();
        dup.model_id = "GPT-4".to_string();
        config.vendors[0].models.push(dup);
        expect_config_err(&config, &["openai_main", "GPT-4"]);
    }
}

/// `[otlp]` 段：缺省值与边界校验（usage-observability T004）
#[cfg(test)]
mod otlp_config {
    use crate::common;
    use fluxweir::config::{validate_config, OtlpSection};

    #[test]
    fn test_otlp_section_defaults() {
        let otlp = OtlpSection::default();
        assert!(!otlp.enabled, "缺段/缺字段必须禁用（双门控运行时半边）");
        assert!(otlp.endpoint.is_empty());
        assert_eq!(otlp.interval_secs, 60);
        assert_eq!(otlp.timeout_secs, 10);

        let config = common::test_config(vec![]);
        assert_eq!(config.otlp, otlp, "Config 缺段走同一 Default");
        assert!(validate_config(&config).is_ok(), "缺省段合法");
    }

    #[test]
    fn test_otlp_interval_secs_out_of_range_rejected() {
        let mut config = common::test_config(vec![]);
        config.otlp.interval_secs = 0;
        assert!(validate_config(&config).is_err(), "0 必须拒绝");
        config.otlp.interval_secs = 86_401;
        assert!(validate_config(&config).is_err(), "超上限必须拒绝");
        config.otlp.interval_secs = 1;
        assert!(validate_config(&config).is_ok(), "下边界合法");
        config.otlp.interval_secs = 86_400;
        assert!(validate_config(&config).is_ok(), "上边界合法");
    }

    #[test]
    fn test_otlp_timeout_secs_out_of_range_rejected() {
        let mut config = common::test_config(vec![]);
        config.otlp.timeout_secs = 0;
        assert!(validate_config(&config).is_err(), "0 必须拒绝");
        config.otlp.timeout_secs = 301;
        assert!(validate_config(&config).is_err(), "超上限必须拒绝");
        config.otlp.timeout_secs = 300;
        assert!(validate_config(&config).is_ok(), "上边界合法");
    }

    #[test]
    fn test_otlp_enabled_requires_endpoint() {
        let mut config = common::test_config(vec![]);
        config.otlp.enabled = true;
        config.otlp.endpoint = String::new();
        assert!(validate_config(&config).is_err(), "enabled 空端点必须拒绝");
        config.otlp.endpoint = "  ".to_string();
        assert!(validate_config(&config).is_err(), "纯空白端点必须拒绝");
        config.otlp.endpoint = "http://localhost:4318/v1/metrics".to_string();
        assert!(validate_config(&config).is_ok(), "enabled + 端点合法");

        let mut config = common::test_config(vec![]);
        config.otlp.enabled = false;
        config.otlp.endpoint = String::new();
        assert!(
            validate_config(&config).is_ok(),
            "disabled 不校验 endpoint（运行时半边关着即可）"
        );
    }

    /// `[usage_insights]`：缺省开启（内部统计零外部面），缺段与显式同值。
    #[test]
    fn test_usage_insights_defaults_enabled() {
        let insights = fluxweir::config::UsageInsightsSection::default();
        assert!(insights.enabled, "缺省必须开启：纯内部统计无对外面");

        let config = common::test_config(vec![]);
        assert_eq!(config.usage_insights, insights, "Config 缺段走同一 Default");
        assert!(validate_config(&config).is_ok(), "缺省段合法");
    }

    /// 关闭不影响配置合法性（运行期门，非启动期校验）。
    #[test]
    fn test_usage_insights_disabled_is_valid() {
        let mut config = common::test_config(vec![]);
        config.usage_insights.enabled = false;
        assert!(
            validate_config(&config).is_ok(),
            "关闭是运行期开关，不拒绝启动"
        );
    }
}

/// `[routing.affinity]` 段：缺省值 / 非法 mode 点名拒绝 / vendor TTL 边界。
#[cfg(test)]
mod affinity_config {
    use crate::common;
    use fluxweir::config::validate_config;

    /// `[routing.affinity]`：缺省 legacy（现行为冻结）+ 阈值/窗口缺省值；
    /// 非法 mode 点名拒绝；vendor cache_ttl_secs 越界拒绝（边界含端点）。
    #[test]
    fn test_affinity_section_defaults() {
        let section = fluxweir::config::AffinitySection::default();
        assert_eq!(section.mode, "legacy", "缺段必须 legacy（零回归）");
        assert_eq!(section.cache_worth_tokens, 1024);
        assert_eq!(section.turn_gap_secs, 120);

        let config = common::test_config(vec![]);
        assert_eq!(
            config.routing.affinity, section,
            "Config 缺段走同一 Default"
        );
        assert!(validate_config(&config).is_ok(), "缺省段合法");
    }

    #[test]
    fn test_affinity_mode_unknown_rejected() {
        let mut config = common::test_config(vec![]);
        config.routing.affinity.mode = "autoo".into();
        let err = validate_config(&config).expect_err("未知 mode 必须点名拒绝");
        assert!(
            err.to_string().contains("autoo"),
            "错误信息必须回显原值（脱敏口径下唯一可透出的配置枚举词）: {err}"
        );
        for mode in ["legacy", "auto", "turn", "off", "AUTO", " Off "] {
            config.routing.affinity.mode = mode.into();
            assert!(
                validate_config(&config).is_ok(),
                "合法值 {mode} 必须放行（大小写不敏感 + trim）"
            );
        }
    }

    #[test]
    fn test_vendor_cache_ttl_secs_out_of_range_rejected() {
        let mut config = common::test_config(vec![common::base_vendor("v1", "")]);
        config.vendors[0].cache_ttl_secs = 0;
        assert!(
            validate_config(&config).is_err(),
            "0 必须拒绝（写完即过期）"
        );
        config.vendors[0].cache_ttl_secs = 86_401;
        assert!(validate_config(&config).is_err(), "超上限必须拒绝");
        config.vendors[0].cache_ttl_secs = 1;
        assert!(validate_config(&config).is_ok(), "下边界合法");
        config.vendors[0].cache_ttl_secs = 3_600;
        assert!(validate_config(&config).is_ok(), "1h 缓存部署合法");
    }
}

/// `[[quota.windows]]` / `[[vendor_budgets]]` / `[routing.quota_factor]`：
/// 旧单窗字段兼容映射、数组优先、闭集与数值域校验、因子模式闭集。
#[cfg(test)]
mod quota_windows_config {
    use crate::common;
    use fluxweir::config::validate_config;

    use fluxweir::config::{QuotaSection, QuotaWindowConfig};

    /// 只配旧字段（无 `[[quota.windows]]`）：生效窗口编译为 `id="default"`
    /// 的 rolling 窗，行为与历史逐位一致（R-quota-001 兼容验收）。
    #[test]
    fn test_legacy_single_window_maps_to_default() {
        let mut config = common::test_config(vec![]);
        config.quota = QuotaSection {
            window_secs: 1800,
            ..Default::default()
        };
        assert!(config.quota.windows.is_empty());
        let effective = fluxweir::config::effective_quota_windows(&config.quota);
        assert_eq!(effective.len(), 1, "旧字段编译为单窗");
        assert_eq!(effective[0].id, "default");
        assert_eq!(effective[0].rolling_secs, 1800, "window_secs 原值继承");
        assert_eq!(effective[0].period, "rolling");
        assert!(validate_config(&config).is_ok(), "只配旧字段必须合法");
    }

    /// 两形态并存：`[[quota.windows]]` 优先、旧 `window_secs` 忽略
    ///（加载期 WARN，这里断言生效集为数组原样）。
    #[test]
    fn test_windows_array_takes_precedence_over_legacy_fields() {
        let mut config = common::test_config(vec![]);
        config.quota.window_secs = 1800;
        config.quota.windows = vec![
            QuotaWindowConfig {
                id: "daily".into(),
                period: "calendar".into(),
                calendar: "daily".into(),
                tokens_limit: 100_000,
                ..Default::default()
            },
            QuotaWindowConfig {
                id: "weekly".into(),
                period: "calendar".into(),
                calendar: "weekly".into(),
                tokens_limit: 700_000,
                ..Default::default()
            },
        ];
        let effective = fluxweir::config::effective_quota_windows(&config.quota);
        assert_eq!(
            effective, config.quota.windows,
            "数组非空时原样生效（旧字段忽略）"
        );
        assert!(validate_config(&config).is_ok());
    }

    /// 合法多窗（rolling + calendar 混排、global 作用域）放行。
    #[test]
    fn test_valid_windows_pass() {
        let mut config = common::test_config(vec![]);
        config.quota.windows = vec![
            QuotaWindowConfig {
                id: "burst".into(),
                rolling_secs: 300,
                tokens_limit: 10_000,
                ..Default::default()
            },
            QuotaWindowConfig {
                id: "fleet".into(),
                scope: "global".into(),
                period: "calendar".into(),
                calendar: "monthly".into(),
                tz_offset_secs: 8 * 3600,
                cost_limit_millicents: 5_000_000,
                ..Default::default()
            },
        ];
        assert!(validate_config(&config).is_ok());
    }

    /// id 空 / 重复：窗口 id 是账本行键与指标标签，二者都拒绝。
    #[test]
    fn test_window_id_empty_and_duplicate_rejected() {
        let mut config = common::test_config(vec![]);
        config.quota.windows = vec![QuotaWindowConfig {
            id: "  ".into(),
            rolling_secs: 60,
            tokens_limit: 1,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("空 id 必须拒绝");
        assert!(err.to_string().contains("quota.windows"), "{err}");

        config.quota.windows = vec![
            QuotaWindowConfig {
                id: "daily".into(),
                rolling_secs: 60,
                tokens_limit: 1,
                ..Default::default()
            },
            QuotaWindowConfig {
                id: "daily".into(),
                rolling_secs: 120,
                tokens_limit: 1,
                ..Default::default()
            },
        ];
        let err = validate_config(&config).expect_err("重复 id 必须拒绝");
        assert!(err.to_string().contains("daily"), "错误回显冲突 id: {err}");
    }

    /// scope / period / calendar 闭集与两形态互斥逐一点名拒绝。
    #[test]
    fn test_window_period_and_scope_closed_sets() {
        let base = || QuotaWindowConfig {
            id: "w".into(),
            rolling_secs: 60,
            tokens_limit: 1,
            ..Default::default()
        };
        let mut config = common::test_config(vec![]);

        config.quota.windows = vec![QuotaWindowConfig {
            scope: "tenant".into(),
            ..base()
        }];
        let err = validate_config(&config).expect_err("非法 scope 必须拒绝");
        assert!(err.to_string().contains("tenant"), "{err}");

        config.quota.windows = vec![QuotaWindowConfig {
            period: "hourly".into(),
            ..base()
        }];
        let err = validate_config(&config).expect_err("非法 period 必须拒绝");
        assert!(err.to_string().contains("hourly"), "{err}");

        config.quota.windows = vec![QuotaWindowConfig {
            rolling_secs: 0,
            ..base()
        }];
        assert!(validate_config(&config).is_err(), "rolling_secs=0 必须拒绝");

        config.quota.windows = vec![QuotaWindowConfig {
            rolling_secs: 60,
            calendar: "daily".into(),
            ..base()
        }];
        let err = validate_config(&config).expect_err("两形态同配必须拒绝");
        assert!(err.to_string().contains("w"), "{err}");

        config.quota.windows = vec![QuotaWindowConfig {
            rolling_secs: 0,
            period: "calendar".into(),
            calendar: "yearly".into(),
            ..base()
        }];
        let err = validate_config(&config).expect_err("非法 calendar 必须拒绝");
        assert!(err.to_string().contains("yearly"), "{err}");

        config.quota.windows = vec![QuotaWindowConfig {
            rolling_secs: 0,
            period: "calendar".into(),
            calendar: "daily".into(),
            tz_offset_secs: 86_401,
            ..base()
        }];
        assert!(
            validate_config(&config).is_err(),
            "tz_offset_secs 超 ±86400 必须拒绝"
        );
    }

    /// scope=global 双零拒绝：全局窗按请求租户回填会让同一窗的判定随
    /// 请求者漂移（A 租户的 per-key 限额约束整个舰队）、无限额租户整体
    /// 绕过——舰队级约束必须配置面显式；显式单维度限额放行。
    #[test]
    fn test_global_window_zero_limits_rejected() {
        let mut config = common::test_config(vec![]);
        config.quota.windows = vec![QuotaWindowConfig {
            id: "fleet".into(),
            scope: "global".into(),
            rolling_secs: 60,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("global 双零必须拒绝（不回填）");
        assert!(err.to_string().contains("fleet"), "{err}");

        config.quota.windows = vec![QuotaWindowConfig {
            id: "fleet".into(),
            scope: "global".into(),
            rolling_secs: 60,
            tokens_limit: 100_000,
            ..Default::default()
        }];
        assert!(validate_config(&config).is_ok(), "显式单维度限额放行");
    }

    /// id 长度上限（落 quota_ledger.window_id，MySQL VARCHAR(128)，超长行
    /// 每 flush 必败改启动即拒）与 ASCII 大小写变体去重（window id 进
    /// 指标标签与账本行键，仅差大小写会产生双标签/双行）。
    #[test]
    fn test_window_id_length_and_case_variant_rejected() {
        let mut config = common::test_config(vec![]);
        config.quota.windows = vec![QuotaWindowConfig {
            id: "w".repeat(129),
            rolling_secs: 60,
            tokens_limit: 1,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("超长 id 必须拒绝");
        assert!(err.to_string().contains("128"), "{err}");

        config.quota.windows = vec![
            QuotaWindowConfig {
                id: "daily".into(),
                rolling_secs: 60,
                tokens_limit: 1,
                ..Default::default()
            },
            QuotaWindowConfig {
                id: "Daily".into(),
                rolling_secs: 120,
                tokens_limit: 1,
                ..Default::default()
            },
        ];
        let err = validate_config(&config).expect_err("仅大小写不同的 id 必须拒绝");
        assert!(err.to_string().contains("Daily"), "{err}");
    }

    /// `[[vendor_budgets]]`：vendor_id 必须对得上 `[[vendors]]`、去重、
    /// 双限额零拒绝、周期闭集同源；合法条目放行。
    #[test]
    fn test_vendor_budgets_validation() {
        let budget = || fluxweir::config::VendorBudgetConfig {
            vendor_id: "v1".into(),
            tokens_limit: 1_000_000,
            period: "calendar".into(),
            calendar: "weekly".into(),
            ..Default::default()
        };

        let mut config = common::test_config(vec![common::base_vendor("v1", "")]);
        config.vendor_budgets = vec![budget()];
        assert!(validate_config(&config).is_ok(), "合法预算放行");

        config.vendor_budgets = vec![fluxweir::config::VendorBudgetConfig {
            vendor_id: "ghost".into(),
            tokens_limit: 1,
            rolling_secs: 60,
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("未知 vendor 必须拒绝");
        assert!(err.to_string().contains("ghost"), "{err}");

        config.vendor_budgets = vec![budget(), budget()];
        let err = validate_config(&config).expect_err("重复 vendor_id 必须拒绝");
        assert!(err.to_string().contains("v1"), "{err}");

        config.vendor_budgets = vec![fluxweir::config::VendorBudgetConfig {
            vendor_id: "v1".into(),
            rolling_secs: 60,
            ..Default::default()
        }];
        assert!(
            validate_config(&config).is_err(),
            "双限额同时为 0 必须拒绝（空转段）"
        );

        config.vendor_budgets = vec![fluxweir::config::VendorBudgetConfig {
            vendor_id: "v1".into(),
            tokens_limit: 1,
            period: "calendar".into(),
            calendar: "solar".into(),
            ..Default::default()
        }];
        let err = validate_config(&config).expect_err("预算周期闭集同 quota.windows");
        assert!(err.to_string().contains("solar"), "{err}");
    }

    /// `[routing.quota_factor]`：缺省 off（零参与）、w_quota 同温标；
    /// 非法 mode 点名拒绝；合法值大小写不敏感。
    #[test]
    fn test_quota_factor_section_defaults_and_closed_set() {
        let section = fluxweir::config::QuotaFactorSection::default();
        assert_eq!(section.mode, "off", "缺段必须 off（零回归）");
        assert!((section.w_quota - 0.3).abs() < 1e-9);

        let config = common::test_config(vec![]);
        assert_eq!(
            config.routing.quota_factor, section,
            "Config 缺段走同一 Default"
        );
        assert!(validate_config(&config).is_ok(), "缺省段合法");

        let mut bad = common::test_config(vec![]);
        bad.routing.quota_factor.mode = "fastest".into();
        let err = validate_config(&bad).expect_err("未知 mode 必须点名拒绝");
        assert!(err.to_string().contains("fastest"), "{err}");

        for mode in ["off", "smart", "pace", "SMART", " Pace "] {
            let mut ok = common::test_config(vec![]);
            ok.routing.quota_factor.mode = mode.into();
            assert!(
                validate_config(&ok).is_ok(),
                "合法值 {mode} 必须放行（大小写不敏感 + trim）"
            );
        }
    }

    /// `[routing.quota_factor]` 配额时钟补全（R-budget-002）：ε/阈值/乘数
    /// 数值域与档序校验；缺省全绿（平局打破 ε=0.05、降档启用）。
    #[test]
    fn test_quota_factor_clock_fields_validation() {
        use fluxweir::config::QuotaFactorSection;
        let section = QuotaFactorSection::default();
        assert!((section.tie_epsilon - 0.05).abs() < 1e-9, "ε 缺省 0.05");
        assert!(section.degrade_enabled, "降档缺省启用");
        assert!((section.degrade_used_tier1 - 0.90).abs() < 1e-9);
        assert!((section.degrade_multiplier_tier1 - 0.9).abs() < 1e-9);
        assert!((section.degrade_used_tier2 - 0.98).abs() < 1e-9);
        assert!((section.degrade_multiplier_tier2 - 0.75).abs() < 1e-9);
        assert!(
            validate_config(&common::test_config(vec![])).is_ok(),
            "缺省段合法"
        );

        fn with_field(mutate: impl FnOnce(&mut QuotaFactorSection)) -> fluxweir::config::Config {
            let mut config = common::test_config(vec![]);
            mutate(&mut config.routing.quota_factor);
            config
        }
        // ε 越界（<0 或 >1）
        for eps in [-0.1f64, 1.5, f64::NAN] {
            let err =
                validate_config(&with_field(|q| q.tie_epsilon = eps)).expect_err("ε 越界必须拒绝");
            assert!(err.to_string().contains("tie_epsilon"), "{err}");
        }
        // 阈值/乘数越界（≤0 或 >1）
        let err = validate_config(&with_field(|q| q.degrade_used_tier1 = 0.0))
            .expect_err("阈值越界必须拒绝");
        assert!(err.to_string().contains("degrade_used_tier1"), "{err}");
        let err = validate_config(&with_field(|q| q.degrade_multiplier_tier2 = 1.5))
            .expect_err("乘数越界必须拒绝");
        assert!(
            err.to_string().contains("degrade_multiplier_tier2"),
            "{err}"
        );
        // 档序：tier1 ≥ tier2 拒绝（档二永不可达）
        let err = validate_config(&with_field(|q| {
            q.degrade_used_tier1 = 0.98;
            q.degrade_used_tier2 = 0.98;
        }))
        .expect_err("档序倒置必须拒绝");
        assert!(err.to_string().contains("tier"), "{err}");
    }

    /// `[key_budgets]`：hash 必须 64 位小写 hex、cost_limit_millicents > 0、
    /// window 闭集 daily|monthly、同 (hash, window) 不得重复；空段整体关闭。
    #[test]
    fn test_key_budgets_validation() {
        /// sha256("test-good-key")（与 quota/rate-limit 测试同值的小写 hex）。
        const KEY_HASH: &str = "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";
        let rule = || fluxweir::config::KeyBudgetRule {
            api_key_hash: KEY_HASH.into(),
            cost_limit_millicents: 100,
            window: "daily".into(),
            fail_open: false,
        };

        let mut config = common::test_config(vec![]);
        config.key_budgets.rules = vec![rule()];
        assert!(validate_config(&config).is_ok(), "合法规则放行");

        config.key_budgets.rules = vec![fluxweir::config::KeyBudgetRule {
            window: "monthly".into(),
            ..rule()
        }];
        assert!(validate_config(&config).is_ok(), "monthly 窗口放行");

        config.key_budgets.rules = vec![];
        assert!(validate_config(&config).is_ok(), "空段整体关闭不校验");

        config.key_budgets.rules = vec![fluxweir::config::KeyBudgetRule {
            api_key_hash: "abc".into(),
            ..rule()
        }];
        let err = validate_config(&config).expect_err("非 64 位 hex 摘要必须拒绝");
        assert!(err.to_string().contains("abc"), "{err}");

        config.key_budgets.rules = vec![fluxweir::config::KeyBudgetRule {
            api_key_hash: KEY_HASH.to_uppercase(),
            ..rule()
        }];
        assert!(
            validate_config(&config).is_err(),
            "大写 hex 必须拒绝（与 auth 摘要同口径：小写）"
        );

        config.key_budgets.rules = vec![fluxweir::config::KeyBudgetRule {
            cost_limit_millicents: 0,
            ..rule()
        }];
        let err = validate_config(&config).expect_err("limit=0 必须拒绝");
        assert!(
            err.to_string().contains("0") || err.to_string().contains(KEY_HASH),
            "{err}"
        );

        config.key_budgets.rules = vec![fluxweir::config::KeyBudgetRule {
            window: "weekly".into(),
            ..rule()
        }];
        let err = validate_config(&config).expect_err("window 闭集 daily|monthly");
        assert!(err.to_string().contains("weekly"), "{err}");

        config.key_budgets.rules = vec![rule(), rule()];
        let err = validate_config(&config).expect_err("同 (hash, window) 重复必须拒绝");
        assert!(err.to_string().contains(KEY_HASH), "{err}");
    }
}

/// `[auth.oidc_rules]`（R-auth-001）：claims 通配/前缀意图硬拦、母键必须
/// 存在、ttl 越界拒绝、合法规则放行；无段/无规则整体关闭。
#[cfg(test)]
mod oidc_rules_config {
    use crate::common;
    use fluxweir::config::{validate_config, OidcRuleConfig, OidcRulesSection};
    use std::collections::HashMap;

    /// sha256("test-good-key")（与 key_budgets 校验测试同值的小写 hex）。
    const KEY_HASH: &str = "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303";

    fn rule() -> OidcRuleConfig {
        OidcRuleConfig {
            issuer: "https://token.example".into(),
            audience: "gateway-client".into(),
            claims: HashMap::new(),
            bind_key_hash: KEY_HASH.into(),
            ttl_secs: 3600,
        }
    }

    fn config(rules: Vec<OidcRuleConfig>) -> fluxweir::config::Config {
        let mut config = common::test_config(vec![]);
        config.auth = Some(fluxweir::config::AuthSection {
            enabled: true,
            api_keys: vec![KEY_HASH.into()],
            header: Default::default(),
            keys: vec![],
            oidc_rules: Some(OidcRulesSection { rules }),
        });
        config
    }

    #[test]
    fn test_oidc_rules_validation() {
        assert!(
            validate_config(&config(vec![rule()])).is_ok(),
            "合法规则放行"
        );
        assert!(validate_config(&config(vec![])).is_ok(), "空规则整体关闭");

        // claims 通配/前缀意图：键或值含 * / ? 一律硬拦
        for claims in [
            HashMap::from([("role".into(), "ad*min".into())]),
            HashMap::from([("env*".into(), "prod".into())]),
            HashMap::from([("env?".into(), "prod".into())]),
        ] {
            let err = validate_config(&config(vec![OidcRuleConfig { claims, ..rule() }]))
                .expect_err("claims 通配/前缀意图必须硬拦");
            assert!(
                err.to_string().contains("*") || err.to_string().contains("?"),
                "{err}"
            );
        }

        // 母键缺失：hash 不在 api_keys/keys
        let err = validate_config(&config(vec![OidcRuleConfig {
            bind_key_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
            ..rule()
        }]))
        .expect_err("母键缺失必须拒绝");
        assert!(err.to_string().contains("e3b0c442"), "{err}");

        // 母键格式：非 64 hex 同拒
        let err = validate_config(&config(vec![OidcRuleConfig {
            bind_key_hash: "not-a-hash".into(),
            ..rule()
        }]))
        .expect_err("母键格式错误必须拒绝");
        assert!(err.to_string().contains("not-a-hash"), "{err}");

        // ttl 越界：0 与 86401
        for ttl in [0u64, 86_401] {
            let err = validate_config(&config(vec![OidcRuleConfig {
                ttl_secs: ttl,
                ..rule()
            }]))
            .expect_err("ttl 越界必须拒绝");
            assert!(err.to_string().contains(&ttl.to_string()), "{err}");
        }

        // issuer 闭集：http 仅 loopback 放行
        let err = validate_config(&config(vec![OidcRuleConfig {
            issuer: "http://token.example".into(),
            ..rule()
        }]))
        .expect_err("非 loopback http issuer 必须拒绝");
        assert!(err.to_string().contains("http://token.example"), "{err}");
        let mut loopback = rule();
        loopback.issuer = "http://127.0.0.1:8080".into();
        assert!(
            validate_config(&config(vec![loopback])).is_ok(),
            "loopback http 放行"
        );

        // 缺段整体关闭
        let mut none = common::test_config(vec![]);
        none.auth = Some(fluxweir::config::AuthSection {
            enabled: true,
            api_keys: vec![],
            header: Default::default(),
            keys: vec![],
            oidc_rules: None,
        });
        assert!(validate_config(&none).is_ok(), "无 oidc_rules 段合法");
    }
}

/// `[catalog_sync]` 段：缺省值（catalog-sync 双门控运行时半边）与
/// source_url 校验（仅 HTTPS；loopback 放行 http 供嵌入式上游测试）。
#[cfg(test)]
mod catalog_sync_config {
    use crate::common;
    use fluxweir::config::{validate_config, CatalogSyncSection};
    use std::collections::HashMap;

    #[test]
    fn test_catalog_sync_section_defaults() {
        let section = CatalogSyncSection::default();
        assert!(!section.enabled, "缺段/缺字段必须禁用（双门控运行时半边）");
        assert_eq!(section.source_url, "https://models.dev/api.json");
        assert_eq!(section.interval_secs, 0, "0 = 不挂周期任务");
        assert_eq!(section.timeout_secs, 10);
        assert!(
            section.alias.is_empty(),
            "缺省无别名表（slug 即 vendor_id）"
        );

        let config = common::test_config(vec![]);
        assert_eq!(config.catalog_sync, section, "Config 缺段走同一 Default");
        assert!(validate_config(&config).is_ok(), "缺省段合法");
    }

    /// `[catalog_sync.alias]` 表解析：slug → vendor_id 覆盖生效。
    #[test]
    fn test_catalog_sync_alias_table_parses() {
        let config: fluxweir::config::Config = toml::from_str(
            r#"
            [server]
            host = "127.0.0.1"
            port = 8080
            [database]
            path = ":memory:"
            [[vendors]]
            id = "v"
            name = "V"
            type_ = "openai"
            api_key_env = "K"
            rate_limit = { type = "token", max_tokens = 1 }
            concurrency = { requests_per_second = 1 }
            [catalog_sync]
            enabled = true
            [catalog_sync.alias]
            anthropic = "claude-main"
            openai = "gpt-vendor"
            "#,
        )
        .expect("alias 表解析");
        assert_eq!(
            config
                .catalog_sync
                .alias
                .get("anthropic")
                .map(String::as_str),
            Some("claude-main")
        );
        assert_eq!(config.catalog_sync.alias.len(), 2);
    }

    /// enabled=false 完全旁路不校验（与「缺段」同语义，difficulty 先例）；
    /// enabled=true 时非 HTTPS 公网源拒绝、HTTPS 放行、loopback http 放行
    ///（嵌入式上游测试面）、非法 URL 拒绝。
    #[test]
    fn test_catalog_sync_source_url_validation() {
        // 旁路：disabled 不校验 URL
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = false;
        config.catalog_sync.source_url = "not a url".into();
        assert!(
            validate_config(&config).is_ok(),
            "disabled 不校验 source_url（运行时半边关着即可）"
        );

        // enabled + 公网 http 拒绝
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.source_url = "http://models.dev/api.json".into();
        let err = validate_config(&config).expect_err("公网 http 源必须拒绝");
        assert!(
            err.to_string().contains("http://models.dev/api.json"),
            "错误必须回显原值: {err}"
        );

        // enabled + https 放行
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.source_url = "https://mirror.example.com/api.json".into();
        assert!(validate_config(&config).is_ok(), "自定义 https 源合法");

        // enabled + loopback http 放行（嵌入式上游测试）
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.source_url = "http://127.0.0.1:1234/api.json".into();
        assert!(
            validate_config(&config).is_ok(),
            "loopback http 放行（嵌入式上游）"
        );

        // enabled + 非 https 非 loopback 的其它 scheme 拒绝
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.source_url = "ftp://models.dev/api.json".into();
        assert!(validate_config(&config).is_err(), "ftp scheme 必须拒绝");
    }

    /// alias 表键值非空白校验（enabled 时）：空键/空值拒绝。
    #[test]
    fn test_catalog_sync_alias_blank_entries_rejected() {
        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.alias = HashMap::from([("".to_string(), "v".to_string())]);
        assert!(validate_config(&config).is_err(), "空 slug 键必须拒绝");

        let mut config = common::test_config(vec![]);
        config.catalog_sync.enabled = true;
        config.catalog_sync.alias = HashMap::from([("anthropic".to_string(), "  ".to_string())]);
        assert!(validate_config(&config).is_err(), "空白 vendor_id 必须拒绝");
    }
}

/// `[subscription]` / `[[subscription_accounts]]` 段：缺省值（subscription-vendor
/// 双门控运行时半边）与账号声明校验（id 唯一非空、vendor 绑定存在）。
#[cfg(test)]
mod subscription_config {
    use crate::common;
    use fluxweir::config::{validate_config, SubscriptionAccountConfig, SubscriptionSection};

    #[test]
    fn test_subscription_section_defaults() {
        let section = SubscriptionSection::default();
        assert!(!section.enabled, "缺段/缺字段必须禁用（双门控运行时半边）");
        assert_eq!(
            section.client_id, "",
            "缺省未配置 client_id（authorize 点名拒绝）"
        );

        let config = common::test_config(vec![]);
        assert_eq!(config.subscription, section, "Config 缺段走同一 Default");
        assert!(config.subscription_accounts.is_empty(), "缺省无账号声明");
        assert!(validate_config(&config).is_ok(), "缺省段合法");
    }

    /// `[[subscription_accounts]]` 解析：id + vendor_id 声明生效，缺段为空。
    #[test]
    fn test_subscription_accounts_table_parses() {
        let config: fluxweir::config::Config = toml::from_str(
            r#"
            [server]
            host = "127.0.0.1"
            port = 8080
            [database]
            path = ":memory:"
            [[vendors]]
            id = "claude-main"
            name = "Claude"
            type_ = "anthropic"
            api_key_env = "K"
            rate_limit = { type = "token", max_tokens = 1 }
            concurrency = { requests_per_second = 1 }
            [subscription]
            enabled = true
            client_id = "my-client-id"
            [[subscription_accounts]]
            id = "sub-1"
            vendor_id = "claude-main"
            "#,
        )
        .expect("subscription 段解析");
        assert!(config.subscription.enabled);
        assert_eq!(config.subscription.client_id, "my-client-id");
        assert_eq!(
            config.subscription_accounts,
            vec![SubscriptionAccountConfig {
                id: "sub-1".to_string(),
                vendor_id: "claude-main".to_string(),
            }]
        );
        assert!(validate_config(&config).is_ok(), "合法声明通过校验");
    }

    /// 校验三态：空 id / 重复 id / 未知 vendor 绑定，全部点名拒绝；
    /// enabled=false 时声明仍校验（声明是持久 vault 键的绑定面，与开关正交）。
    #[test]
    fn test_subscription_accounts_validation() {
        let mut config = common::test_config(vec![common::base_vendor("claude-main", "")]);
        config.subscription_accounts = vec![SubscriptionAccountConfig {
            id: "  ".to_string(),
            vendor_id: "claude-main".to_string(),
        }];
        let err = validate_config(&config).expect_err("空 id 必须拒绝");
        assert!(err.to_string().contains("id"), "错误需点名 id 维度: {err}");

        let mut config = common::test_config(vec![common::base_vendor("claude-main", "")]);
        config.subscription_accounts = vec![
            SubscriptionAccountConfig {
                id: "sub-1".to_string(),
                vendor_id: "claude-main".to_string(),
            },
            SubscriptionAccountConfig {
                id: "sub-1".to_string(),
                vendor_id: "claude-main".to_string(),
            },
        ];
        let err = validate_config(&config).expect_err("重复 id 必须拒绝");
        assert!(err.to_string().contains("sub-1"), "错误需回显原值: {err}");

        let mut config = common::test_config(vec![common::base_vendor("claude-main", "")]);
        config.subscription_accounts = vec![SubscriptionAccountConfig {
            id: "sub-1".to_string(),
            vendor_id: "ghost-vendor".to_string(),
        }];
        let err = validate_config(&config).expect_err("未知 vendor 绑定必须拒绝");
        assert!(
            err.to_string().contains("ghost-vendor"),
            "错误需回显原值: {err}"
        );

        // 空白 vendor_id 拒绝
        let mut config = common::test_config(vec![common::base_vendor("claude-main", "")]);
        config.subscription_accounts = vec![SubscriptionAccountConfig {
            id: "sub-1".to_string(),
            vendor_id: " ".to_string(),
        }];
        assert!(validate_config(&config).is_err(), "空白 vendor_id 必须拒绝");

        // enabled=false 时声明仍校验
        let mut config = common::test_config(vec![common::base_vendor("claude-main", "")]);
        config.subscription.enabled = false;
        config.subscription_accounts = vec![SubscriptionAccountConfig {
            id: "sub-1".to_string(),
            vendor_id: "ghost-vendor".to_string(),
        }];
        assert!(
            validate_config(&config).is_err(),
            "enabled=false 不豁免声明校验"
        );
    }
}

#[cfg(test)]
mod web_search {
    use fluxweir::config::{parse_config_content, WebSearchProviderConfig, WebSearchSection};
    use std::path::Path;

    fn parse(web_search_toml: &str) -> Result<fluxweir::config::Config, fluxweir::GateError> {
        let content = format!(
            r#"
[server]
host = "127.0.0.1"
port = 0

[database]
path = ":memory:"

[[vendors]]
id = "v"
name = "V"
type_ = "openai"
api_key_env = "V_KEY"
concurrency = {{ requests_per_second = 1 }}
rate_limit = {{ type = "token", max_tokens = 1000 }}

{web_search_toml}
"#
        );
        parse_config_content(Path::new("config.toml"), &content)
    }

    /// 缺段 = 安全默认（enabled=false、providers 空）。
    #[test]
    fn web_search_defaults_when_section_absent() {
        let config = parse("").expect("缺段解析");
        assert_eq!(config.web_search, WebSearchSection::default());
        assert!(!config.web_search.enabled);
        assert!(config.web_search.providers.is_empty());
    }

    /// 合法配置解析（env 多 key 池 + base_url）。
    #[test]
    fn web_search_parses_providers() {
        let config = parse(
            r#"[web_search]
enabled = true

[[web_search.providers]]
kind = "tavily"
api_key_env = "TAVILY_KEY"
api_key_envs = ["TAVILY_KEY_2"]

[[web_search.providers]]
kind = "searxng"
base_url = "http://127.0.0.1:8888/search"
"#,
        )
        .expect("合法 web_search 解析");
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.providers.len(), 2);
        let p: &WebSearchProviderConfig = &config.web_search.providers[0];
        assert_eq!(p.kind, "tavily");
        assert_eq!(p.api_key_env, "TAVILY_KEY");
        assert_eq!(p.api_key_envs, vec!["TAVILY_KEY_2".to_string()]);
    }

    /// kind 闭集外 → 加载拒绝（点名原值）。
    #[test]
    fn web_search_rejects_unknown_kind() {
        let err = parse(
            r#"[web_search]
enabled = true

[[web_search.providers]]
kind = "bing"
api_key_env = "BING_KEY"
"#,
        )
        .expect_err("未知 kind 必须拒绝");
        assert!(err.to_string().contains("bing"), "{err}");
    }

    /// searxng 缺 base_url → 加载拒绝。
    #[test]
    fn web_search_rejects_searxng_without_base_url() {
        let err = parse(
            r#"[web_search]
enabled = true

[[web_search.providers]]
kind = "searxng"
"#,
        )
        .expect_err("searxng 缺 base_url 必须拒绝");
        assert!(!err.to_string().is_empty());
    }

    /// 需 key 源的 kind（tavily/brave/exa）全源缺省 → 加载拒绝；searxng 免 key。
    #[test]
    fn web_search_rejects_keyless_tavily_but_allows_keyless_searxng() {
        let err = parse(
            r#"[web_search]
enabled = true

[[web_search.providers]]
kind = "tavily"
"#,
        )
        .expect_err("tavily 无 key 源必须拒绝");
        assert!(!err.to_string().is_empty());

        parse(
            r#"[web_search]
enabled = true

[[web_search.providers]]
kind = "searxng"
base_url = "http://127.0.0.1:8888/search"
"#,
        )
        .expect("searxng 免 key 合法");
    }

    /// enabled=false 完全旁路不校验（与 difficulty 缺段同语义）。
    #[test]
    fn web_search_disabled_bypasses_validation() {
        parse(
            r#"[web_search]
enabled = false

[[web_search.providers]]
kind = "bing"
"#,
        )
        .expect("disabled 时不校验 kind");
    }
}

/// `[masking]` 段：默认零行为 + 闭集/正则校验（feature `masking` 编译门 +
/// enabled 运行时门，双层隔离）。
#[cfg(test)]
mod masking_config {
    use crate::common;
    use fluxweir::config::{parse_config_content, validate_config, MaskingSection};

    #[test]
    fn test_masking_section_defaults() {
        let section = MaskingSection::default();
        assert!(!section.enabled, "默认 false = 零行为");
        assert!(section.kinds.is_empty(), "空 kinds = 全启用");
        assert!(section.actions.is_empty(), "空 actions = 缺省 mask");
        assert!(section.words.is_empty());
        assert!(!section.words_any_case);
        assert!(section.rules.is_empty());
        assert!(section.allow.is_empty());
    }

    /// enabled=false（默认）时零行为变化：配置加载/校验路径与无段一致。
    #[test]
    fn test_masking_disabled_is_inert() {
        let mut config = common::test_config(vec![]);
        config.masking = MaskingSection::default();
        validate_config(&config).expect("默认段必过校验");
        // 序列化往返：段缺省不丢（persist_and_swap 全量写回不抹段）
        let rendered = toml::to_string_pretty(&config).expect("序列化");
        assert!(
            rendered.contains("[masking]"),
            "Section 字段落地防热更抹段: {}",
            &rendered[..rendered.len().min(400)]
        );
    }

    /// kinds/actions 闭集：非法 kind / 非法动作加载拒绝（原值点名）。
    #[test]
    fn test_masking_closed_sets_rejected() {
        let mut config = common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            kinds: vec!["secret_kind".into()],
            ..Default::default()
        };
        let err = validate_config(&config).expect_err("非法 kind 必须拒绝");
        assert!(err.to_string().contains("secret_kind"), "{err}");

        let mut config = common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            actions: [("api_key".into(), "delete".into())].into_iter().collect(),
            ..Default::default()
        };
        let err = validate_config(&config).expect_err("非法动作必须拒绝");
        assert!(err.to_string().contains("delete"), "{err}");

        let mut config = common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            actions: [("not_a_kind".into(), "mask".into())].into_iter().collect(),
            ..Default::default()
        };
        assert!(validate_config(&config).is_err(), "actions 键同 kinds 闭集");
    }

    /// 合法配置通过；TOML 加载面（含注释段缺省）可解析。
    #[test]
    fn test_masking_valid_config_parses() {
        let parsed = parse_config_content(
            std::path::Path::new("masking-test.toml"),
            r#"
[server]
host = "127.0.0.1"
port = 0

[database]
path = ":memory:"

[[vendors]]
id = "v"
name = "v"
type_ = "openai"
api_key_env = "K"
api_base = "http://127.0.0.1:1"
enabled = true
models = [{ model_id = "m", enabled = true }]
concurrency = { requests_per_second = 1 }
rate_limit = { type = "token", max_tokens = 1000 }

[masking]
enabled = true
words = ["内部门牌"]
rules = ["AKIA[0-9A-Z]{16}"]
allow = ["sk-proj-"]

[masking.actions]
email = "warn"
"#,
        )
        .expect("合法 masking 配置必须可加载");
        assert!(parsed.masking.enabled);
        assert_eq!(parsed.masking.words, vec!["内部门牌".to_string()]);
        assert_eq!(parsed.masking.allow, vec!["sk-proj-".to_string()]);
        assert_eq!(
            parsed.masking.actions.get("email").map(String::as_str),
            Some("warn")
        );
    }

    /// 非法正则：`masking` feature 启用时加载拒绝（刚性）；kind 闭集与
    /// `crate::masking::MaskKind::parse` 单源（防漂移）。
    #[cfg(feature = "masking")]
    #[test]
    fn test_masking_invalid_rule_rejected_and_kind_set_aligned() {
        let mut config = common::test_config(vec![]);
        config.masking = MaskingSection {
            enabled: true,
            rules: vec!["(unclosed".into()],
            ..Default::default()
        };
        let err = validate_config(&config).expect_err("非法正则必须拒绝");
        assert!(
            err.to_string().contains("(unclosed") || !err.to_string().is_empty(),
            "{err}"
        );

        // kind 闭集防漂移：config 侧合法值 ⊆ masking::MaskKind::parse
        for raw in [
            "api_key",
            "private_key",
            "token",
            "email",
            "card",
            "id",
            "custom",
        ] {
            assert!(
                fluxweir::masking::MaskKind::parse(raw).is_some(),
                "config 闭集与 MaskKind::parse 漂移: {raw}"
            );
        }
    }
}
