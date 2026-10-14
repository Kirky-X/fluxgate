// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 缓存成本优化套件集成测试：前缀亲和黏性（同前缀落点 ≤2 厂商）、
//! cache_expected 价格因子偏向、batch 带内过滤。全部默认关回归另测。

#![cfg(feature = "router")]

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use nexusflow::config::{Config, ConfigManager, DatabaseConfig, ModelConfig, ServerConfig};
    use nexusflow::router::{RouteCtx, Router};
    use nexusflow::vendor::VendorManager;
    use std::sync::Arc;

    fn model(id: &str, weight: f64) -> ModelConfig {
        ModelConfig {
            model_id: id.to_string(),
            abstract_id: Some("shared".to_string()),
            weight,
            enabled: true,
        }
    }

    fn vendor(
        id: &str,
        models: Vec<ModelConfig>,
        supports_batch: bool,
    ) -> nexusflow::config::VendorConfig {
        let mut v = common::base_vendor(id, String::new());
        v.models = models;
        v.supports_batch = supports_batch;
        v
    }

    fn make_config(
        prefix_affinity: bool,
        price_factor_mode: &str,
        vendors: Vec<nexusflow::config::VendorConfig>,
    ) -> Config {
        let mut config = Config {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: "./test.db".to_string(),
                pool_size: 5,
                url: String::new(),
            },
            ..common::test_config(vendors)
        };
        config.routing.prefix_affinity.enabled = prefix_affinity;
        config.routing.price_factor_mode = price_factor_mode.to_string();
        config
    }

    fn make_router(config: Config) -> Arc<Router> {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()))
    }

    fn ctx(hash: u64, batch: bool) -> Option<RouteCtx> {
        Some(RouteCtx {
            prefix_hash: hash,
            est_prompt_tokens: 100,
            batch,
        })
    }

    #[tokio::test]
    async fn prefix_affinity_sticks_within_two_vendors() {
        // 4 家厂商同供 "shared" 抽象名；同前缀 60 次采样必须落 ≤2 家
        let config = make_config(
            true,
            "nominal",
            vec![
                vendor("v1", vec![model("m1", 10.0)], false),
                vendor("v2", vec![model("m2", 10.0)], false),
                vendor("v3", vec![model("m3", 10.0)], false),
                vendor("v4", vec![model("m4", 10.0)], false),
            ],
        );
        let router = make_router(config);
        let mut dist: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for _ in 0..60 {
            let d = router
                .route_weighted(
                    "shared",
                    None,
                    None,
                    &[],
                    None,
                    None,
                    None,
                    ctx(0xABCD, false),
                )
                .await
                .expect("路由");
            *dist.entry(d.vendor.id).or_default() += 1;
        }
        println!("[cache-cost] 同前缀 60 次采样分布: {dist:?}");
        assert!(dist.len() <= 2, "前缀亲和必须黏在 ≤2 家: {dist:?}");

        // 异前缀分散：10 个不同前缀覆盖 ≥3 家
        let mut seen = std::collections::HashSet::new();
        for seed in 0..10u64 {
            let d = router
                .route_weighted(
                    "shared",
                    None,
                    None,
                    &[],
                    None,
                    None,
                    None,
                    ctx(seed * 7919, false),
                )
                .await
                .expect("路由");
            seen.insert(d.vendor.id);
        }
        println!("[cache-cost] 10 异前缀覆盖厂商数: {}", seen.len());
        assert!(seen.len() >= 3, "异前缀应分散: {seen:?}");

        // 关闭（默认）：完全随机分散
        let config = make_config(
            false,
            "nominal",
            vec![
                vendor("v1", vec![model("m1", 10.0)], false),
                vendor("v2", vec![model("m2", 10.0)], false),
                vendor("v3", vec![model("m3", 10.0)], false),
                vendor("v4", vec![model("m4", 10.0)], false),
            ],
        );
        let router = make_router(config);
        let mut dist: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for _ in 0..60 {
            let d = router
                .route_weighted(
                    "shared",
                    None,
                    None,
                    &[],
                    None,
                    None,
                    None,
                    ctx(0xABCD, false),
                )
                .await
                .expect("路由");
            *dist.entry(d.vendor.id).or_default() += 1;
        }
        println!("[cache-cost] 关闭亲和 60 次采样分布: {dist:?}");
        assert!(dist.len() >= 3, "关闭时必须随机分散: {dist:?}");
    }

    #[tokio::test]
    async fn cache_expected_factor_prefers_cache_friendly_vendor() {
        // cache_expected 模式：v-cache 的 cache_read 价 0.1 → 期望单价低于
        // v-full 的无缓存 1.0 → 权重显著偏向 v-cache
        let vendors = vec![
            vendor(
                "v-full",
                vec![ModelConfig {
                    model_id: "m-full".to_string(),
                    abstract_id: Some("shared".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                false,
            ),
            vendor(
                "v-cache",
                vec![ModelConfig {
                    model_id: "m-cache".to_string(),
                    abstract_id: Some("shared".to_string()),
                    weight: 1.0,
                    enabled: true,
                }],
                false,
            ),
        ];
        let mut config = make_config(false, "nominal", vendors);
        config.model_catalog = vec![
            nexusflow::config::ModelCatalogEntry {
                model_id: "m-full".into(),
                context_window: 128_000,
                input_price: 1.0,
                output_price: 1.0,
                cache_read_price: 0.0,
                cache_creation_price: 0.0,
                long_context_input_price: 0.0,
                long_context_threshold_tokens: 0,
                currency: Default::default(),
                max_output_tokens: None,
                supports_tools: false,
                supports_thinking: false,
                cache_breakpoints: None,
            },
            nexusflow::config::ModelCatalogEntry {
                model_id: "m-cache".into(),
                context_window: 128_000,
                input_price: 1.0,
                output_price: 1.0,
                cache_read_price: 0.1,
                cache_creation_price: 0.0,
                long_context_input_price: 0.0,
                long_context_threshold_tokens: 0,
                currency: Default::default(),
                max_output_tokens: None,
                supports_tools: false,
                supports_thinking: false,
                cache_breakpoints: None,
            },
        ];
        let router = make_router(config.clone());
        let mut cache_wins = 0;
        for _ in 0..60 {
            let d = router
                .route_weighted("shared", None, None, &[], None, None, None, None)
                .await
                .expect("路由");
            if d.vendor.id == "v-cache" {
                cache_wins += 1;
            }
        }
        println!("[cache-cost] nominal 模式 60 次: v-cache 胜 {cache_wins} 次（等权应 ~30±15）");
        assert!(
            (15..=45).contains(&cache_wins),
            "nominal 等权应近对半: {cache_wins}/60"
        );

        // cache_expected：m-cache 期望单价 (0.1+1)/2 远低于 m-full → 价格因子反转
        let mut config_expected = config;
        config_expected.routing.price_factor_mode = "cache_expected".to_string();
        let router = make_router(config_expected);
        let mut cache_wins = 0;
        for _ in 0..60 {
            let d = router
                .route_weighted("shared", None, None, &[], None, None, None, None)
                .await
                .expect("路由");
            if d.vendor.id == "v-cache" {
                cache_wins += 1;
            }
        }
        println!(
            "[cache-cost] cache_expected 模式 60 次: v-cache 胜 {cache_wins} 次（期望 ~64.5%）"
        );
        assert!(
            cache_wins >= 32,
            "cache_expected 必须让缓存友好厂商显著占优: {cache_wins}/60"
        );
    }

    #[tokio::test]
    async fn batch_flag_filters_to_batch_capable_vendors() {
        let vendors = vec![
            vendor("v-std", vec![model("m1", 10.0)], false),
            vendor("v-batch", vec![model("m2", 1.0)], true),
        ];
        let router = make_router(make_config(false, "nominal", vendors));
        // 带 batch 标记：全部落 v-batch（唯一 supports_batch）
        for _ in 0..20 {
            let d = router
                .route_weighted("shared", None, None, &[], None, None, None, ctx(1, true))
                .await
                .expect("路由");
            assert_eq!(d.vendor.id, "v-batch", "batch 标记必须收缩到 batch 厂商");
        }
        // 无标记：两 vendors 都在候选（60 次至少出现两家）
        let vendors = vec![
            vendor("v-std", vec![model("m1", 10.0)], false),
            vendor("v-batch", vec![model("m2", 1.0)], true),
        ];
        let router = make_router(make_config(false, "nominal", vendors));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..60 {
            let d = router
                .route_weighted("shared", None, None, &[], None, None, None, None)
                .await
                .expect("路由");
            seen.insert(d.vendor.id);
        }
        assert!(seen.len() >= 2, "无标记时应随机分散: {seen:?}");
    }
}
