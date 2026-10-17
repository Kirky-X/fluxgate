// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 定档编排器：旁路判定（F1/F13）→ 特征 → 缓存 → 熔断内分类（F5/F6）→
//! 融合切分（R-001/002）→ 档位改写。全部降级显性化：路径随
//! [`TierDecision::path`] 返回，事件经 `tracing` 输出；本层不产生
//! HTTP 错误——定档永不导致请求失败（spec difficulty-degradation 约束）。

use crate::config::{ConfigManager, Tier};
use crate::i18n::{t, t_args};
use crate::utils::metrics::GlobalMetrics;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

use super::cache::{CachedDecision, DecisionCache};
use super::classifier::DifficultyClassifier;
use super::features::{extract_features, Lexicon, RuleFeatures};
use super::fusion::{decide_band, fuse, parse_mid_band_policy, rule_score, MidBandPolicy};
use super::guard::{Availability, Guard};

/// 定档路径（观测面：计数与 X-Difficulty-Tier 的来源）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierPath {
    /// 融合：分类概率 + 规则
    Fusion,
    /// 纯规则：分类器未接线（F3）/熔断 open（F6）/单次调用失败（F5）
    RuleOnly,
    /// 特征 hash 命中缓存
    CacheHit,
}

/// 一次定档决策。`p_clf` 仅 Fusion 路径有值；`r_rule` 恒有（缓存命中
/// 取缓存值）
#[derive(Debug, Clone, PartialEq)]
pub struct TierDecision {
    pub tier: Tier,
    /// 档位映射的抽象模型名（改写目标）
    pub rewritten_model: String,
    pub score: f64,
    pub p_clf: Option<f64>,
    pub r_rule: f64,
    pub path: TierPath,
}

/// 两协议入口共用的定档前置（design §5 统一入口）：`router = None`（未
/// 接线）或 `bypass`（openai 侧 X-Vendor/X-Model 显式 hints 直连，语义同
/// 亲和）时原样直通 `model`；命中 groups 则返回 (档位标签, 改写后模型名)。
/// 改写仅作用于路由解析，请求 body 不动
pub async fn resolve_tier(
    router: Option<&DifficultyRouter>,
    model: &str,
    body: &Value,
    bypass: bool,
) -> (Option<&'static str>, String) {
    let Some(router) = router.filter(|_| !bypass) else {
        return (None, model.to_string());
    };
    match router.classify(model, body).await {
        Some(tier_decision) => (
            Some(crate::router::difficulty::tier_label(tier_decision.tier)),
            tier_decision.rewritten_model,
        ),
        None => (None, model.to_string()),
    }
}

/// 定档路由器。`classifier = None` 即纯规则形态（feature 未接线 / F3 降级）
pub struct DifficultyRouter {
    config: Arc<ConfigManager>,
    classifier: Option<Arc<dyn DifficultyClassifier>>,
    guard: Arc<Guard>,
    cache: DecisionCache,
    metrics: Arc<GlobalMetrics>,
}

impl DifficultyRouter {
    pub fn new(
        config: Arc<ConfigManager>,
        classifier: Option<Arc<dyn DifficultyClassifier>>,
        guard: Guard,
        cache: DecisionCache,
        metrics: Arc<GlobalMetrics>,
    ) -> Self {
        Self {
            config,
            classifier,
            guard: Arc::new(guard),
            cache,
            metrics,
        }
    }

    /// 定档主入口。返回 None = 不定档（未启用 / groups 未命中），调用方
    /// 原样直通请求模型名；Some = 请求方 model 应改写为 `rewritten_model`
    pub async fn classify(&self, model: &str, body: &Value) -> Option<TierDecision> {
        // 单一配置快照：阈值/权重/词法表/档位名同代读取，热更换代不会撕裂
        let config = self.config.read();
        let d = &config.routing.difficulty;
        if !d.enabled {
            return None;
        }
        // F13：groups 在快照内借用匹配，未命中（多数流量）零克隆
        let normalized = model.trim().to_ascii_lowercase();
        if !d
            .groups
            .iter()
            .any(|g| g.trim().to_ascii_lowercase() == normalized)
        {
            return None;
        }
        // 熔断参数热读（spec difficulty-degradation Constraints）
        self.guard.update_params(
            d.classifier.failure_threshold,
            d.classifier.probe_interval_secs,
        );

        let started = Instant::now();
        let features = extract_features(
            body,
            Lexicon::new(
                d.lexicon.hard_words(),
                d.lexicon.easy_words(),
                d.lexicon.math_symbols(),
                d.lexicon.question_marks(),
                d.lexicon.code_fence_markers(),
            ),
        );
        if body.as_object().is_none() {
            // F7：畸形 body → 中性特征续算（显性计数）
            self.metrics.record_difficulty_features_degraded();
        }
        let key = DecisionCache::key(&features, DecisionCache::decision_config_fingerprint(d));

        if let Some(cached) = self.cache.get(&key).await {
            self.metrics
                .record_difficulty_decision(false, true, cached.tier, started.elapsed());
            return Some(TierDecision {
                tier: cached.tier,
                rewritten_model: Self::tier_name_of(&d.tiers, cached.tier),
                score: cached.score,
                p_clf: None,
                r_rule: cached.r_rule,
                path: TierPath::CacheHit,
            });
        }

        let r = rule_score(&features, &d.scoring);

        // 分类调用：熔断放行才调（F5/F6）；spawn_blocking 包住阻塞推理，
        // timeout 兜住挂死。失败/超时 → 本次退纯规则 + 计失败
        let p_clf = self
            .classify_with_guard(&features, d.w_clf, d.classifier.timeout_ms)
            .await;

        // 无概率（未接线/降级）→ w_clf 记 0：S 退化为 r（纯规则）
        let score = match p_clf {
            Some(p) => fuse(d.w_clf, p, d.w_rule, r),
            None => fuse(0.0, 0.0, d.w_rule, r),
        };
        let tier = decide_band(
            score,
            d.score_low,
            d.score_high,
            d.tiers.mid.is_some(),
            parse_mid_band_policy(&d.mid_band_policy).unwrap_or(MidBandPolicy::Strong),
        );

        self.cache
            .put(
                &key,
                &CachedDecision {
                    tier,
                    score,
                    r_rule: r,
                },
            )
            .await;

        self.metrics
            .record_difficulty_decision(p_clf.is_some(), false, tier, started.elapsed());

        Some(TierDecision {
            tier,
            rewritten_model: Self::tier_name_of(&d.tiers, tier),
            score,
            p_clf,
            r_rule: r,
            path: match p_clf {
                Some(_) => TierPath::Fusion,
                None => TierPath::RuleOnly,
            },
        })
    }

    /// 失败记账单点：熔断迁移 + 指标计数（F5/F6）
    fn record_failure_tracked(&self, now: Instant) -> bool {
        let opened = self.guard.record_failure(now);
        if opened {
            self.metrics.record_difficulty_breaker_opened();
        }
        opened
    }

    /// 熔断内分类调用。返回 None = 本次无概率（含 F3 未接线 / Unavailable 拒绝）
    async fn classify_with_guard(
        &self,
        features: &RuleFeatures,
        w_clf: f64,
        timeout_ms: u64,
    ) -> Option<f64> {
        let classifier = self.classifier.clone()?;
        if w_clf <= 0.0 || !w_clf.is_finite() {
            return None;
        }
        let now = Instant::now();
        match self.guard.check(now) {
            Availability::Unavailable => None,
            Availability::Available | Availability::Probe => {
                let features = features.clone();
                let result = timeout(
                    Duration::from_millis(timeout_ms.max(1)),
                    tokio::task::spawn_blocking(move || classifier.p_high(&features)),
                )
                .await;
                match result {
                    Ok(Ok(Ok(p))) => {
                        let recovered = self.guard.record_success();
                        if recovered {
                            self.metrics.record_difficulty_breaker_recovered();
                            tracing::info!("{}", t("difficulty-classifier-recovered"));
                        }
                        if p.is_finite() && (0.0..=1.0).contains(&p) {
                            Some(p)
                        } else {
                            // 越界概率按 InvalidOutput 语义计失败（F5）
                            self.record_failure_tracked(Instant::now());
                            self.metrics.record_difficulty_classifier_failed();
                            None
                        }
                    }
                    Ok(Ok(Err(error))) => {
                        self.record_failure_tracked(Instant::now());
                        self.metrics.record_difficulty_classifier_failed();
                        tracing::debug!(?error, "{}", t("difficulty-classifier-failed"));
                        None
                    }
                    Ok(Err(join_error)) => {
                        self.record_failure_tracked(Instant::now());
                        self.metrics.record_difficulty_classifier_failed();
                        tracing::debug!(
                            ?join_error,
                            "{}",
                            t("difficulty-classifier-task-panicked")
                        );
                        None
                    }
                    Err(_elapsed) => {
                        self.record_failure_tracked(Instant::now());
                        self.metrics.record_difficulty_classifier_failed();
                        tracing::debug!("{}", t("difficulty-classifier-timeout"));
                        None
                    }
                }
            }
        }
    }

    /// 档位 → 抽象模型名（热路径复用已持有的 config 快照，免二次读）
    fn tier_name_of(tiers: &crate::config::DifficultyTiers, tier: Tier) -> String {
        match tier {
            Tier::Low => tiers.low.clone(),
            Tier::Mid => tiers.mid.clone().unwrap_or_else(|| tiers.high.clone()),
            Tier::High => tiers.high.clone(),
        }
    }

    /// 定档解释（`POST /admin/route/difficulty-explain` 数据源）：S 分解 +
    /// 命中档 + 路径 + 熔断状态。旁路语义同 [`Self::classify`]（未启用/
    /// groups 未命中 → None）。只读观测：不写缓存、不改熔断状态机、分类
    /// 推理为纯读（仅熔断放行时咨询，失败不记账仅报 error 字段）
    pub async fn explain(&self, model: &str, body: &Value) -> Option<serde_json::Value> {
        let config = self.config.read();
        let d = &config.routing.difficulty;
        if !d.enabled {
            return None;
        }
        let normalized = model.trim().to_ascii_lowercase();
        if !d
            .groups
            .iter()
            .any(|g| g.trim().to_ascii_lowercase() == normalized)
        {
            return None;
        }

        let started = Instant::now();
        let features = extract_features(
            body,
            Lexicon::new(
                d.lexicon.hard_words(),
                d.lexicon.easy_words(),
                d.lexicon.math_symbols(),
                d.lexicon.question_marks(),
                d.lexicon.code_fence_markers(),
            ),
        );
        let key = DecisionCache::key(&features, DecisionCache::decision_config_fingerprint(d));
        let cached = self.cache.get(&key).await;

        let r = rule_score(&features, &d.scoring);
        let availability = self.guard.check(Instant::now());
        let (p_clf, classifier_error) = match (&self.classifier, availability) {
            (Some(classifier), Availability::Available | Availability::Probe)
                if d.w_clf > 0.0 && d.w_clf.is_finite() =>
            {
                let classifier = classifier.clone();
                let features = features.clone();
                match timeout(
                    Duration::from_millis(d.classifier.timeout_ms.max(1)),
                    tokio::task::spawn_blocking(move || classifier.p_high(&features)),
                )
                .await
                {
                    Ok(Ok(Ok(p))) if p.is_finite() && (0.0..=1.0).contains(&p) => (Some(p), None),
                    Ok(Ok(Ok(p))) => (
                        None,
                        Some(t_args(
                            "difficulty-explain-invalid-probability",
                            &[("p", p.to_string())],
                        )),
                    ),
                    // $error 为 ClassifierError Debug 透传：Engine 载荷本身已本地化
                    //（vecboost_engine.rs 各失败分支），此处保持裸透传不新增文案。
                    Ok(Ok(Err(error))) => (None, Some(format!("{error:?}"))),
                    Ok(Err(join_error)) => (
                        None,
                        Some(t_args(
                            "difficulty-explain-task-panicked",
                            &[("join_error", join_error.to_string())],
                        )),
                    ),
                    Err(_) => (None, Some(t("difficulty-explain-timeout"))),
                }
            }
            (None, _) => (None, Some(t("difficulty-explain-not-wired"))),
            (_, Availability::Unavailable) => (None, Some(t("difficulty-explain-breaker-open"))),
            // 熔断 open 期不咨询（Available 已被上面带 guard 的臂消费；
            // 此兜底覆盖 w_clf=0 等剩余组合）
            (_, _) => (None, Some(t("difficulty-explain-not-consulted"))),
        };

        let score = match p_clf {
            Some(p) => fuse(d.w_clf, p, d.w_rule, r),
            None => fuse(0.0, 0.0, d.w_rule, r),
        };
        let tier = decide_band(
            score,
            d.score_low,
            d.score_high,
            d.tiers.mid.is_some(),
            parse_mid_band_policy(&d.mid_band_policy).unwrap_or(MidBandPolicy::Strong),
        );

        Some(serde_json::json!({
            "model_input": model,
            "tier": crate::router::difficulty::tier_label(tier),
            "rewritten_model": Self::tier_name_of(&d.tiers, tier),
            "score": score,
            "score_breakdown": {
                "w_clf": d.w_clf,
                "w_rule": d.w_rule,
                "p_clf": p_clf,
                "r_rule": r,
                "formula": "S = (w_clf·p + w_rule·r) / (w_clf + w_rule)",
            },
            "thresholds": {
                "score_low": d.score_low,
                "score_high": d.score_high,
                "mid_band_policy": d.mid_band_policy.clone(),
                "mid_configured": d.tiers.mid.is_some(),
            },
            "cached": cached.is_some(),
            "cached_decision": cached.as_ref().map(|c| serde_json::json!({
                "tier": crate::router::difficulty::tier_label(c.tier),
                "score": c.score,
                "r_rule": c.r_rule,
            })),
            "classifier": {
                "wired": self.classifier.is_some(),
                "breaker": match availability {
                    Availability::Available => "closed",
                    Availability::Probe => "half_open",
                    Availability::Unavailable => "open",
                },
                "error": classifier_error,
            },
            "explain_latency_ms": started.elapsed().as_millis() as u64,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DatabaseConfig, DifficultySection, DifficultyTiers, ServerConfig};
    use crate::router::difficulty::classifier::{ClassifierError, MockDifficultyClassifier};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn body(text: &str) -> Value {
        json!({"messages": [{"role": "user", "content": text}]})
    }

    fn base_config() -> Config {
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: vec![],
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
        }
    }

    fn config_enabled(w_clf: f64, mid: Option<&str>) -> Config {
        let mut config = base_config();
        config.routing.difficulty = DifficultySection {
            enabled: true,
            groups: vec!["auto".to_string()],
            w_clf,
            w_rule: 0.4,
            score_low: 0.35,
            score_high: 0.65,
            mid_band_policy: "strong".to_string(),
            tiers: DifficultyTiers {
                low: "fast".to_string(),
                mid: mid.map(str::to_string),
                high: "reasoning".to_string(),
            },
            classifier: Default::default(),
            ..DifficultySection::default()
        };
        config
    }

    fn router(
        config: Config,
        classifier: Option<Arc<dyn DifficultyClassifier>>,
    ) -> DifficultyRouter {
        DifficultyRouter::new(
            Arc::new(ConfigManager::new(config)),
            classifier,
            Guard::new(3, 30),
            DecisionCache::with_memory_backend(300),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        )
    }

    #[tokio::test]
    async fn disabled_or_unmatched_group_returns_none() {
        let mut config = config_enabled(0.0, None);
        config.routing.difficulty.enabled = false;
        let r = router(config, None);
        assert!(r.classify("auto", &body("hi")).await.is_none(), "F1 旁路");

        let r = router(config_enabled(0.0, None), None);
        assert!(
            r.classify("gpt-x", &body("hi")).await.is_none(),
            "F13 groups 未命中直通"
        );
    }

    #[tokio::test]
    async fn fusion_path_high_when_classifier_confident() {
        // p=1.0 → S = 0.6·1 + 0.4·r ≥ 0.6 > 0.65？不——r 大时才过。
        // 用强规则文本抬 r：S 必过 0.65
        let r = router(
            config_enabled(0.6, None),
            Some(Arc::new(MockDifficultyClassifier::ok(1.0))),
        );
        let d = r
            .classify(
                "auto",
                &body("请证明这个定理，推导过程加算法复杂度分析，step by step"),
            )
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::High);
        assert_eq!(d.rewritten_model, "reasoning");
        assert_eq!(d.path, TierPath::Fusion);
        assert_eq!(d.p_clf, Some(1.0));
    }

    #[tokio::test]
    async fn rule_only_when_classifier_absent_or_failing() {
        // F3 形态：无分类器 → S = r；强 hard 文本 r ≈0.44 → High
        let r = router(config_enabled(0.0, None), None);
        let d = r
            .classify(
                "auto",
                &body("请证明这个定理并给出推导过程：a*x^2 + b = 0 的积分与矩阵方程？递归并发死锁如何排查？step by step"),
            )
            .await
            .expect("定档");
        assert_eq!(d.path, TierPath::RuleOnly);
        assert_eq!(d.p_clf, None);
        assert_eq!(d.tier, Tier::High);
        assert_eq!(d.rewritten_model, "reasoning");

        // F5 形态：分类器报错 → 该请求退纯规则
        let mock = MockDifficultyClassifier::new(vec![Err(ClassifierError::Engine("boom".into()))]);
        let r = router(config_enabled(0.0, None), Some(Arc::new(mock)));
        let d = r.classify("auto", &body("hello")).await.expect("定档");
        assert_eq!(d.path, TierPath::RuleOnly);
    }

    #[tokio::test]
    async fn mid_tier_and_mid_absent_policy() {
        // p=0.9, easy 文本 r≈0 → S≈0.54 ∈ (0.35, 0.65) → mid 区间
        let r = router(
            config_enabled(0.6, Some("balanced")),
            Some(Arc::new(MockDifficultyClassifier::ok(0.9))),
        );
        let d = r
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::Mid);
        assert_eq!(d.rewritten_model, "balanced");

        // mid 未配置 + strong 策略：同分落 High
        let r = router(
            config_enabled(0.6, None),
            Some(Arc::new(MockDifficultyClassifier::ok(0.9))),
        );
        let d = r
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::High);
    }

    #[tokio::test]
    async fn mid_absent_cheap_and_nearest_policies_end_to_end() {
        // 同一融合分（p=0.9, r≈0 → S≈0.54 中带）：cheap → Low，nearest → High
        //（0.54 距上阈 0.11 < 距下阈 0.19）
        let make = |policy: &str| {
            let mut config = config_enabled(0.6, None);
            config.routing.difficulty.mid_band_policy = policy.to_string();
            router(config, Some(Arc::new(MockDifficultyClassifier::ok(0.9))))
        };
        let d = make("cheap")
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::Low);
        let d = make("nearest")
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::High);
        // nearest 平局（S=0.5）：p=0.8333 → S≈0.5，取 high（钉死语义）
        let mut config = config_enabled(0.6, None);
        config.routing.difficulty.mid_band_policy = "nearest".to_string();
        let r = router(
            config,
            Some(Arc::new(MockDifficultyClassifier::ok(0.5 / 0.6))),
        );
        let d = r
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.tier, Tier::High, "nearest 平局取 high");
    }

    #[tokio::test]
    async fn metrics_counters_track_paths() {
        let metrics = Arc::new(crate::utils::metrics::GlobalMetrics::new());
        let mut config = config_enabled(0.6, None);
        config.routing.difficulty.classifier.failure_threshold = 2;
        let mock = Arc::new(MockDifficultyClassifier::new(vec![
            Err(ClassifierError::Timeout),
            Err(ClassifierError::Timeout),
            Ok(1.0),
        ]));
        let r = DifficultyRouter::new(
            Arc::new(ConfigManager::new(config)),
            Some(mock),
            Guard::new(2, 3600),
            DecisionCache::with_memory_backend(300),
            metrics.clone(),
        );
        // 两次失败（F5/F6: rule_only ×2，第 2 次触发 open）+ open 期直连
        for i in 0..3 {
            r.classify("auto", &body(&format!("metric-case-{i}")))
                .await
                .expect("定档");
        }
        let snap = metrics.snapshot();
        assert_eq!(snap.difficulty.rule_only, 3, "三笔全走纯规则");
        assert_eq!(snap.difficulty.fusion, 0);
        assert_eq!(snap.difficulty.classifier_failed, 2, "两次分类失败计数");
        assert_eq!(snap.difficulty.breaker_opened, 1, "阈值触发 →open 计数");
        assert!(snap.difficulty.avg_latency_ms >= 0.0);
    }

    #[tokio::test]
    async fn invalid_probability_degrades_to_rule_only_and_counts() {
        // Ok(1.5) 越界 = InvalidOutput 语义（F5）：该请求退纯规则 + 计失败
        let mock = Arc::new(MockDifficultyClassifier::ok(1.5));
        let metrics = Arc::new(crate::utils::metrics::GlobalMetrics::new());
        let r = DifficultyRouter::new(
            Arc::new(ConfigManager::new(config_enabled(0.6, None))),
            Some(mock),
            Guard::new(5, 30),
            DecisionCache::with_memory_backend(300),
            metrics.clone(),
        );
        let d = r
            .classify("auto", &body("hello thanks"))
            .await
            .expect("定档");
        assert_eq!(d.path, TierPath::RuleOnly);
        assert_eq!(d.p_clf, None);
        assert_eq!(metrics.snapshot().difficulty.classifier_failed, 1);
    }

    #[tokio::test]
    async fn malformed_body_counts_features_degraded() {
        // F7：body 非对象 → 中性特征续算 + features_degraded 计数
        let metrics = Arc::new(crate::utils::metrics::GlobalMetrics::new());
        let r = DifficultyRouter::new(
            Arc::new(ConfigManager::new(config_enabled(0.0, None))),
            None,
            Guard::new(5, 30),
            DecisionCache::with_memory_backend(300),
            metrics.clone(),
        );
        let d = r
            .classify("auto", &serde_json::Value::Null)
            .await
            .expect("定档");
        assert_eq!(d.path, TierPath::RuleOnly);
        let snap = metrics.snapshot().difficulty;
        assert_eq!(snap.features_degraded, 1, "畸形 body 计特征退化");
        assert_eq!(snap.rule_only, 1);
    }

    #[tokio::test]
    async fn cache_hit_short_circuits_classifier() {
        let mock = Arc::new(MockDifficultyClassifier::ok(1.0));
        let r = router(config_enabled(0.6, None), Some(mock.clone()));
        let payload = body("identical payload 请证明");
        let first = r.classify("auto", &payload).await.expect("首算");
        assert_eq!(first.path, TierPath::Fusion);
        let second = r.classify("auto", &payload).await.expect("命中");
        assert_eq!(second.path, TierPath::CacheHit);
        assert_eq!(second.tier, first.tier);
        assert_eq!(second.score, first.score, "缓存命中的分数与首算一致");
    }

    #[tokio::test]
    async fn breaker_opens_after_consecutive_failures() {
        // 阈值 3：前 3 次真实调用失败 → open；第 4 次不再调用 mock
        let script = vec![
            Err(ClassifierError::Timeout),
            Err(ClassifierError::Timeout),
            Err(ClassifierError::Timeout),
            Ok(1.0), // open 后不得被消费
        ];
        let mock = Arc::new(MockDifficultyClassifier::new(script));
        let mut config = config_enabled(0.6, None);
        // 熔断阈值走配置热读（classify 每请求刷新），测试须在配置面设 3
        config.routing.difficulty.classifier.failure_threshold = 3;
        let r = router(config, Some(mock.clone() as Arc<dyn DifficultyClassifier>));
        for i in 0..3 {
            let d = r
                .classify("auto", &body(&format!("distinct-{i} hello")))
                .await
                .expect("定档");
            assert_eq!(d.path, TierPath::RuleOnly, "第 {} 次失败降级", i + 1);
        }
        let d = r
            .classify("auto", &body("distinct-9 fresh"))
            .await
            .expect("定档");
        assert_eq!(d.path, TierPath::RuleOnly, "F6 open 期直接纯规则");
        assert_eq!(
            mock.calls.load(Ordering::Relaxed),
            3,
            "open 后不得再调用分类器"
        );
    }
}
