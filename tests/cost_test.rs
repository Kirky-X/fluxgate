#![cfg(feature = "vendor-management")]
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! T046 — CostCalculator 单元测试。
//!
//! 测试属性（规则9）：
//! 1. 命中 pricing entry 时按公式计算 USD 成本
//!    formula: (prompt/1000)*in_price + (completion/1000)*out_price
//! 2. 未命中 vendor/model 返回 0.0
//! 3. update_entry 后立即生效（运行时动态更新）

#![cfg(feature = "vendor-management")]

use nexusflow::config::PricingConfig;
use nexusflow::vendor::cost::{CostCalculator, PricingEntry};

#[test]
fn test_calculate_cost_with_known_pricing() {
    let pricing = vec![PricingConfig {
        vendor_id: "openai".into(),
        model_id: "gpt-4".into(),
        input_per_1k_tokens: 0.03,
        output_per_1k_tokens: 0.06,
    }];
    let calc = CostCalculator::from_config(&pricing);

    // 1000 prompt * 0.03/1k = 0.03
    // 500 completion * 0.06/1k = 0.03
    // total = 0.06 USD
    let cost = calc.calculate("openai", "gpt-4", 1000, 500);
    assert!(
        (cost - 0.06).abs() < 1e-9,
        "expected 0.06 USD, got {}",
        cost
    );
}

#[test]
fn test_calculate_cost_zero_for_unknown_pricing() {
    let pricing = vec![PricingConfig {
        vendor_id: "openai".into(),
        model_id: "gpt-4".into(),
        input_per_1k_tokens: 0.03,
        output_per_1k_tokens: 0.06,
    }];
    let calc = CostCalculator::from_config(&pricing);

    // vendor 未配置
    let cost = calc.calculate("anthropic", "gpt-4", 1000, 500);
    assert!(
        (cost - 0.0).abs() < f64::EPSILON,
        "unknown vendor should return 0.0, got {}",
        cost
    );

    // model 未配置
    let cost = calc.calculate("openai", "gpt-3.5-turbo", 1000, 500);
    assert!(
        (cost - 0.0).abs() < f64::EPSILON,
        "unknown model should return 0.0, got {}",
        cost
    );
}

#[test]
fn test_update_entry_takes_effect_immediately() {
    let calc = CostCalculator::from_config(&[]);

    // 初始未命中
    let cost_before = calc.calculate("openai", "gpt-4", 1000, 1000);
    assert!(
        (cost_before - 0.0).abs() < f64::EPSILON,
        "before update should return 0.0, got {}",
        cost_before
    );

    // 动态更新
    calc.update_entry(
        "openai",
        "gpt-4",
        PricingEntry {
            input_per_1k_tokens: 0.01,
            output_per_1k_tokens: 0.03,
        },
    );

    // 立即生效
    let cost_after = calc.calculate("openai", "gpt-4", 1000, 1000);
    assert!(
        (cost_after - 0.04).abs() < 1e-9,
        "after update should return 0.04 USD (0.01 + 0.03), got {}",
        cost_after
    );
}

#[test]
fn test_calculate_cost_with_zero_tokens() {
    // 边界：0 tokens 应返回 0.0 USD
    let pricing = vec![PricingConfig {
        vendor_id: "openai".into(),
        model_id: "gpt-4".into(),
        input_per_1k_tokens: 0.03,
        output_per_1k_tokens: 0.06,
    }];
    let calc = CostCalculator::from_config(&pricing);

    let cost = calc.calculate("openai", "gpt-4", 0, 0);
    assert!(
        (cost - 0.0).abs() < f64::EPSILON,
        "0 tokens should return 0.0, got {}",
        cost
    );
}
