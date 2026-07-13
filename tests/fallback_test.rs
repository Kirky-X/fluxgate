// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! T040 — FallbackChain 单元测试。
//!
//! 测试属性（规则9：验证有意义行为）：
//! 1. primary 5xx 失败 → 返回 secondary（按 priority 升序）
//! 2. trigger=Http5xx 时 429 错误不触发 fallback
//! 3. trigger=All 时任何错误都触发
//! 4. 全部 fallback 用尽返回 None
//! 5. trigger=CircuitOpen 仅匹配 "circuit open" 错误
//! 6. trigger=RateLimited 匹配 RateLimited variant 与 429 VendorUnavailable

#![cfg(feature = "resilience")]

use nexusflow::error::NexusFlowError;
use nexusflow::vendor::resilience::{FallbackChain, FallbackTarget, FallbackTrigger};

fn target(vendor: &str, priority: u8) -> FallbackTarget {
    FallbackTarget {
        vendor_id: vendor.into(),
        model_id: "gpt-4".into(),
        priority,
    }
}

#[test]
fn test_primary_5xx_failure_returns_secondary() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Http5xx,
    );
    let error = NexusFlowError::VendorUnavailable {
        vendor: "primary (500: internal server error)".into(),
    };

    assert!(chain.should_trigger(&error), "500 错误应触发 Http5xx");
    let next = chain
        .next_after_failure("primary", &error)
        .expect("应返回下一个 fallback target");
    assert_eq!(next.vendor_id, "secondary");
    assert_eq!(next.priority, 2);
}

#[test]
fn test_http5xx_trigger_ignores_429() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Http5xx,
    );
    let error = NexusFlowError::VendorUnavailable {
        vendor: "primary (429: rate limit exceeded)".into(),
    };

    assert!(
        !chain.should_trigger(&error),
        "429 不应触发 Http5xx fallback"
    );
    assert!(
        chain.next_after_failure("primary", &error).is_none(),
        "未触发 fallback 时应返回 None"
    );
}

#[test]
fn test_all_trigger_fires_on_any_error() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::All,
    );

    // Timeout 错误
    let timeout_err = NexusFlowError::Timeout {
        vendor: "primary".into(),
    };
    assert!(chain.should_trigger(&timeout_err), "All 应匹配 Timeout");

    // RateLimited variant
    let rl_err = NexusFlowError::RateLimited {
        vendor: "primary".into(),
    };
    assert!(
        chain.should_trigger(&rl_err),
        "All 应匹配 RateLimited"
    );

    // VendorUnavailable 5xx
    let vu_err = NexusFlowError::VendorUnavailable {
        vendor: "primary (503: unavailable)".into(),
    };
    assert!(
        chain.should_trigger(&vu_err),
        "All 应匹配 VendorUnavailable 5xx"
    );

    // 应返回 secondary
    let next = chain
        .next_after_failure("primary", &vu_err)
        .expect("All 触发后应返回下一个 target");
    assert_eq!(next.vendor_id, "secondary");
}

#[test]
fn test_all_fallbacks_exhausted_returns_none() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::All,
    );
    let error = NexusFlowError::VendorUnavailable {
        vendor: "secondary (500: error)".into(),
    };

    assert!(chain.should_trigger(&error));
    // secondary 是 priority 最高的（数值最大），无下一个 target
    assert!(
        chain.next_after_failure("secondary", &error).is_none(),
        "fallback 用尽应返回 None"
    );
}

#[test]
fn test_circuit_open_trigger_matches_only_circuit_open_errors() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::CircuitOpen,
    );

    // CB Open 错误（vendor 字符串含 "circuit open"）
    let cb_open = NexusFlowError::VendorUnavailable {
        vendor: "primary (circuit open)".into(),
    };
    assert!(
        chain.should_trigger(&cb_open),
        "CircuitOpen 应匹配 'circuit open' 错误"
    );
    let next = chain
        .next_after_failure("primary", &cb_open)
        .expect("应返回 secondary");
    assert_eq!(next.vendor_id, "secondary");

    // 普通 5xx 不应触发 CircuitOpen
    let normal_5xx = NexusFlowError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(
        !chain.should_trigger(&normal_5xx),
        "CircuitOpen 不应匹配普通 5xx"
    );
}

#[test]
fn test_rate_limited_trigger_matches_429_and_variant() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::RateLimited,
    );

    // RateLimited variant
    let rl_variant = NexusFlowError::RateLimited {
        vendor: "primary".into(),
    };
    assert!(
        chain.should_trigger(&rl_variant),
        "RateLimited 应匹配 RateLimited variant"
    );

    // VendorUnavailable 含 429 状态码
    let vu_429 = NexusFlowError::VendorUnavailable {
        vendor: "primary (429: rate limit)".into(),
    };
    assert!(
        chain.should_trigger(&vu_429),
        "RateLimited 应匹配 VendorUnavailable 429"
    );

    // 5xx 不应触发 RateLimited
    let vu_500 = NexusFlowError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(
        !chain.should_trigger(&vu_500),
        "RateLimited 不应匹配 5xx"
    );
}

#[test]
fn test_timeout_trigger_matches_only_timeout() {
    let chain = FallbackChain::new(
        vec![target("primary", 1), target("secondary", 2)],
        FallbackTrigger::Timeout,
    );

    let timeout_err = NexusFlowError::Timeout {
        vendor: "primary".into(),
    };
    assert!(chain.should_trigger(&timeout_err), "Timeout 应匹配 Timeout");
    let next = chain
        .next_after_failure("primary", &timeout_err)
        .expect("应返回 secondary");
    assert_eq!(next.vendor_id, "secondary");

    // 5xx 不应触发 Timeout
    let vu_500 = NexusFlowError::VendorUnavailable {
        vendor: "primary (500: error)".into(),
    };
    assert!(
        !chain.should_trigger(&vu_500),
        "Timeout 不应匹配 5xx"
    );
}
