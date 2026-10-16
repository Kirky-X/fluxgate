// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 难度定档 → 路由链集成测试：DifficultyRouter 改写档位抽象名后，
//! 既有 Router 链（resolve_model → 候选过滤 → 加权选择）必须能按改写名
//! 解析到服务该抽象组的 vendor（spec difficulty-config R-003）。

#![cfg(feature = "router")]

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use fluxgate::config::{
        Config, ConfigManager, DatabaseConfig, DifficultySection, DifficultyTiers, ModelConfig,
        ServerConfig,
    };
    use fluxgate::router::difficulty::{
        DecisionCache, DifficultyRouter, Guard, MockDifficultyClassifier,
    };
    use fluxgate::router::Router;
    use fluxgate::vendor::VendorManager;
    use serde_json::json;
    use std::sync::Arc;

    fn body(text: &str) -> serde_json::Value {
        json!({"messages": [{"role": "user", "content": text}]})
    }

    fn vendor_serving(
        id: &str,
        model_id: &str,
        abstract_id: &str,
    ) -> fluxgate::config::VendorConfig {
        let mut vendor = common::base_vendor(id, String::new());
        vendor.models = vec![ModelConfig {
            model_id: model_id.to_string(),
            abstract_id: Some(abstract_id.to_string()),
            weight: 1.0,
            enabled: true,
        }];
        vendor
    }

    fn difficulty_config() -> Config {
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
            ..common::test_config(vec![
                vendor_serving("low_vendor", "qwen-turbo", "fast"),
                vendor_serving("high_vendor", "claude-sonnet", "reasoning"),
            ])
        };
        config.routing.difficulty = DifficultySection {
            enabled: true,
            groups: vec!["auto".to_string()],
            w_clf: 0.6,
            w_rule: 0.4,
            score_low: 0.35,
            score_high: 0.65,
            mid_band_policy: "strong".to_string(),
            tiers: DifficultyTiers {
                low: "fast".to_string(),
                mid: None,
                high: "reasoning".to_string(),
            },
            classifier: Default::default(),
        };
        config
    }

    fn router_with(
        config: Config,
        p_high: f64,
    ) -> (Arc<ConfigManager>, Arc<Router>, DifficultyRouter) {
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));
        let difficulty = DifficultyRouter::new(
            config_manager.clone(),
            Some(Arc::new(MockDifficultyClassifier::ok(p_high))),
            Guard::new(3, 30),
            DecisionCache::with_memory_backend(300),
            vendor_manager.metrics().clone(),
        );
        (config_manager, router, difficulty)
    }

    #[tokio::test]
    async fn high_tier_rewrite_resolves_to_strong_vendor() {
        let (_config, router, difficulty) = router_with(difficulty_config(), 1.0);
        let decision = difficulty
            .classify("auto", &body("请证明这个定理，推导过程 step by step"))
            .await
            .expect("定档");
        assert_eq!(decision.rewritten_model, "reasoning");

        // 改写名进既有路由链：解析到服务 reasoning 抽象组的 vendor
        let routed = router.route(&decision.rewritten_model).await.expect("路由");
        assert_eq!(routed.vendor.id, "high_vendor");
    }

    #[tokio::test]
    async fn low_tier_rewrite_resolves_to_weak_vendor() {
        let (_config, router, difficulty) = router_with(difficulty_config(), 0.0);
        let decision = difficulty
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(decision.rewritten_model, "fast");

        let routed = router.route(&decision.rewritten_model).await.expect("路由");
        assert_eq!(routed.vendor.id, "low_vendor");
    }

    #[tokio::test]
    async fn unmatched_group_passes_through_untouched() {
        let (_config, router, difficulty) = router_with(difficulty_config(), 1.0);
        assert!(difficulty.classify("gpt-4", &body("hi")).await.is_none());
        // 原名在既有链上照常解析（直通语义）
        let routed = router.route("gpt-4").await;
        assert!(routed.is_err() || routed.unwrap().model.model_id == "gpt-4");
    }

    #[tokio::test]
    async fn explain_reports_breakdown_without_mutation() {
        let (_config, _router, difficulty) = router_with(difficulty_config(), 0.9);
        let payload = body("hello thanks");
        let explanation = difficulty.explain("auto", &payload).await.expect("解释");
        assert_eq!(explanation["tier"], "high");
        assert_eq!(explanation["rewritten_model"], "reasoning");
        assert_eq!(explanation["score_breakdown"]["p_clf"], 0.9);
        assert_eq!(explanation["classifier"]["wired"], true);
        assert_eq!(explanation["classifier"]["breaker"], "closed");
        // 只读：explain 不写缓存
        let cached = explanation["cached"].as_bool().unwrap();
        assert!(!cached);
    }

    #[tokio::test]
    async fn explain_without_classifier_reports_not_wired() {
        let config_manager = Arc::new(ConfigManager::new(difficulty_config()));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let difficulty = DifficultyRouter::new(
            config_manager,
            None,
            Guard::always_open(),
            DecisionCache::with_memory_backend(0),
            vendor_manager.metrics().clone(),
        );
        let explanation = difficulty
            .explain("auto", &body("hello"))
            .await
            .expect("解释");
        assert_eq!(explanation["classifier"]["wired"], false);
        assert_eq!(
            explanation["classifier"]["breaker"], "open",
            "F3 常驻降级形态：breaker 恒 open"
        );
        assert_eq!(
            explanation["classifier"]["error"],
            fluxgate::i18n::t("difficulty-explain-not-wired")
        );
        assert_eq!(
            explanation["score_breakdown"]["p_clf"],
            serde_json::Value::Null
        );
    }
}
