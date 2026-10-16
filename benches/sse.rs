// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! SSE 帧解析基线（sse）：两协议 `parse_sse_frame_to_chunk` 各一组基准——
//! 透传路径的组件级代理（入站协议与上游一致时走零拷贝透传，流式内存热点在
//! 逐帧解析）。fixture 为真实 SSE 帧样例，经生产分帧器剥出 data 负载后喂入。

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
// criterion 0.8 起 `criterion::black_box` 已废弃（-D warnings 下会红），改用标准库实现。
use std::hint::black_box;

mod common;

fn bench_sse_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("sse");

    // ---- OpenAI 帧序列（data: 前缀 + [DONE] 终止帧） ----
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
                if fluxgate::protocol::openai_converter::parse_sse_frame_to_chunk(payload).is_some()
                {
                    mapped += 1;
                }
            }
            black_box(mapped)
        })
    });

    // ---- Anthropic 帧序列（event:/data: 前缀 + message_stop 收束） ----
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
                if fluxgate::protocol::anthropic_converter::parse_sse_frame_to_chunk(payload)
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

criterion_group!(sse_benches, bench_sse_parsing);
criterion_main!(sse_benches);
