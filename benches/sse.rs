// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SSE 帧解析基线（sse）：两协议 `parse_sse_frame_to_chunk` 各一组基准——
//! 透传路径的组件级代理（入站协议与上游一致时走零拷贝透传，流式内存热点在
//! 逐帧解析）。fixture 为真实 SSE 帧样例，经生产分帧器剥出 data 负载后喂入。

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
// criterion 0.8 起 `criterion::black_box` 已废弃（-D warnings 下会红），改用标准库实现。
use std::hint::black_box;

mod common;

fn bench_sse_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("sse");

    // OpenAI 帧序列（data: 前缀 + [DONE] 终止帧）
    let openai_payloads = common::data_payloads(common::OPENAI_SSE_TAIL);
    assert!(
        !openai_payloads.is_empty(),
        "OpenAI 帧样例必须可解析出 data 负载"
    );
    group.throughput(Throughput::Elements(openai_payloads.len() as u64));
    group.bench_function("openai_parse_sse_frame_to_chunk", |b| {
        b.iter(|| {
            let mut mapped = 0usize;
            for payload in &openai_payloads {
                if fluxweir::protocol::openai_converter::parse_sse_frame_to_chunk(payload).is_some()
                {
                    mapped += 1;
                }
            }
            black_box(mapped)
        })
    });

    // Anthropic 帧序列（event:/data: 前缀 + message_stop 收束）
    let anthropic_payloads = common::data_payloads(common::ANTHROPIC_SSE_TAIL);
    assert!(
        !anthropic_payloads.is_empty(),
        "Anthropic 帧样例必须可解析出 data 负载"
    );
    group.throughput(Throughput::Elements(anthropic_payloads.len() as u64));
    group.bench_function("anthropic_parse_sse_frame_to_chunk", |b| {
        b.iter(|| {
            let mut mapped = 0usize;
            for payload in &anthropic_payloads {
                if fluxweir::protocol::anthropic_converter::parse_sse_frame_to_chunk(payload)
                    .is_some()
                {
                    mapped += 1;
                }
            }
            black_box(mapped)
        })
    });

    group.finish();
}

/// 流式回程还原吞吐对照（feature `masking` 门控编译）：掩码关 = 纯文本
/// 透传基线；掩码开 = 同文本流经 `StreamRestorer`（尾部保持缓冲 +
/// 占位符回填）。量化流式还原的每事件开销。
#[cfg(feature = "masking")]
fn bench_stream_restore(c: &mut Criterion) {
    use fluxweir::masking::{MaskKind, PlaceholderRegistry, StreamRestorer};
    use std::sync::Arc;

    let mut group = c.benchmark_group("sse_masking");
    // 模拟 SSE 文本增量流：20 字节 × 512 增量（占位符整段落在单个增量内）
    let registry = Arc::new(PlaceholderRegistry::new());
    let placeholder = registry.mask(MaskKind::ApiKey, "sk-bench-secret-0123456789abcdef");
    let plain: Vec<String> = (0..512)
        .map(|i| format!("plain delta {i:04} is here........"))
        .collect();
    let masked: Vec<String> = plain
        .iter()
        .enumerate()
        .map(|(i, d)| {
            if i % 16 == 0 {
                format!("{d} {}", placeholder)
            } else {
                d.clone()
            }
        })
        .collect();

    group.throughput(Throughput::Bytes(
        plain.iter().map(|d| d.len() as u64).sum::<u64>(),
    ));
    // 掩码关：透传基线（零还原语义）
    group.bench_function("restore_off_passthrough", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for delta in &plain {
                total += black_box(delta).len();
            }
            black_box(total)
        })
    });
    // 掩码开：StreamRestorer 全量流（含占位符回填与 hold 缓冲）
    group.bench_function("restore_on_placeholder_stream", |b| {
        b.iter(|| {
            let mut restorer = StreamRestorer::new(Arc::clone(&registry));
            let mut total = 0usize;
            for delta in &masked {
                total += restorer.feed(black_box(delta)).len();
            }
            total += restorer.flush().len();
            black_box(total)
        })
    });
    group.finish();
}

#[cfg(feature = "masking")]
criterion_group!(sse_benches, bench_sse_parsing, bench_stream_restore);
#[cfg(not(feature = "masking"))]
criterion_group!(sse_benches, bench_sse_parsing);
criterion_main!(sse_benches);
