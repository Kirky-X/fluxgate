// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! settle 决策的锁定测试（TDD Red → Green）。
//!
//! 覆盖 R-affinity-001（四模式语义）与 R-affinity-002（阈值上下两侧 +
//! 沉默缓存兜底）。契约断言落在「决策 + 亲和表实际状态」两侧：只断言决策
//! 会漏掉写入动作本身（粘住了但键没写 = 功能不存在）。

use super::{decide, AffinityWriter};
use crate::config::{
    loader::default_vendor_cache_ttl_secs, AffinityMode, AffinitySection, ConfigManager,
    VendorConfig,
};
use crate::router::affinity::{
    AffinityOutcome, AffinityStickTable, AffinityTable, CacheUsage, DEFAULT_AFFINITY_TTL,
};
use std::sync::Arc;
use std::time::Duration;

/// 粘性行构造（避免每个用例重复五字段字面量）
fn stick(
    session_key: &str,
    vendor_id: &str,
    expires_at: i64,
) -> crate::router::affinity::AffinityStick {
    crate::router::affinity::AffinityStick {
        tenant: String::new(),
        session_key: session_key.into(),
        vendor_id: vendor_id.into(),
        worth_tokens: 1,
        expires_at,
    }
}

/// 测试用 vendor（只带本决策消费的两字段；其余继承基座）
fn vendor(id: &str, cache_ttl_secs: u64, silent_cache: bool) -> VendorConfig {
    crate::config::VendorConfig {
        id: id.into(),
        name: id.into(),
        type_: "openai".into(),
        api_key_env: "FLUXGATE_TEST_KEY".into(),
        api_key_envs: Vec::new(),
        extra_headers: Default::default(),
        api_base: String::new(),
        enabled: true,
        priority: 1,
        models: vec![],
        rate_limit: crate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        },
        concurrency: crate::config::ConcurrencyConfig::default(),
        timeout_ms: 60_000,
        stream_ttft_ms: 60_000,
        stream_idle_ms: 30_000,
        supports_batch: false,
        chat_path: None,
        api_style: None,
        anthropic_version: None,
        forward_beta_headers: false,
        send_nonstandard_params: false,
        cache_ttl_secs,
        silent_cache,
    }
}

fn section(mode: &str) -> AffinitySection {
    AffinitySection {
        mode: mode.into(),
        cache_worth_tokens: 1024,
        turn_gap_secs: 120,
    }
}

/// 裸写入器（无落盘通道）：断言决策与亲和表状态
async fn build_writer(mode: &str) -> (AffinityWriter, Arc<AffinityTable>) {
    let config = crate::config::Config {
        masking: Default::default(),
        web_search: Default::default(),
        rate_limit: Default::default(),
        server: crate::config::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: crate::config::DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors: vec![],
        global_model: None,
        pricing: vec![],
        model_catalog: Vec::new(),
        auth: None,
        cors: None,
        retry: Default::default(),
        routing: crate::config::RoutingSection {
            affinity: section(mode),
            ..Default::default()
        },
        cache: Default::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: Default::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
        otlp: Default::default(),
        vendor_budgets: Vec::new(),
        usage_insights: Default::default(),
        catalog_sync: Default::default(),
        subscription: Default::default(),
        subscription_accounts: Vec::new(),
        metrics: Default::default(),
    };
    let config = Arc::new(ConfigManager::new(config));
    let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let sticks = Arc::new(AffinityStickTable::new());
    (
        AffinityWriter::new(
            config,
            table.clone(),
            sticks,
            None,
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        ),
        table,
    )
}

/// auto：worth=200 < 1024 → 不写（也不删既有记录）
#[tokio::test]
async fn auto_mode_below_threshold_writes_nothing() {
    let (writer, table) = build_writer("auto").await;
    let v = vendor("v1", 300, false);
    // 预置一条 legacy 粘住的旧记录（TTL 1h）——阈值下不得被删除
    table.bind(None, "s", "old-vendor").await;

    let d = writer
        .settle(
            None,
            Some("s"),
            &v,
            CacheUsage {
                prompt_tokens: 5_000,
                cache_read_tokens: 200,
                cache_creation_tokens: 0,
            },
        )
        .await;
    assert_eq!(d.mode, AffinityMode::Auto);
    assert_eq!(d.outcome, AffinityOutcome::BelowThreshold);
    assert_eq!(d.worth_tokens, 200, "非沉默缓存不计 prompt_tokens");
    assert_eq!(d.ttl_secs, 0, "未写入不报 TTL");
    assert!(!d.outcome.stuck());
    assert_eq!(
        table.lookup(None, "s").await.as_deref(),
        Some("old-vendor"),
        "既有记录必须保留（自然过期，不中途撕裂）"
    );
}

/// auto：worth=5000 ≥ 1024 → 粘住，TTL = vendor `cache_ttl_secs`
#[tokio::test]
async fn auto_mode_worth_writes_with_vendor_ttl() {
    let (writer, table) = build_writer("auto").await;
    let v = vendor("v1", 3600, false);

    let d = writer
        .settle(
            None,
            Some("s"),
            &v,
            CacheUsage {
                prompt_tokens: 9_000,
                cache_read_tokens: 4_000,
                cache_creation_tokens: 1_000,
            },
        )
        .await;
    assert_eq!(d.outcome, AffinityOutcome::Stuck);
    assert_eq!(d.worth_tokens, 5_000, "cache_read + cache_creation");
    assert_eq!(d.ttl_secs, 3600, "TTL 取 vendor cache_ttl_secs");
    assert_eq!(table.lookup(None, "s").await.as_deref(), Some("v1"));
}

/// 沉默缓存：cache 列全 0 但 prompt=2000 且 silent=true → 粘住
#[tokio::test]
async fn silent_cache_counts_prompt_tokens() {
    let (writer, table) = build_writer("auto").await;
    let v = vendor("v1", 300, true);

    let d = writer
        .settle(
            None,
            Some("s"),
            &v,
            CacheUsage {
                prompt_tokens: 2_000,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        )
        .await;
    assert_eq!(d.outcome, AffinityOutcome::Stuck);
    assert_eq!(d.worth_tokens, 2_000, "沉默缓存把 prompt_tokens 当写入值");
    assert_eq!(table.lookup(None, "s").await.as_deref(), Some("v1"));

    // 对照：同一 usage 在静默关闭时不粘（worth 恒 0）
    let (writer2, table2) = build_writer("auto").await;
    let v2 = vendor("v1", 300, false);
    let d2 = writer2
        .settle(
            None,
            Some("s"),
            &v2,
            CacheUsage {
                prompt_tokens: 2_000,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        )
        .await;
    assert_eq!(d2.outcome, AffinityOutcome::BelowThreshold);
    assert_eq!(table2.lookup(None, "s").await, None);
}

/// legacy：粘 1h（现行为冻结——零回归锚点）
#[tokio::test]
async fn legacy_writes_1h() {
    let (writer, table) = build_writer("legacy").await;
    let v = vendor("v1", 300, false);

    let d = writer
        .settle(None, Some("s"), &v, CacheUsage::default())
        .await;
    assert_eq!(d.mode, AffinityMode::Legacy);
    assert_eq!(d.outcome, AffinityOutcome::LegacyStuck);
    assert_eq!(d.ttl_secs, 3600, "legacy 恒 1h，不看 vendor TTL / worth");
    assert_eq!(table.lookup(None, "s").await.as_deref(), Some("v1"));
}

/// turn：粘 `turn_gap_secs`（不看 worth——usage 全 0 也要粘）
#[tokio::test]
async fn turn_writes_gap_ttl() {
    let (writer, table) = build_writer("turn").await;
    let v = vendor("v1", 3600, false);

    let d = writer
        .settle(None, Some("s"), &v, CacheUsage::default())
        .await;
    assert_eq!(d.mode, AffinityMode::Turn);
    assert_eq!(d.outcome, AffinityOutcome::TurnStuck);
    assert_eq!(d.ttl_secs, 120, "TTL = turn_gap_secs");
    assert_eq!(d.worth_tokens, 0, "turn 不看缓存价值");
    assert_eq!(table.lookup(None, "s").await.as_deref(), Some("v1"));
}

/// off：不写（亲和键恒 miss）
#[tokio::test]
async fn off_writes_nothing() {
    let (writer, table) = build_writer("off").await;
    let v = vendor("v1", 3600, true);

    let d = writer
        .settle(
            None,
            Some("s"),
            &v,
            CacheUsage {
                prompt_tokens: 100_000,
                cache_read_tokens: 100_000,
                cache_creation_tokens: 100_000,
            },
        )
        .await;
    assert_eq!(d.mode, AffinityMode::Off);
    assert_eq!(d.outcome, AffinityOutcome::Skipped);
    assert_eq!(table.lookup(None, "s").await, None, "off 模式绝不写亲和");
}

/// 无会话键：零副作用（不写、不算命中），模式仍如实回传
#[tokio::test]
async fn no_session_key_is_inert() {
    let (writer, table) = build_writer("auto").await;
    let v = vendor("v1", 300, true);
    let d = writer.settle(None, None, &v, CacheUsage::default()).await;
    assert_eq!(d.mode, AffinityMode::Auto);
    assert_eq!(d.outcome, AffinityOutcome::Skipped);
    // 无会话键：亲和键不得被写入（键空间保持空）
    assert_eq!(table.lookup(None, "s").await, None, "无会话键零副作用");
}

/// 阈值边界：worth 恰好等于门限 → 粘（≥ 语义，含等号）
#[test]
fn threshold_boundary_is_inclusive() {
    let s = section("auto");
    let v = vendor("v", 300, false);
    let at = CacheUsage {
        prompt_tokens: 0,
        cache_read_tokens: 1024,
        cache_creation_tokens: 0,
    };
    assert_eq!(decide(&s, Some(&v), at).outcome, AffinityOutcome::Stuck);
    let below = CacheUsage {
        prompt_tokens: 0,
        cache_read_tokens: 1023,
        cache_creation_tokens: 0,
    };
    assert_eq!(
        decide(&s, Some(&v), below).outcome,
        AffinityOutcome::BelowThreshold
    );
}

/// 未知 mode 按 legacy（回滚阀——配置期已拒绝，运行期兜底）
#[test]
fn unknown_mode_falls_back_to_legacy() {
    let s = section("turbo");
    assert_eq!(
        decide(&s, None, CacheUsage::default()).mode,
        AffinityMode::Legacy
    );
    // 大小写不敏感 + 前后空白
    for raw in ["AUTO", " auto ", "Off"] {
        let mut s2 = section(raw);
        s2.mode = raw.into();
        let m = AffinityMode::from_section(&s2);
        assert_ne!(m, AffinityMode::Legacy, "{raw} 必须被识别");
    }
}

/// auto 且 vendor 缺失（配置竞态）：TTL 取 vendor 级缺省 300，不 panic
#[test]
fn auto_without_vendor_uses_default_ttl() {
    let d = decide(
        &section("auto"),
        None,
        CacheUsage {
            prompt_tokens: 0,
            cache_read_tokens: 2048,
            cache_creation_tokens: 0,
        },
    );
    assert_eq!(d.outcome, AffinityOutcome::Stuck);
    assert_eq!(d.ttl_secs, default_vendor_cache_ttl_secs());
}

/// settle 落键的 TTL 随模式变化（auto 取 vendor TTL、turn 取 gap、legacy 恒 1h）
#[tokio::test]
async fn settled_ttl_follows_mode() {
    // auto → vendor cache_ttl_secs（600，与 legacy 的 3600 明显可区分）
    let (writer, table) = build_writer("auto").await;
    writer
        .settle(
            None,
            Some("s"),
            &vendor("v1", 600, false),
            CacheUsage {
                prompt_tokens: 0,
                cache_read_tokens: 4_000,
                cache_creation_tokens: 0,
            },
        )
        .await;
    let ttl = table
        .remaining_ttl(None, "s")
        .await
        .expect("已粘住的键必须有剩余 TTL");
    assert!(
        ttl <= Duration::from_secs(600) && ttl > Duration::from_secs(540),
        "auto 的 TTL 必须取 vendor cache_ttl_secs=600（实测 {ttl:?}）"
    );

    // turn → turn_gap_secs（120）
    let (writer, table) = build_writer("turn").await;
    writer
        .settle(
            None,
            Some("s"),
            &vendor("v1", 3600, false),
            CacheUsage::default(),
        )
        .await;
    let ttl = table
        .remaining_ttl(None, "s")
        .await
        .expect("turn 模式必须粘住");
    assert!(
        ttl <= Duration::from_secs(120) && ttl > Duration::from_secs(60),
        "turn 的 TTL 必须取 turn_gap_secs=120（实测 {ttl:?}）"
    );

    // legacy → 恒 1h（不看 vendor 的 600）
    let (writer, table) = build_writer("legacy").await;
    writer
        .settle(
            None,
            Some("s"),
            &vendor("v1", 600, false),
            CacheUsage::default(),
        )
        .await;
    let ttl = table
        .remaining_ttl(None, "s")
        .await
        .expect("legacy 必须粘住");
    assert!(
        ttl > Duration::from_secs(3000),
        "legacy 恒 1h，不受 vendor cache_ttl_secs 影响（实测 {ttl:?}）"
    );
}

/// auto 结算后的短 TTL 经一次**路由命中**不得被顶回表级 1h（C-1 回归）。
///
/// 这里用 `lookup_for_mode` 复现路由器读路径的语义（router.rs 的
/// `affinity_hit` 就是它），断言 settle→路由读之后剩余 TTL 仍 ≈ vendor
/// `cache_ttl_secs`。现有 `settled_ttl_follows_mode` 只在续期前取值，
/// 测不到这条。
#[tokio::test]
async fn auto_settle_ttl_survives_route_lookup() {
    let (writer, table) = build_writer("auto").await;
    writer
        .settle(
            None,
            Some("s"),
            &vendor("v1", 300, false),
            CacheUsage {
                prompt_tokens: 0,
                cache_read_tokens: 4_000,
                cache_creation_tokens: 0,
            },
        )
        .await;
    let after_settle = table.remaining_ttl(None, "s").await.expect("已粘住");
    assert!(
        after_settle <= Duration::from_secs(300),
        "settle 后 TTL 必须取 vendor cache_ttl_secs=300（实测 {after_settle:?}）"
    );

    // 模拟路由读：auto 模式走 peek（不续期）
    for _ in 0..3 {
        assert_eq!(
            table
                .lookup_for_mode(AffinityMode::Auto, None, "s")
                .await
                .as_deref(),
            Some("v1"),
            "auto 命中必须返回绑定 vendor"
        );
    }
    let after_route = table.remaining_ttl(None, "s").await.expect("仍粘住");
    assert!(
        after_route <= Duration::from_secs(300),
        "路由命中不得把 auto 的 TTL 顶回表级 1h（实测 {after_route:?}）"
    );
}

/// legacy 结算不得绕过首绑定收敛：租户隔离开启时 settle 走 `bind_if_absent`，
/// 后到的结算不覆盖已收敛的绑定（M-1 回归）。
#[tokio::test]
async fn legacy_settle_respects_tenant_scoped_first_binding() {
    let config = crate::config::Config {
        masking: Default::default(),
        web_search: Default::default(),
        rate_limit: Default::default(),
        server: crate::config::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: crate::config::DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors: vec![],
        global_model: None,
        pricing: vec![],
        model_catalog: Vec::new(),
        auth: None,
        cors: None,
        retry: Default::default(),
        routing: crate::config::RoutingSection {
            affinity: section("legacy"),
            affinity_tenant_scoped: true,
            ..Default::default()
        },
        cache: Default::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: Default::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
        otlp: Default::default(),
        vendor_budgets: Vec::new(),
        usage_insights: Default::default(),
        catalog_sync: Default::default(),
        subscription: Default::default(),
        subscription_accounts: Vec::new(),
        metrics: Default::default(),
    };
    let config = Arc::new(ConfigManager::new(config));
    let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let writer = AffinityWriter::new(
        config,
        table.clone(),
        Arc::new(AffinityStickTable::new()),
        None,
        Arc::new(crate::utils::metrics::GlobalMetrics::new()),
    );

    // 并发首绑定已由 API 层收敛到 v-a（bind_if_absent 胜出者）
    assert!(
        table.bind_if_absent(Some("tenant-a"), "s", "v-a").await,
        "首绑定应写入"
    );
    // 后到的 settle 报的是另一个 vendor（竞态/换路），不得改写绑定
    let d = writer
        .settle(
            Some("tenant-a"),
            Some("s"),
            &vendor("v-b", 300, false),
            CacheUsage::default(),
        )
        .await;
    assert_eq!(d.outcome, AffinityOutcome::LegacyStuck);
    assert_eq!(
        table.peek(Some("tenant-a"), "s").await.as_deref(),
        Some("v-a"),
        "legacy settle 不得覆盖首绑定收敛结果"
    );
}

/// off 模式：既有绑定在查询侧立即失效（止损阀，H-1 回归）。
#[tokio::test]
async fn off_mode_query_short_circuits_existing_bindings() {
    let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
    table.bind(None, "s", "v-a").await;
    assert_eq!(
        table.lookup_for_mode(AffinityMode::Off, None, "s").await,
        None,
        "off 模式必须查询短路（存量绑定不得继续生效）"
    );
    // 键本身不被 off 读路径删除（写侧才是唯一删除点）
    assert_eq!(
        table.peek(None, "s").await.as_deref(),
        Some("v-a"),
        "查询短路不得顺带删除绑定"
    );
}

/// 粘性记账表：同键覆盖（保留最后写入的行）
#[test]
fn stick_table_overwrites_same_key() {
    use crate::router::affinity::AffinityStickTable;
    let table = AffinityStickTable::new();
    assert!(table.is_empty());
    table.record(stick("s", "v1", 1));
    table.record(stick("s", "v2", 2));
    assert_eq!(table.len(), 1, "同键覆盖不累加");
    let snap = table.snapshot();
    assert_eq!(snap[0].vendor_id, "v2", "保留最后写入的行");
}

/// 粘性落盘：auto/turn 投递到 sink（legacy/off 不投递）
#[tokio::test]
async fn sink_receives_sticks_for_auto_and_turn_only() {
    use crate::router::affinity::AffinityStick;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingSink(Arc<AtomicUsize>);
    impl crate::storage::AffinityStickSink for CountingSink {
        fn record(&self, _stick: AffinityStick) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    for (mode, expect) in [("auto", 1usize), ("turn", 1), ("legacy", 0), ("off", 0)] {
        let counter = Arc::new(AtomicUsize::new(0));
        let config = crate::config::Config {
            masking: Default::default(),
            web_search: Default::default(),
            rate_limit: Default::default(),
            server: crate::config::ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: crate::config::RoutingSection {
                affinity: section(mode),
                ..Default::default()
            },
            cache: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: Default::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
            responses: Default::default(),
            otlp: Default::default(),
            vendor_budgets: Vec::new(),
            usage_insights: Default::default(),
            catalog_sync: Default::default(),
            subscription: Default::default(),
            subscription_accounts: Vec::new(),
            metrics: Default::default(),
        };
        let config = Arc::new(ConfigManager::new(config));
        let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
        let writer = AffinityWriter::new(
            config,
            table,
            Arc::new(AffinityStickTable::new()),
            Some(Arc::new(CountingSink(counter.clone()))),
            Arc::new(crate::utils::metrics::GlobalMetrics::new()),
        );
        writer
            .settle(
                None,
                Some("s"),
                &vendor("v1", 300, true),
                CacheUsage {
                    prompt_tokens: 4_000,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            )
            .await;
        assert_eq!(counter.load(Ordering::Relaxed), expect, "{mode} 投递数不符");
    }
}

/// 粘性记账表：同键覆盖、上限淘汰最旧的
#[test]
fn stick_table_is_bounded_by_oldest_first() {
    use crate::router::affinity::{AffinityStickTable, EVICT_SCAN_INTERVAL, MAX_TRACKED_STICKS};
    let table = AffinityStickTable::new();
    for i in 0..(MAX_TRACKED_STICKS + 600) {
        table.record(stick(&format!("s{i}"), "v", 1));
    }
    // 摊还淘汰：扫描点之间可短暂越界，上界 = 上限 + 扫描步长
    assert!(
        table.len() <= MAX_TRACKED_STICKS + EVICT_SCAN_INTERVAL as usize
            && table.len() >= MAX_TRACKED_STICKS,
        "条目数必须被压在上限附近（实测 {}，上限 {MAX_TRACKED_STICKS}）",
        table.len()
    );
    let snap = table.snapshot();
    assert!(
        !snap.iter().any(|s| s.session_key == "s0"),
        "最旧的 s0 必须被淘汰"
    );
    assert!(
        snap.iter()
            .any(|s| s.session_key == format!("s{}", MAX_TRACKED_STICKS + 599)),
        "最新写入的必须保留"
    );
}

/// worth 饱和：三档各自 u64::MAX 相加不 panic、仍判为值得粘
#[test]
fn worth_saturates_instead_of_overflowing() {
    let usage = CacheUsage {
        prompt_tokens: u64::MAX,
        cache_read_tokens: u64::MAX,
        cache_creation_tokens: u64::MAX,
    };
    assert_eq!(usage.worth(true), u64::MAX);
    assert_eq!(usage.worth(false), u64::MAX);
}

// ── 亲和读路径的模式分派（TTL 续期语义）─────────────────────────────

/// 与模式分派读对照的旧 `lookup`：legacy 语义（命中续期）在表层保持可用。
#[tokio::test]
async fn lookup_still_reports_hit_and_miss() {
    let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);

    assert_eq!(table.lookup(None, "s").await, None, "无绑定是 miss");
    table.bind(None, "s", "vendor-a").await;
    assert_eq!(
        table.lookup(None, "s").await.as_deref(),
        Some("vendor-a"),
        "命中返回绑定 vendor"
    );
}

/// legacy 命中的续期语义由 `lookup`/`lookup_for_mode(Legacy)` 承担；
/// `auto`/`turn` 走 peek（不续期）——见 `affinity.rs` 的模式分派单测。
#[tokio::test]
async fn legacy_lookup_keeps_renewing_active_sessions() {
    let table = AffinityTable::with_memory_backend(Duration::from_millis(500));
    table.bind(None, "hot", "vendor-a").await;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            table.lookup(None, "hot").await.as_deref(),
            Some("vendor-a"),
            "legacy 活跃会话命中必须续期"
        );
    }
}

/// 独占键空间：租户维度命中不影响 legacy 键（与 `lookup` 同键规则）
#[tokio::test]
async fn scoped_keys_are_isolated() {
    let table = AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL);
    table.bind(Some("tenant-a"), "s", "vendor-a").await;
    assert_eq!(
        table.peek(Some("tenant-a"), "s").await.as_deref(),
        Some("vendor-a")
    );
    assert_eq!(
        table.peek(Some("tenant-b"), "s").await,
        None,
        "他租户不得命中"
    );
    assert_eq!(
        table.peek(None, "s").await,
        None,
        "legacy 键空间与租户键互不可见"
    );
}
