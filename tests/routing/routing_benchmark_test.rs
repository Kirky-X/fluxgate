// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 难度定档路由 benchmark（纯仿真，零 API key / 零出站）：
//! 六家主流厂商 × 17 个大小模型配成 fast/balanced/reasoning 三档，
//! 一组人工标注难度的 prompt 走「难度定档 → 抽象名改写 → 带内选厂商」
//! 全链。正确性断言两条：
//! ① 定档档位与人工标注一致（规则引擎，w_clf=0 纯规则形态）；
//! ② 最终路由决策必须落在该档候选集内（vendor ∈ 档位厂商集，
//!    model.abstract_id == 档位抽象名）。
//! 带 `--nocapture` 运行可看 [route-debug] 逐案报告与档位分布表。

#![cfg(feature = "router")]

#[path = "../common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use nexusflow::config::{
        Config, ConfigManager, DatabaseConfig, DifficultySection, DifficultyTiers, ModelConfig,
        ServerConfig, Tier,
    };
    use nexusflow::router::difficulty::{DecisionCache, DifficultyRouter, Guard};
    use nexusflow::router::Router;
    use nexusflow::vendor::VendorManager;
    use serde_json::json;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Arc;

    // ---- 模型阵容：六家主流厂商，大小模型混合，abstract_id 即档位标签 ----

    fn model(id: &str, tier: &str, weight: f64) -> ModelConfig {
        ModelConfig {
            model_id: id.to_string(),
            abstract_id: Some(tier.to_string()),
            weight,
            enabled: true,
        }
    }

    fn benchmark_config() -> Config {
        let mut openai = common::base_vendor("openai", String::new());
        openai.models = vec![
            model("gpt-5", "reasoning", 2.0),
            model("gpt-4.1", "balanced", 1.5),
            model("gpt-4.1-mini", "fast", 2.0),
        ];
        let mut anthropic = common::base_vendor("anthropic", String::new());
        anthropic.type_ = "anthropic".into();
        anthropic.models = vec![
            model("claude-opus-4-5", "reasoning", 1.5),
            model("claude-sonnet-4-5", "balanced", 1.5),
            model("claude-haiku-4-5", "fast", 1.5),
        ];
        let mut google = common::base_vendor("google", String::new());
        google.models = vec![
            model("gemini-2.5-pro", "reasoning", 1.5),
            model("gemini-2.5-flash", "fast", 2.0),
        ];
        let mut zhipu = common::base_vendor("zhipu", String::new());
        zhipu.models = vec![
            model("glm-4.6", "reasoning", 1.5),
            model("glm-4.5-air", "fast", 1.0),
        ];
        let mut deepseek = common::base_vendor("deepseek", String::new());
        deepseek.models = vec![
            model("deepseek-r1", "reasoning", 1.5),
            model("deepseek-v3.2", "balanced", 2.0),
        ];
        let mut qwen = common::base_vendor("qwen", String::new());
        qwen.models = vec![
            model("qwen3-max", "reasoning", 1.0),
            model("qwen3-turbo", "fast", 2.0),
        ];

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
            ..common::test_config(vec![openai, anthropic, google, zhipu, deepseek, qwen])
        };
        config.routing.difficulty = DifficultySection {
            enabled: true,
            groups: vec!["auto".to_string()],
            w_clf: 0.0, // 纯规则形态：不依赖 vecboost 引擎与任何模型文件
            w_rule: 1.0,
            score_low: 0.35,
            score_high: 0.65,
            mid_band_policy: "strong".to_string(),
            tiers: DifficultyTiers {
                low: "fast".to_string(),
                mid: Some("balanced".to_string()),
                high: "reasoning".to_string(),
            },
            classifier: Default::default(),
        };
        config
    }

    fn harness() -> (Arc<Router>, DifficultyRouter, Arc<ConfigManager>) {
        let config_manager = Arc::new(ConfigManager::new(benchmark_config()));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        let router = Arc::new(Router::new(config_manager.clone(), vendor_manager.clone()));
        let difficulty = DifficultyRouter::new(
            config_manager.clone(),
            None, // 纯规则：零 API key、零模型文件
            Guard::new(5, 30),
            DecisionCache::with_memory_backend(0), // 关缓存：逐案独立评分，便于看真实分布
            vendor_manager.metrics().clone(),
        );
        (router, difficulty, config_manager)
    }

    fn tier_vendors(config: &Config, tier_abstract: &str) -> HashSet<String> {
        config
            .vendors
            .iter()
            .filter(|v| {
                v.enabled
                    && v.models.iter().any(|m| {
                        m.enabled
                            && m.abstract_id
                                .as_deref()
                                .is_some_and(|a| a.eq_ignore_ascii_case(tier_abstract))
                    })
            })
            .map(|v| v.id.clone())
            .collect()
    }

    fn body(prompt: &str) -> serde_json::Value {
        json!({"model": "auto", "messages": [{"role": "user", "content": prompt}]})
    }

    // ---- benchmark 用例：人工标注难度（意图带），规则引擎应落在同档 ----

    fn cases() -> Vec<(&'static str, &'static str, Tier)> {
        vec![
            // ---- 简单档：寒暄/翻译/摘要/致谢，零 hard 信号 ----
            ("寒暄", "你好呀，今天天气不错", Tier::Low),
            ("翻译", "帮我把「谢谢」翻译成英文", Tier::Low),
            ("摘要", "帮我总结一下这段话的中心思想", Tier::Low),
            ("致谢", "hello, thanks a lot!", Tier::Low),
            // ---- 中间档：有推理/调试信号但非深水区（r 落 0.35~0.65）----
            (
                "代码调试",
                "为什么这段并发代码会出现 race condition？帮我 debug 并分析 root cause，然后给出重构方案",
                Tier::Mid,
            ),
            (
                "慢查询",
                "```text\nslow query\n```\n这个递归算法的复杂度为什么高？如何调优",
                Tier::Mid,
            ),
            (
                "原理提问",
                "解释一下数据库索引的原理，为什么它能加速查询？涉及哪些数据结构与算法，高并发下还成立吗",
                Tier::Mid,
            ),
            // ---- 困难档：证明/推导 + 代码/公式 + 多问（r ≥ 0.65）----
            (
                "数学证明",
                "请证明这个积分方程并给出完整推导，step by step：f(x)=∫g(t)dt≥0，为什么收敛？？？\n```math\nT(n)=2T(n/2)+n≤c·n·logn\n```",
                Tier::High,
            ),
            (
                "算法深水区",
                "请证明 P≠NP 的任何证明尝试为什么失败？给出复杂度下界的严格推导，step by step 分析递归与并发两种算法的矩阵运算优化，附代码：```python\nwhile True: pass\n``` ？？？",
                Tier::High,
            ),
            (
                "积分求解",
                "求解以下积分方程并证明收敛性：∫∫f(x,y)dxdy ≤ M，其中矩阵 A^2=A+I，为什么？给出算法复杂度推导 ```\nO(n^2)\n``` ？？？？",
                Tier::High,
            ),
        ]
    }

    #[tokio::test]
    async fn difficulty_routing_benchmark() {
        let (router, difficulty, config_manager) = harness();
        let config = config_manager.read();

        println!(
            "\n[route-debug] ===== 定档基准：{} 个用例（纯规则 w_rule=1.0，阈值 {:.2}/{:.2}）=====",
            cases().len(),
            config.routing.difficulty.score_low,
            config.routing.difficulty.score_high
        );

        let mut pass = 0usize;
        let mut fail = 0usize;
        for (name, prompt, want) in cases() {
            let decision = difficulty
                .classify("auto", &body(prompt))
                .await
                .expect("groups 命中，必出档");
            let ok = decision.tier == want;
            if ok {
                pass += 1
            } else {
                fail += 1
            }
            println!(
                "[route-debug] {} {:<12} r={:.3} 定档={:<5} 期望={:<5} 改写→{}",
                if ok { "PASS" } else { "FAIL" },
                name,
                decision.score,
                tier_name(decision.tier),
                tier_name(want),
                decision.rewritten_model
            );
            assert_eq!(
                decision.tier, want,
                "用例「{name}」定档不符：r={:.3}",
                decision.score
            );

            // 改写名进既有路由链：带内选厂商，20 次采样全部必须落在该档候选集
            let allowed = tier_vendors(&config, &decision.rewritten_model);
            let mut dist: BTreeMap<String, usize> = BTreeMap::new();
            for _ in 0..20 {
                let routed = router
                    .route(&decision.rewritten_model)
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "用例「{name}」改写名 {} 路由失败: {e}",
                            decision.rewritten_model
                        )
                    });
                assert!(
                    allowed.contains(&routed.vendor.id),
                    "用例「{name}」路由越档：vendor={} 不在 {:?}",
                    routed.vendor.id,
                    allowed
                );
                assert_eq!(
                    routed.model.abstract_id.as_deref(),
                    Some(decision.rewritten_model.as_str()),
                    "用例「{name}」路由到的模型档位不符"
                );
                *dist
                    .entry(format!("{}/{}", routed.vendor.id, routed.model.model_id))
                    .or_default() += 1;
            }
            println!(
                "[route-debug]   └ 20 次带内采样分布: {:?}（候选厂商 {:?}）",
                dist, allowed
            );
        }
        println!("[route-debug] ===== 结果: {pass} PASS / {fail} FAIL =====\n");
        assert_eq!(fail, 0, "存在定档不符用例");
    }

    #[tokio::test]
    async fn tier_distribution_is_cross_vendor() {
        // 三档各 60 次采样：验证加权轮盘在带内多厂商间真实分散（非固定单点）
        let (router, _difficulty, _config_manager) = harness();
        for (abstract_name, min_vendors_hit) in
            [("fast", 3usize), ("balanced", 2), ("reasoning", 3)]
        {
            let mut dist: BTreeMap<String, usize> = BTreeMap::new();
            for _ in 0..60 {
                let routed = router.route(abstract_name).await.expect("路由成功");
                *dist.entry(routed.vendor.id).or_default() += 1;
            }
            println!("[route-debug] 档位 {abstract_name}: 60 次采样分布 {dist:?}");
            assert!(
                dist.len() >= min_vendors_hit,
                "档位 {abstract_name} 60 次采样只落在 {} 家厂商（期望 ≥{min_vendors_hit}）：{dist:?}",
                dist.len()
            );
        }
    }

    #[tokio::test]
    async fn direct_and_case_insensitive_routing() {
        let (router, _difficulty, _config) = harness();
        // vendor.model 直连：跳过选择直接命中
        let d = router.route("deepseek.deepseek-r1").await.expect("直连");
        assert_eq!(d.vendor.id, "deepseek");
        assert_eq!(d.model.model_id, "deepseek-r1");
        // 书写大小写不敏感
        let d = router.route("DeepSeek.DeepSeek-R1").await.expect("CI 直连");
        assert_eq!(d.vendor.id, "deepseek");
        assert_eq!(d.model.model_id, "deepseek-r1");
        // 抽象名直呼（不经定档）
        let d = router.route("reasoning").await.expect("抽象名");
        assert_eq!(d.model.abstract_id.as_deref(), Some("reasoning"));
    }

    fn tier_name(tier: Tier) -> &'static str {
        match tier {
            Tier::Low => "low",
            Tier::Mid => "mid",
            Tier::High => "high",
        }
    }
}
