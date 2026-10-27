// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 缓存经济学亲和基线（affinity）：settle 决策（模式分派 + 阈值比较）、
//! 粘性记账 / 亲和表写入与落盘通道投递的每请求成本。验收口径：纯决策路径
//! ≤ 1μs 量级（热路径上每次响应结算多出的固定开销）；写入路径含 oxcache
//! 内存后端 set，量级在 μs 级；带 sink 的变体覆盖本特性新增的
//! `try_send` + 差值对账（通道满时不阻塞）。fixture 不依赖网络与外部服务。

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
// criterion 0.8 起 `criterion::black_box` 已废弃（-D warnings 下会红），改用标准库实现。
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

mod common;

use fluxgate::config::{
    loader::default_vendor_cache_ttl_secs, AffinitySection, ConfigManager, VendorConfig,
};
use fluxgate::router::affinity::{
    AffinityStickTable, AffinityTable, CacheUsage, DEFAULT_AFFINITY_TTL,
};
use fluxgate::router::affinity_settle::{decide, AffinityWriter};
use fluxgate::storage::{AffinityStick, AffinityStickSink};

fn affinity_vendor(id: &str, cache_ttl_secs: u64, silent_cache: bool) -> VendorConfig {
    VendorConfig {
        id: id.into(),
        name: id.into(),
        type_: "openai".into(),
        api_key_env: "FLUXGATE_BENCH_KEY".into(), // pragma: allowlist secret（仅 fixture 环境变量名，非凭证）
        api_key_envs: Vec::new(),
        extra_headers: Default::default(),
        api_base: String::new(),
        enabled: true,
        priority: 1,
        models: vec![],
        rate_limit: fluxgate::config::RateLimitConfig::Interval {
            hours: 1,
            sliding: false,
        },
        concurrency: fluxgate::config::ConcurrencyConfig::default(),
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

fn writer_fixture(mode: &str) -> AffinityWriter {
    let mut config = common::routing_config();
    config.routing.affinity = AffinitySection {
        mode: mode.into(),
        cache_worth_tokens: 1024,
        turn_gap_secs: 120,
    };
    let config = Arc::new(ConfigManager::new(config));
    let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let sticks = Arc::new(AffinityStickTable::new());
    AffinityWriter::new(
        config,
        table,
        sticks,
        None,
        Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
    )
}

/// 计数型 sink（不落库）：只观测投递是否到达与丢弃计数的读取成本。
struct CountingSink {
    recorded: AtomicU64,
    dropped: AtomicU64,
}

impl AffinityStickSink for CountingSink {
    fn record(&self, _stick: AffinityStick) {
        self.recorded.fetch_add(1, Ordering::Relaxed);
    }

    fn dropped_sticks(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

fn writer_fixture_with_sink(mode: &str) -> (AffinityWriter, Arc<CountingSink>) {
    let mut config = common::routing_config();
    config.routing.affinity = AffinitySection {
        mode: mode.into(),
        cache_worth_tokens: 1024,
        turn_gap_secs: 120,
    };
    let config = Arc::new(ConfigManager::new(config));
    let table = Arc::new(AffinityTable::with_memory_backend(DEFAULT_AFFINITY_TTL));
    let sticks = Arc::new(AffinityStickTable::new());
    let sink = Arc::new(CountingSink {
        recorded: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    (
        AffinityWriter::new(
            config,
            table,
            sticks,
            Some(sink.clone()),
            Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
        ),
        sink,
    )
}

/// 纯决策：模式分派 + 阈值比较（零 IO、零锁）。验收 ≤ 1μs 量级。
fn bench_decide_auto(c: &mut Criterion) {
    let section = AffinitySection {
        mode: "auto".into(),
        cache_worth_tokens: 1024,
        turn_gap_secs: 120,
    };
    let vendor = affinity_vendor("bench-vendor", default_vendor_cache_ttl_secs(), false);
    let usage = CacheUsage {
        prompt_tokens: 6_000,
        cache_read_tokens: 5_000,
        cache_creation_tokens: 500,
    };
    let mut group = c.benchmark_group("affinity");
    group.throughput(Throughput::Elements(1));
    group.bench_function("decide_auto", |b| {
        b.iter(|| {
            black_box(decide(
                black_box(&section),
                Some(black_box(&vendor)),
                black_box(usage),
            ))
        })
    });
    group.finish();
}

/// worth 计算（沉默缓存 vendor：三档饱和加法）
fn bench_worth_silent(c: &mut Criterion) {
    let usage = CacheUsage {
        prompt_tokens: 6_000,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    };
    let mut group = c.benchmark_group("affinity");
    group.throughput(Throughput::Elements(1));
    group.bench_function("worth_silent_cache", |b| {
        b.iter(|| black_box(black_box(usage).worth(true)))
    });
    group.finish();
}

/// settle 全路径（auto 粘住：读锁快照 + 决策 + oxcache set + 粘性记账）。
/// async 执行按仓库惯例由 tokio 运行时以 `block_on` 承载。
fn bench_settle_auto_stick(c: &mut Criterion) {
    let writer = Arc::new(writer_fixture("auto"));
    let vendor = affinity_vendor("bench-vendor", 300, false);
    let usage = CacheUsage {
        prompt_tokens: 6_000,
        cache_read_tokens: 5_000,
        cache_creation_tokens: 500,
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio 运行时");
    let mut group = c.benchmark_group("affinity");
    group.throughput(Throughput::Elements(1));
    group.bench_function("settle_auto_stick", |b| {
        b.iter(|| {
            // 会话键固定：粘性记账同键覆盖（与热路径上活跃会话重复结算同形）
            let decision =
                runtime.block_on(writer.settle(None, Some("bench-session"), &vendor, usage));
            black_box(decision)
        })
    });
    group.finish();
}

/// settle 全路径（auto 低于阈值：读锁快照 + 决策，无写入）
fn bench_settle_auto_below_threshold(c: &mut Criterion) {
    let writer = Arc::new(writer_fixture("auto"));
    let vendor = affinity_vendor("bench-vendor", 300, false);
    let usage = CacheUsage {
        prompt_tokens: 200,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio 运行时");
    let mut group = c.benchmark_group("affinity");
    group.throughput(Throughput::Elements(1));
    group.bench_function("settle_auto_below_threshold", |b| {
        b.iter(|| {
            let decision =
                runtime.block_on(writer.settle(None, Some("bench-session"), &vendor, usage));
            black_box(decision)
        })
    });
    group.finish();
}

/// settle 全路径带落盘通道（auto 粘住）：本特性新增的 sink 投递与
/// 丢弃差值对账（`sync_sink_drops` 的原子读 + 罕见原子写）。
fn bench_settle_auto_with_sink(c: &mut Criterion) {
    let (writer, sink) = writer_fixture_with_sink("auto");
    let writer = Arc::new(writer);
    let vendor = affinity_vendor("bench-vendor", 300, false);
    let usage = CacheUsage {
        prompt_tokens: 6_000,
        cache_read_tokens: 5_000,
        cache_creation_tokens: 500,
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio 运行时");
    let mut group = c.benchmark_group("affinity");
    group.throughput(Throughput::Elements(1));
    group.bench_function("settle_auto_with_sink", |b| {
        b.iter(|| {
            let decision =
                runtime.block_on(writer.settle(None, Some("bench-sink"), &vendor, usage));
            black_box(decision)
        })
    });
    group.finish();
    black_box(sink.recorded.load(Ordering::Relaxed));
}

criterion_group!(
    affinity_benches,
    bench_decide_auto,
    bench_worth_silent,
    bench_settle_auto_stick,
    bench_settle_auto_below_threshold,
    bench_settle_auto_with_sink
);
criterion_main!(affinity_benches);
