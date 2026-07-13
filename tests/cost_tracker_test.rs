// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! T050 — CostTracker 单元测试。
//!
//! 测试属性（规则9）：
//! 1. 并发 1000 次 record(0.01) 后 total_usd ≈ 10.0（容差 0.01）
//! 2. by_vendor_usd 累加正确，且 == total_usd（仅 openai vendor 时）
//! 3. by_model_usd 累加正确
//! 4. snapshot() 含 total_usd + by_vendor + by_model 三组字段
//! 5. record(0.0) 不改变 total_usd
//! 6. 跨 vendor/model 隔离 — 不同 vendor 独立计数

#![cfg(feature = "cost-tracking")]

use nexusflow::utils::cost_tracker::CostTracker;
use std::sync::Arc;

#[tokio::test]
async fn test_concurrent_record_accumulates_total() {
    let tracker = Arc::new(CostTracker::new());
    let mut handles = vec![];
    for _ in 0..1000 {
        let t = tracker.clone();
        handles.push(tokio::spawn(async move {
            t.record("openai", "gpt-4", 0.01);
        }));
    }
    for h in handles {
        h.await.expect("task should not panic");
    }

    let total = tracker.total_usd();
    assert!(
        (total - 10.0).abs() < 0.01,
        "after 1000 records of 0.01 USD, expected ~10.0, got {}",
        total
    );
}

#[test]
fn test_by_vendor_usd_matches_total_when_single_vendor() {
    let tracker = CostTracker::new();
    tracker.record("openai", "gpt-4", 0.5);
    tracker.record("openai", "gpt-3.5-turbo", 0.3);
    tracker.record("openai", "gpt-4", 0.2);

    let total = tracker.total_usd();
    let by_vendor = tracker.by_vendor_usd("openai");
    assert!(
        (total - 1.0).abs() < 1e-9,
        "total should be 1.0 USD, got {}",
        total
    );
    assert!(
        (by_vendor - 1.0).abs() < 1e-9,
        "by_vendor(openai) should equal total = 1.0, got {}",
        by_vendor
    );
}

#[test]
fn test_by_model_usd_isolated_per_model() {
    let tracker = CostTracker::new();
    tracker.record("openai", "gpt-4", 0.5);
    tracker.record("openai", "gpt-3.5-turbo", 0.3);
    tracker.record("openai", "gpt-4", 0.2);

    let gpt4 = tracker.by_model_usd("gpt-4");
    let gpt35 = tracker.by_model_usd("gpt-3.5-turbo");
    assert!(
        (gpt4 - 0.7).abs() < 1e-9,
        "gpt-4 should be 0.7 USD (0.5 + 0.2), got {}",
        gpt4
    );
    assert!(
        (gpt35 - 0.3).abs() < 1e-9,
        "gpt-3.5-turbo should be 0.3 USD, got {}",
        gpt35
    );
}

#[test]
fn test_snapshot_has_correct_fields() {
    let tracker = CostTracker::new();
    tracker.record("openai", "gpt-4", 0.5);
    tracker.record("anthropic", "claude-3-opus", 0.3);

    let snap = tracker.snapshot();
    assert!(
        (snap.total_usd - 0.8).abs() < 1e-9,
        "snapshot.total_usd should be 0.8, got {}",
        snap.total_usd
    );
    assert_eq!(snap.by_vendor.len(), 2, "should have 2 vendors");
    assert_eq!(snap.by_model.len(), 2, "should have 2 models");

    let openai_usd = *snap.by_vendor.get("openai").expect("openai entry");
    let anthropic_usd = *snap.by_vendor.get("anthropic").expect("anthropic entry");
    assert!((openai_usd - 0.5).abs() < 1e-9, "openai USD: {}", openai_usd);
    assert!((anthropic_usd - 0.3).abs() < 1e-9, "anthropic USD: {}", anthropic_usd);
}

#[test]
fn test_zero_cost_record_does_not_change_total() {
    let tracker = CostTracker::new();
    tracker.record("openai", "gpt-4", 0.5);
    let before = tracker.total_usd();

    tracker.record("openai", "gpt-4", 0.0);
    let after = tracker.total_usd();

    assert!(
        (after - before).abs() < f64::EPSILON,
        "zero-cost record should not change total (before={}, after={})",
        before,
        after
    );
}

#[test]
fn test_vendor_isolation() {
    let tracker = CostTracker::new();
    tracker.record("openai", "gpt-4", 0.5);
    tracker.record("anthropic", "claude-3-opus", 0.3);

    let openai = tracker.by_vendor_usd("openai");
    let anthropic = tracker.by_vendor_usd("anthropic");
    let unknown = tracker.by_vendor_usd("unknown-vendor");

    assert!((openai - 0.5).abs() < 1e-9, "openai: {}", openai);
    assert!((anthropic - 0.3).abs() < 1e-9, "anthropic: {}", anthropic);
    assert!(
        (unknown - 0.0).abs() < f64::EPSILON,
        "unknown vendor should return 0.0, got {}",
        unknown
    );
}

#[test]
fn test_total_usd_zero_initially() {
    let tracker = CostTracker::new();
    assert!(
        tracker.total_usd() == 0.0,
        "new tracker total should be 0.0"
    );
}
