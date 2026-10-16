// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 验收 — 价格因子路由：等价条件下低价候选胜出；开关关闭行为与
//! 现状逐位一致（权重向量精确相等）。
//!
//! 因子定义（router.rs `price_factor_multipliers` doc 同源）：候选单价
//! p = (input_price + output_price) / 2（每 1M tokens），集合内已登记
//! 最低价者乘性因子 1、高价按 min_p/p 衰减；未登记/0 价候选中性 1.0。

#![cfg(feature = "router")]

#[path = "../common/mod.rs"]
mod common;

use fluxgate::config::{
    ConfigManager, ModelCatalogEntry, ModelConfig, PricingCurrency, VendorConfig,
};
use fluxgate::router::Router;
use fluxgate::vendor::VendorManager;
use std::sync::Arc;

fn vendor_with_model(id: &str, model_id: &str) -> VendorConfig {
    let mut v = common::base_vendor(id, "http://127.0.0.1:9/v1".to_string());
    v.models = vec![ModelConfig {
        model_id: model_id.into(),
        abstract_id: Some("assistant".into()),
        weight: 1.0,
        enabled: true,
    }];
    v
}

fn entry(model_id: &str, input: f64, output: f64) -> ModelCatalogEntry {
    ModelCatalogEntry {
        model_id: model_id.into(),
        context_window: 128_000,
        input_price: input,
        output_price: output,
        currency: PricingCurrency::Usd,
        ..Default::default()
    }
}

fn make_router(
    price_factor_enabled: bool,
    catalog: Vec<ModelCatalogEntry>,
) -> (Router, Arc<VendorManager>) {
    let mut config = common::test_config(vec![
        vendor_with_model("v-expensive", "expensive-model"),
        vendor_with_model("v-cheap", "cheap-model"),
    ]);
    config.routing.price_factor_enabled = price_factor_enabled;
    config.model_catalog = catalog;
    let cm = Arc::new(ConfigManager::new(config));
    let vm = Arc::new(VendorManager::new(cm.clone()));
    (Router::new(cm, vm.clone()), vm)
}

fn weight_of(weighted: &[(f64, (VendorConfig, ModelConfig))], vendor_id: &str) -> f64 {
    weighted
        .iter()
        .find(|(_, (v, _))| v.id == vendor_id)
        .map(|(w, _)| *w)
        .unwrap_or_else(|| panic!("candidate {vendor_id} missing"))
}

/// 等权重双候选：低价乘性因子 1、高价按 min_p/p 衰减 → 低价权重严格更大；
/// 权重比 = 价格因子比（单价 90 vs 9 → 0.1）
#[tokio::test]
async fn price_factor_prefers_cheaper_candidate() {
    let (router, _vm) = make_router(
        true,
        vec![
            entry("expensive-model", 30.0, 150.0),
            entry("cheap-model", 3.0, 15.0),
        ],
    );
    // 候选构造经 router 的公开确定性入口（calculate_weights 内含价格因子）
    let candidates = vec![
        vendor_with_model("v-expensive", "expensive-model"),
        vendor_with_model("v-cheap", "cheap-model"),
    ]
    .into_iter()
    .map(|v| {
        let m = v.models[0].clone();
        (v, m)
    })
    .collect::<Vec<_>>();
    let weighted = router.calculate_weights(candidates).await;

    let w_exp = weight_of(&weighted, "v-expensive");
    let w_cheap = weight_of(&weighted, "v-cheap");
    assert!(
        w_cheap > w_exp,
        "等权重下低价候选权重必须更大: cheap={w_cheap} expensive={w_exp}"
    );
    let ratio = w_exp / w_cheap;
    assert!(
        (ratio - 0.1).abs() < 1e-9,
        "权重比 = 价格因子比 min_p/p = 0.1，got {ratio}"
    );
}

/// 开关关闭：权重向量与无目录基线逐位一致（乘性修正整体跳过）
#[tokio::test]
async fn toggle_off_reproduces_baseline_bitwise() {
    let (off, _) = make_router(
        false,
        vec![
            entry("expensive-model", 30.0, 150.0),
            entry("cheap-model", 3.0, 15.0),
        ],
    );
    let (baseline, _) = make_router(false, vec![]);

    let candidates: Vec<(VendorConfig, ModelConfig)> = [
        ("v-expensive", "expensive-model"),
        ("v-cheap", "cheap-model"),
    ]
    .iter()
    .map(|(id, model)| {
        let v = vendor_with_model(id, model);
        let m = v.models[0].clone();
        (v, m)
    })
    .collect();

    let w_off = off.calculate_weights(candidates.clone()).await;
    let w_base = baseline.calculate_weights(candidates).await;

    assert_eq!(w_off.len(), w_base.len());
    for ((wo, (vo, _)), (wb, (vb, _))) in w_off.iter().zip(w_base.iter()) {
        assert_eq!(vo.id, vb.id, "候选序稳定（对位比较前提）");
        assert_eq!(
            wo, wb,
            "开关关闭：权重与无目录基线逐位一致（{} vs {}）",
            wo, wb
        );
    }
}

/// 未登记候选中性：只登记 cheap 时无高价对照，全体因子 1.0——权重与
/// 无目录基线逐位一致（calculate_weight 的负载因子两 vendor 同基座同值）
#[tokio::test]
async fn unregistered_candidates_stay_neutral() {
    let (router, _) = make_router(true, vec![entry("cheap-model", 3.0, 15.0)]);
    let (baseline, _) = make_router(true, vec![]);
    let candidates: Vec<(VendorConfig, ModelConfig)> = [
        ("v-expensive", "expensive-model"),
        ("v-cheap", "cheap-model"),
    ]
    .iter()
    .map(|(id, model)| {
        let v = vendor_with_model(id, model);
        let m = v.models[0].clone();
        (v, m)
    })
    .collect();
    let weighted = router.calculate_weights(candidates.clone()).await;
    let base = baseline.calculate_weights(candidates).await;
    for ((w, (v, _)), (bw, _)) in weighted.iter().zip(base.iter()) {
        assert_eq!(
            w, bw,
            "无有效价格对照时与无目录基线逐位一致（中性）: {} = {w}",
            v.id
        );
    }
}

/// 全 0 价档：无有效单价 → 与无目录基线逐位一致（0 价不构成「更便宜」
/// 信号，不奖励无定价者）
#[tokio::test]
async fn zero_price_entries_stay_neutral() {
    let (router, _) = make_router(
        true,
        vec![
            entry("expensive-model", 0.0, 0.0),
            entry("cheap-model", 0.0, 0.0),
        ],
    );
    let (baseline, _) = make_router(true, vec![]);
    let candidates: Vec<(VendorConfig, ModelConfig)> = [
        ("v-expensive", "expensive-model"),
        ("v-cheap", "cheap-model"),
    ]
    .iter()
    .map(|(id, model)| {
        let v = vendor_with_model(id, model);
        let m = v.models[0].clone();
        (v, m)
    })
    .collect();
    let weighted = router.calculate_weights(candidates.clone()).await;
    let base = baseline.calculate_weights(candidates).await;
    for ((w, _), (bw, _)) in weighted.iter().zip(base.iter()) {
        assert_eq!(w, bw, "0 价不参与因子（与基线逐位一致）: {w} vs {bw}");
    }
}
