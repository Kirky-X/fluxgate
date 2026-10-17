// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 路由决策基线（routing）：固定 5 vendor 加权表上 `Router::route_weighted` 的
//! 决策核心路径（可选层全关，fixture 不依赖网络与外部服务）。async 执行按仓库
//! 惯例由 tokio 运行时以 `block_on` 承载；含 block_on 进入开销属组件级口径。

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
// criterion 0.8 起 `criterion::black_box` 已废弃（-D warnings 下会红），改用标准库实现。
use std::hint::black_box;

mod common;

fn bench_route_weighted(c: &mut Criterion) {
    let router = common::router_fixture();
    let runtime = tokio::runtime::Runtime::new().expect("tokio 运行时");
    let mut group = c.benchmark_group("routing");
    group.throughput(Throughput::Elements(1));
    group.bench_function("route_weighted", |b| {
        b.iter(|| {
            let decision = runtime
                .block_on(router.route_weighted("gpt-4", None, None, &[], None, None, None, None))
                .expect("加权路由必须成功");
            black_box(decision)
        })
    });
    group.finish();
}

criterion_group!(routing_benches, bench_route_weighted);
criterion_main!(routing_benches);
