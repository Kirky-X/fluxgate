// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 目的：为 src/utils/metrics.rs 与 src/vendor/rate_limiter.rs 补集成测试，
//! 把公开 API 面的行为覆盖率提到接近 100%（数据库 / SDK / router_builder
//! 的缺口分别由 database_test / sse_test / rate_limit_test+admission_test
//! 覆盖，本文件不重复）。
//!
//! - metrics：GlobalMetrics 全公开 API —— 计数器、错误率、口径分道的
//!   Peak-EWMA（注入假时钟做精确数值断言）、流量滑窗份额与熵、失败率
//!   滑窗、重试守门器视图、reset。
//! - rate_limiter：ConcurrencyLimiter 包装器（acquire / acquire_weighted
//!   钳位 / in_flight / try_acquire）与 TokenBucket（满桶立即放行 /
//!   桶空真实等待）。

#[cfg(all(test, feature = "vendor-management"))]
mod metrics_coverage {
    use fluxgate::utils::GlobalMetrics;
    use std::time::{Duration, Instant};

    /// 请求计数 / 成败分类 / 平均延迟 / 错误率与 vendor 维度字段的精确值。
    #[test]
    fn record_request_updates_counters_and_vendor_fields() {
        let m = GlobalMetrics::new();
        m.record_request("openai", true, Duration::from_millis(100));
        m.record_request("openai", false, Duration::from_millis(300));
        m.record_request("openai", false, Duration::from_millis(200));

        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 3);
        assert_eq!(snap.successful_requests, 1);
        assert_eq!(snap.failed_requests, 2);
        assert!((snap.error_rate - 2.0 / 3.0).abs() < 1e-12);
        // (100+300+200)/3 = 200ms
        assert!((snap.avg_duration_ms - 200.0).abs() < 1e-9);

        let v = snap.vendors.iter().find(|v| v.vendor == "openai").unwrap();
        assert_eq!(v.requests, 3);
        assert_eq!(v.successes, 1);
        assert_eq!(v.failures, 2);
        assert!(v.last_request_at.is_some(), "最近请求时间必须记账");
        // 错误率 EWMA：成功种子 0.0 → 0.2×1+0.8×0=0.2 → 0.2×1+0.8×0.2=0.36
        assert!((v.ewma_error_rate - 0.36).abs() < 1e-9);
        assert_eq!(v.ewma_samples, 3);
        // VendorMetrics::avg_duration_ms 独立可算
        assert!((v.avg_duration_ms() - 200.0).abs() < 1e-9);
    }

    /// record_error 只记录最近错误，不得动请求计数。
    #[test]
    fn record_error_sets_last_error_without_request_count() {
        let m = GlobalMetrics::new();
        m.record_request("v", true, Duration::from_millis(10));
        m.record_error("v", "upstream 502");
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 1, "record_error 不计请求");
        assert_eq!(snap.vendors[0].last_error.as_deref(), Some("upstream 502"));
    }

    /// 无请求时错误率为 0；Default 与 new 等价（空快照）。
    #[test]
    fn empty_metrics_zero_error_rate_and_default_equals_new() {
        let m = GlobalMetrics::default();
        assert_eq!(m.error_rate(), 0.0);
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert_eq!(snap.avg_duration_ms, 0.0);
        assert!(snap.vendors.is_empty());
    }

    /// 口径分道：ttft / e2e / queue 三道各自独立种子、独立样本数；
    /// 后续 ttft 采样不动其他道。
    #[test]
    fn latency_lanes_are_independent() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_ttft("v", Duration::from_millis(50), t0);
        m.record_e2e("v", Duration::from_millis(200), t0);
        m.record_queue("v", Duration::from_millis(10), t0);

        let v = &m.snapshot().vendors[0];
        assert!((v.ewma_ttft_ms - 50.0).abs() < 1e-12);
        assert!((v.ewma_e2e_ms - 200.0).abs() < 1e-12);
        assert!((v.ewma_queue_ms - 10.0).abs() < 1e-12);
        assert_eq!((v.ttft_samples, v.e2e_samples, v.queue_samples), (1, 1, 1));

        m.record_ttft("v", Duration::from_millis(70), t0 + Duration::from_secs(60));
        let v = &m.snapshot().vendors[0];
        assert_eq!(v.e2e_samples, 1, "ttft 采样不得动 e2e 道样本数");
        assert_eq!(v.queue_samples, 1);
        assert!((v.ewma_e2e_ms - 200.0).abs() < 1e-12);
    }

    /// Peak-EWMA 时间衰减精确值：60s 间隔（τ=10s）→ α=1−e⁻⁶，
    /// ewma = 1000 − 900·e⁻⁶；ewma_latency_ms 别名与 e2e 道逐位一致。
    #[test]
    fn peak_ewma_decays_with_time_gap_and_alias_matches() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_e2e("v", Duration::from_millis(100), t0);
        m.record_e2e(
            "v",
            Duration::from_millis(1000),
            t0 + Duration::from_secs(60),
        );

        let expected = 1000.0 - 900.0 * (-6.0f64).exp();
        let e = m.vendor_ewma("v").expect("e2e 样本后必须 Some").0;
        assert!(
            (e - expected).abs() < 1e-6,
            "60s 间隔精确值: {e} vs {expected}"
        );

        let v = &m.snapshot().vendors[0];
        assert!(
            (v.ewma_latency_ms - v.ewma_e2e_ms).abs() < 1e-12,
            "兼容别名必须与 e2e 道一致"
        );
        assert!((m.vendor_ewma_e2e("v").unwrap() - expected).abs() < 1e-6);
    }

    /// τ 热同步：set_ewma_tau_ms(1000) 后 60s 间隔（60τ）旧值权重 exp(-60)≈0；
    /// τ=0 为「完全采新样本」极限。
    #[test]
    fn ewma_tau_hot_sync_changes_decay() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_e2e("v", Duration::from_millis(100), t0);

        m.set_ewma_tau_ms(1_000);
        m.record_e2e(
            "v",
            Duration::from_millis(1000),
            t0 + Duration::from_secs(60),
        );
        let e = m.vendor_ewma("v").unwrap().0;
        assert!((e - 1000.0).abs() < 1e-6, "60τ 间隔旧值权重≈0: {e}");

        m.set_ewma_tau_ms(0);
        m.record_e2e(
            "v",
            Duration::from_millis(42),
            t0 + Duration::from_secs(120),
        );
        assert!((m.vendor_ewma("v").unwrap().0 - 42.0).abs() < 1e-12);
    }

    /// vendor_ewma 以 e2e 样本判定 Some/None：纯 record_request + record_ttft
    /// 历史不算「延迟数据」（0.0 初始值不得冒充），错误率道观测不受影响。
    #[test]
    fn vendor_ewma_gated_on_e2e_samples() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_request("v", false, Duration::from_millis(10));
        m.record_ttft("v", Duration::from_millis(30), t0);

        assert!(m.vendor_ewma("v").is_none(), "无 e2e 样本必须 None");
        assert!(m.vendor_ewma_e2e("v").is_none());
        assert!((m.snapshot().vendors[0].ewma_error_rate - 1.0).abs() < 1e-12);

        m.record_e2e("v", Duration::from_millis(200), t0 + Duration::from_secs(1));
        assert!((m.vendor_ewma("v").unwrap().0 - 200.0).abs() < 1e-12);
        assert!(m.vendor_ewma("cold").is_none(), "未记录 vendor 返回 None");
    }

    /// 亲和计数：hits/misses/命中率与快照字段一致；无样本为 0.0。
    #[test]
    fn affinity_hit_rate_counts_and_snapshots() {
        let m = GlobalMetrics::new();
        assert_eq!(m.affinity_hit_rate(), 0.0);
        m.record_affinity_hit();
        m.record_affinity_hit();
        m.record_affinity_miss();
        assert!((m.affinity_hit_rate() - 2.0 / 3.0).abs() < 1e-12);
        let snap = m.snapshot();
        assert_eq!(snap.affinity_hits, 2);
        assert_eq!(snap.affinity_misses, 1);
        assert!((snap.affinity_hit_rate - 2.0 / 3.0).abs() < 1e-12);
    }

    /// 流量份额：3 路均分 → 归一熵 1.0、top1=1/3、不告警；
    /// 95/3/2 集中 → top1=0.95 且 3 vendor 活跃触发告警。
    #[test]
    fn traffic_shares_entropy_and_alert_thresholds() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for vendor in ["a", "b", "c"] {
            for _ in 0..10 {
                m.record_traffic_at(vendor, t0);
            }
        }
        let shares = m.traffic_shares();
        assert_eq!(shares.len(), 3);
        for (_, share) in &shares {
            assert!((share - 1.0 / 3.0).abs() < 1e-12);
        }
        assert!((m.top1_share() - 1.0 / 3.0).abs() < 1e-12);
        assert!((m.share_entropy() - 1.0).abs() < 1e-12);
        assert!(!m.should_alert(), "均分不告警");

        let m2 = GlobalMetrics::new();
        for _ in 0..95 {
            m2.record_traffic_at("dominant", t0);
        }
        for _ in 0..3 {
            m2.record_traffic_at("minor-a", t0);
        }
        for _ in 0..2 {
            m2.record_traffic_at("minor-b", t0);
        }
        assert!((m2.top1_share() - 0.95).abs() < 1e-12);
        assert!(m2.should_alert(), "top1>0.9 且 3 vendor 活跃必须告警");
        // 快照携带同一份额读数
        assert!((m2.snapshot().top1_share - 0.95).abs() < 1e-12);
    }

    /// 滑窗轮转：t=0 的量随全局读锚推进衰减出窗（清零语义），
    /// 新流量接管份额；traffic_window_total 为窗内总量。
    #[test]
    fn sliding_window_decays_old_traffic_and_windows_sum() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for _ in 0..10 {
            m.record_traffic_at("a", t0);
        }
        assert_eq!(m.vendor_window_total("a"), 10);
        assert_eq!(m.traffic_window_total(), 10);

        // t=65s：b 写入推进全局读锚（桶宽 6s，abs=10 跨一整圈）→ a 的
        // t=0 量全清出窗，b 独占份额
        let t65 = t0 + Duration::from_secs(65);
        for _ in 0..10 {
            m.record_traffic_at("b", t65);
        }
        let shares = m.traffic_shares();
        assert_eq!(shares.len(), 1, "a 的旧流量必须全部出窗: {shares:?}");
        assert_eq!(shares[0].0, "b");
        assert!((m.top1_share() - 1.0).abs() < 1e-12);
        assert_eq!(m.traffic_window_total(), 10, "窗内只剩 b 的 10 次");
        assert_eq!(m.vendor_window_total("nope"), 0, "未记录 vendor 为 0");
    }

    /// 首样本语义：全新 counter 首次记账只建锚并写桶 0（读数恰 1）。
    #[test]
    fn first_traffic_sample_seeds_window() {
        let m = GlobalMetrics::new();
        m.record_traffic_at("v", Instant::now());
        assert_eq!(m.vendor_window_total("v"), 1);
        assert_eq!(m.vendor_window_total("cold"), 0);
    }

    /// 失败率滑窗：total 记全部结局、failures 只记失败；旧失败随全局
    /// 时间出窗；无样本 vendor 返回 (0, 0)。
    #[test]
    fn vendor_failure_window_counts_then_decays() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            m.record_vendor_outcome_at("v", true, t0);
        }
        for _ in 0..3 {
            m.record_vendor_outcome_at("v", false, t0);
        }
        assert_eq!(m.vendor_failure_window("v"), (6, 3));
        assert_eq!(m.vendor_failure_window("cold"), (0, 0));

        for _ in 0..2 {
            m.record_vendor_outcome_at("b", true, t0 + Duration::from_secs(65));
        }
        assert_eq!(
            m.vendor_failure_window("v"),
            (0, 0),
            "旧失败必须随全局时间出窗"
        );
        assert_eq!(m.vendor_failure_window("b"), (2, 0));
    }

    /// 重试维度计数与守门器视图：r=2/3、f=1.0、λ=6/60 精确值；
    /// 无失败 → None。
    #[test]
    fn amplification_view_returns_exact_guard_inputs() {
        let m = GlobalMetrics::new();
        assert!(m.amplification_view().is_none(), "无失败无 r 可言");

        for _ in 0..6 {
            m.record_request("v", false, Duration::from_millis(1));
        }
        m.record_retry_attempt();
        m.record_retry_attempt();
        for _ in 0..3 {
            m.record_failed_request_before_retry();
        }
        assert_eq!(m.retried_attempts(), 2);
        assert_eq!(m.failed_requests_before_retry(), 3);

        let (r, f, lambda) = m.amplification_view().expect("有失败必须 Some");
        assert!((r - 2.0 / 3.0).abs() < 1e-12);
        assert!((f - 1.0).abs() < 1e-12);
        assert!((lambda - 0.1).abs() < 1e-12, "λ=6/60");
    }

    /// reset 全量清零：计数、vendor 表、流量滑窗、失败率滑窗、重试维度。
    #[test]
    fn reset_clears_everything() {
        let m = GlobalMetrics::new();
        let t0 = Instant::now();
        m.record_request("v", true, Duration::from_millis(10));
        m.record_affinity_hit();
        m.record_retry_attempt();
        m.record_failed_request_before_retry();
        m.record_traffic_at("v", t0);
        m.record_vendor_outcome_at("v", true, t0);
        assert_eq!(m.snapshot().total_requests, 1);

        m.reset();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 0);
        assert!(snap.vendors.is_empty());
        assert_eq!(snap.affinity_hits, 0);
        assert_eq!(m.affinity_hit_rate(), 0.0);
        assert!(m.traffic_shares().is_empty());
        assert_eq!(m.vendor_failure_window("v"), (0, 0));
        assert_eq!(m.retried_attempts(), 0);
        assert_eq!(m.failed_requests_before_retry(), 0);
        assert!(m.amplification_view().is_none());
    }
}

#[cfg(all(test, feature = "vendor-management"))]
mod rate_limiter_coverage {
    use fluxgate::vendor::rate_limiter::{ConcurrencyLimiter, TokenBucket};
    use std::time::{Duration, Instant};

    /// in_flight 记账：acquire 递增、Drop 递减、max_concurrency 透传构造值。
    #[tokio::test]
    async fn concurrency_limiter_tracks_in_flight_across_acquire_and_drop() {
        let limiter = ConcurrencyLimiter::new(4);
        assert_eq!(limiter.max_concurrency(), 4);
        assert_eq!(limiter.in_flight(), 0);

        let p1 = limiter.acquire().await;
        let p2 = limiter.acquire().await;
        assert_eq!(limiter.in_flight(), 2, "2 个在途票据");
        drop(p1);
        assert_eq!(limiter.in_flight(), 1, "Drop 一个立即递减");
        drop(p2);
        assert_eq!(limiter.in_flight(), 0, "全部 Drop 归零（不泄漏）");
    }

    /// try_acquire 是租约式预检：成功即占用 1 张票据至租约到期（30s），
    /// 期间 in_flight 同步 +1 —— 预检不是无副作用的探测。
    #[tokio::test]
    async fn try_acquire_takes_a_counted_lease() {
        let limiter = ConcurrencyLimiter::new(4);
        assert!(limiter.try_acquire().await, "空载预检必须放行");
        assert_eq!(
            limiter.in_flight(),
            1,
            "预检租约期间必须计入在途（近似请求生命周期）"
        );
    }

    /// acquire_weighted 钳位边界：cost=0 按 1 张票据、cost 超容量按容量
    /// 截断（否则 acquire_many 永久等待死锁），持票期间 in_flight 等于
    /// 钳位后的票据数。
    #[tokio::test]
    async fn acquire_weighted_clamps_cost_into_valid_range() {
        let limiter = ConcurrencyLimiter::new(4);

        let p0 = limiter.acquire_weighted(0).await;
        assert_eq!(limiter.in_flight(), 1, "cost=0 钳位为 1 张票据");
        drop(p0);

        let pmax = limiter.acquire_weighted(999).await;
        assert_eq!(limiter.in_flight(), 4, "cost>容量 截断为容量 4 张票据");
        drop(pmax);

        let pmid = limiter.acquire_weighted(2).await;
        assert_eq!(limiter.in_flight(), 2, "正常 cost 按原值发放");
        drop(pmid);
        assert_eq!(limiter.in_flight(), 0);
    }

    /// TokenBucket 满桶：首次 acquire 立即成功（不等 refill）。
    #[tokio::test]
    async fn token_bucket_full_bucket_acquires_immediately() {
        let bucket = TokenBucket::new(1, 1);
        let start = Instant::now();
        bucket.acquire().await;
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "满桶首次获取不得等待: {:?}",
            start.elapsed()
        );
    }

    /// TokenBucket 桶空：配额耗尽后 acquire 必须等待 refill（真实睡眠
    /// 循环语义），而非立即放行或永久挂起。
    #[tokio::test]
    async fn token_bucket_empty_bucket_waits_for_refill() {
        // 容量 1、速率 1 token/s：第 2 次 acquire 至少等一个 refill 周期
        let bucket = TokenBucket::new(1, 1);
        bucket.acquire().await;
        let start = Instant::now();
        bucket.acquire().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(200),
            "桶空必须等待 refill（实测 {elapsed:?}），立即返回即超卖"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "等待须收敛于 refill 周期量级: {elapsed:?}"
        );
    }
}
