// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 协议转换基线（conversion）：OpenAI 流式 chunk 归一化 / Anthropic 出站编码
//! （含缓存断点注入）/ Anthropic 流式帧编码（tool_use 序列）/ 缓存断点纯函数
//! 注入，四组基准覆盖转换路径的逐帧与逐请求热点。

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use fluxgate::protocol::anthropic_converter::inject_cache_control;
use fluxgate::protocol::{AnthropicConverter, AnthropicStreamEncoder, OpenAIStreamEncoder};
// criterion 0.8 起 `criterion::black_box` 已废弃（-D warnings 下会红），改用标准库实现。
use std::hint::black_box;

mod common;

fn bench_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("conversion");

    // ---- OpenAI 流式 chunk 归一化：稳态续块路径（id/created 逐帧重写） ----
    group.throughput(Throughput::Elements(1));
    let fixture = common::openai_chunk_value();
    let mut encoder = OpenAIStreamEncoder::new();
    // 首块一次：id/created/role 落定后，测量的是续块稳态（与真实流多数帧一致）
    let _ = encoder.normalize(fixture.clone());
    group.bench_function("openai_normalize", |b| {
        b.iter(|| {
            let value = fixture.clone();
            black_box(encoder.normalize(value))
        })
    });

    // ---- Anthropic 出站编码 + 自动缓存断点注入（多轮 + system + tools） ----
    group.throughput(Throughput::Elements(1));
    let converter = AnthropicConverter::new();
    let policy = common::cache_policy();
    let request_fixture = common::anthropic_unified_request();
    group.bench_function("anthropic_request_to_external_with_cache_policy", |b| {
        b.iter(|| {
            let request = request_fixture.clone();
            let body = converter
                .request_to_external_with_cache_policy(request, &policy)
                .expect("出站编码必须成功");
            black_box(body)
        })
    });

    // ---- Anthropic 流式帧编码：tool_use 片段序列（首片段 → 参数分片 → 终止） ----
    let sequence = common::tool_use_chunk_sequence();
    // fixture 冒烟：序列必须产出事件帧（防 fixture 形态漂移后基准空转）
    let smoke_events: usize = {
        let mut smoke = AnthropicStreamEncoder::new();
        sequence
            .iter()
            .cloned()
            .map(|chunk| smoke.encode_chunk(chunk).len())
            .sum()
    };
    assert!(smoke_events > 0, "tool_use 序列必须产出事件帧");
    group.throughput(Throughput::Elements(sequence.len() as u64));
    group.bench_function("anthropic_encode_chunk_tool_use_sequence", |b| {
        b.iter(|| {
            let mut encoder = AnthropicStreamEncoder::new();
            let mut event_count = 0usize;
            for chunk in sequence.iter().cloned() {
                event_count += encoder.encode_chunk(chunk).len();
            }
            black_box(event_count)
        })
    });

    // ---- 缓存断点注入纯函数（长多轮请求体：tools / system / last_user 三锚点） ----
    group.throughput(Throughput::Elements(1));
    let body_fixture = common::long_anthropic_body();
    group.bench_function("inject_cache_control", |b| {
        b.iter(|| {
            let mut body = body_fixture.clone();
            black_box(inject_cache_control(&mut body, &policy))
        })
    });

    group.finish();
}

criterion_group!(conversion_benches, bench_conversion);
criterion_main!(conversion_benches);
