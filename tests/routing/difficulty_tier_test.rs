// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

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
        Config, ConfigManager, DatabaseConfig, DifficultyScoringSection, DifficultySection,
        DifficultyTiers, ModelConfig, ServerConfig, Tier,
    };
    use fluxgate::router::difficulty::{
        DecisionCache, DifficultyRouter, Guard, MockDifficultyClassifier, TierPath,
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

            dimensions: None,
            compat_group: None,
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
            ..DifficultySection::default()
        };
        config
    }

    fn hot_reload_config() -> Config {
        let mut config = difficulty_config();
        config.routing.difficulty.w_clf = 0.0;
        config.routing.difficulty.w_rule = 1.0;
        config.routing.difficulty.scoring = DifficultyScoringSection {
            w_hard: 1.0,
            w_code_fence: 0.0,
            w_math: 0.0,
            w_questions: 0.0,
            w_messages: 0.0,
            w_tools: 0.0,
            w_length: 0.0,
            w_easy_penalty: 0.0,
            ..Default::default()
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
    async fn lexicon_and_threshold_hot_reload_take_effect_immediately() {
        let (manager, _router, difficulty) = router_with(hot_reload_config(), 0.0);

        let high_payload = body("证明 推导 求解 算法");
        let first = difficulty
            .classify("auto", &high_payload)
            .await
            .expect("首次定档");
        assert_eq!(first.tier, Tier::High);

        // 词表热更：四个 hard 词被替换为不存在词 → 命中归零，立即降档
        let mut next = manager.get_config();
        next.routing.difficulty.lexicon.hard = Some(vec!["不存在的词".to_string()]);
        manager.swap_memory(next).expect("词表热更");
        let second = difficulty
            .classify("auto", &high_payload)
            .await
            .expect("热更后定档");
        assert_ne!(second.path, TierPath::CacheHit, "词表变化必须换特征键");
        assert_eq!(second.tier, Tier::Low);

        // 同一词表/同一文本/同一命中数，只改阈值：验证配置指纹换键
        let one_hit = body("不存在的词");
        let before = difficulty
            .classify("auto", &one_hit)
            .await
            .expect("阈值前定档");
        assert_eq!(before.tier, Tier::Low);

        let mut next = manager.get_config();
        next.routing.difficulty.score_low = 0.1;
        next.routing.difficulty.score_high = 0.15;
        manager.swap_memory(next).expect("阈值热更");
        let after = difficulty
            .classify("auto", &one_hit)
            .await
            .expect("阈值后定档");
        assert_ne!(after.path, TierPath::CacheHit, "阈值变化必须换配置指纹");
        assert_eq!(after.tier, Tier::High);
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
