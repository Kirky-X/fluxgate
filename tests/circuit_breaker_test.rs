// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 熔断器测试（recId=2）— 直接驱动 `limiteron::circuit::CircuitBreaker`。
//!
//! 手写三态机已删除（HalfOpen 死锁：默认 success_threshold=3 +
//! half_open_max_calls=1，half_open_calls 从不在探针间清零，首个探针
//! 成功后 success_count=1<3 恒拒，熔断一次即永久拒绝）。本文件用
//! limiteron 库版 + 注入假 Clock（`with_clock`，limiteron
//! src/circuit/types.rs:281）验证：
//! 1. 5 次可重试失败 → Open（failure_threshold=5）
//! 2. 死锁回归：超时后探针连续成功 3 次 → Closed（手写版此场景永久卡 HalfOpen）
//! 3. 探针失败 → 立即回 Open
//! 4. 映射层单测：to/from 往返还原、解析失败降级路径、分类器 variant 语义

#![cfg(all(feature = "resilience", feature = "vendor-management"))]

use limiteron::circuit::{CircuitBreaker, CircuitBreakerConfig, ErrorClassifier};
use limiteron::error::{CircuitState, LimiteronError};
use nexusflow::error::NexusFlowError;
use nexusflow::vendor::resilience::{
    from_limiteron, is_circuit_open_error, to_limiteron, NexusFlowErrorClassifier,
};
use std::sync::Arc;
use std::time::Duration;

/// failure_threshold=5 / success_threshold=3 / timeout=30s（生产同款配置），
/// 错误分类器与 manager 接线一致（NexusFlowErrorClassifier）。
fn make_cb() -> (CircuitBreaker, Arc<limiteron::MockClock>) {
    let clock = Arc::new(limiteron::MockClock::new());
    let clock_dyn: Arc<dyn limiteron::Clock> = clock.clone();
    let config = CircuitBreakerConfig::new(5, 3, Duration::from_secs(30))
        .error_classifier(Arc::new(NexusFlowErrorClassifier));
    (CircuitBreaker::with_clock(config, clock_dyn), clock)
}

/// 桩异步操作：返回以映射层编码的上游不可用错误（计失败）
fn failing_op() -> impl FnOnce() -> std::future::Ready<Result<(), LimiteronError>> {
    || {
        std::future::ready(Err(to_limiteron(&NexusFlowError::VendorUnavailable {
            vendor: "vendor_a".into(),
        })))
    }
}

fn ok_op() -> impl FnOnce() -> std::future::Ready<Result<u32, LimiteronError>> {
    || std::future::ready(Ok(1))
}

// ============ ① 5 次可重试失败 → Open ============

#[tokio::test]
async fn five_retryable_failures_open_the_circuit() {
    let (cb, _clock) = make_cb();

    for i in 0..5 {
        let result: Result<(), LimiteronError> = cb.execute(failing_op()).await;
        assert!(result.is_err(), "call {} should fail", i);
    }

    assert_eq!(cb.get_state().await, CircuitState::Open);

    // 第 6 次调用被拒绝且不执行操作（生产中 = 无 HTTP 调用）
    let executed = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let executed_c = executed.clone();
    let result: Result<(), LimiteronError> = cb
        .execute(move || {
            let executed = executed_c.clone();
            async move {
                executed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        executed.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "open circuit must reject without running the operation"
    );
}

/// 网关自身保护类错误（RateLimited）不计失败 — 连续多次不触发熔断。
#[tokio::test]
async fn rate_limited_errors_do_not_trip_the_circuit() {
    let (cb, _clock) = make_cb();

    for _ in 0..10 {
        let result: Result<(), LimiteronError> = cb
            .execute(|| {
                std::future::ready(Err(to_limiteron(&NexusFlowError::RateLimited {
                    vendor: "vendor_a".into(),
                })))
            })
            .await;
        assert!(result.is_err());
    }

    assert_eq!(
        cb.get_state().await,
        CircuitState::Closed,
        "RateLimited 载荷不计失败，不应熔断"
    );
}

// ============ recId=1：slow-call-rate 语义隔离证明 ============

/// limiteron 慢调用参数语义证明（rate_threshold=1.0 下「非全慢」不熔断）：
/// duration_threshold=100ms 时 1 快（0ms）+ 2 慢（800ms）→ 慢调用率依次
/// 0/1、1/2、2/3，全程严格 < 1.0 → Closed；同序列在旧默认 rate=0.5
/// （../limiteron src/constants.rs:111）下 1/2 = 0.5 即 Open。
///
/// 顺序说明（以源码为准，与 plan 假设的差异）：limiteron
/// check_slow_call_rate（src/circuit/types.rs:596-607）是逐调用判定、无
/// 最小样本数——total=1 时 rate 恒 1.0，故「慢调用先到」的序列第 1 次
/// 即 Open，混合态无法达成；本测试因此让快调用先到，使慢调用率在
/// 整个序列保持严格 < 1.0，这正是 rate=1.0 的判定边界。
///
/// 证明力边界（如实声明）：本测试证明 limiteron 参数语义；生产接线使用
/// 修复后参数由 manager.rs #[cfg(test)] 的
/// slow_ok_calls_do_not_open_production_circuit 证明；两者合一才完整。
/// 「首调用即 >duration_threshold」的 1/1=1.0 残余触发场景与 plan risk
/// 声明一致（生产主防线是 duration=60s：LLM 常态成功调用远低于它）。
///
/// MockClock 无法放大 execute() 实测时长（limiteron src/circuit/types.rs
/// 的 elapsed 取真实墙钟），真睡眠 2×800ms 是必要成本。
#[tokio::test]
async fn slow_call_rate_threshold_one_requires_all_slow() {
    let clock = Arc::new(limiteron::MockClock::new());
    let clock_dyn: Arc<dyn limiteron::Clock> = clock.clone();
    let config = CircuitBreakerConfig::new(5, 3, Duration::from_secs(30))
        .slow_call_duration_threshold(Duration::from_millis(100))
        .slow_call_rate_threshold(1.0)
        .error_classifier(Arc::new(NexusFlowErrorClassifier));
    let cb = CircuitBreaker::with_clock(config, clock_dyn);

    // 快调用先到：rate 0/1 = 0.0
    let result: Result<u32, LimiteronError> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    // 随后 2 次慢调用：rate 1/2 = 0.5、2/3 ≈ 0.67，全程 < 1.0
    for i in 0..2 {
        let result: Result<u32, LimiteronError> = cb
            .execute(|| async {
                tokio::time::sleep(Duration::from_millis(800)).await;
                Ok(1)
            })
            .await;
        assert!(result.is_ok(), "slow call {} should succeed", i);
    }

    assert_eq!(
        cb.get_state().await,
        CircuitState::Closed,
        "rate=1.0：2/3 慢（0.67 < 1.0）不得熔断；旧默认 0.5 会 Open"
    );
}

// ============ ② 死锁回归：超时后探针连续成功 3 次 → Closed ============

#[tokio::test]
async fn half_open_recovers_to_closed_after_three_probe_successes() {
    let (cb, clock) = make_cb();

    for _ in 0..5 {
        let _: Result<(), LimiteronError> = cb.execute(failing_op()).await;
    }
    assert_eq!(cb.get_state().await, CircuitState::Open);

    // 推进假时钟越过 open_timeout（30s）
    clock.advance(Duration::from_secs(31));

    // 探针 1：Open → HalfOpen 转换并成功
    let result: Result<u32, LimiteronError> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    assert_eq!(cb.get_state().await, CircuitState::HalfOpen);

    // 探针 2、3 连续成功：success_count 达到阈值 3 → Closed
    // （手写版在 half_open_max_calls=1 下永久卡死于此场景）
    let result: Result<u32, LimiteronError> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    let result: Result<u32, LimiteronError> = cb.execute(ok_op()).await;
    assert!(result.is_ok());

    assert_eq!(
        cb.get_state().await,
        CircuitState::Closed,
        "3 次探针成功后必须收敛回 Closed（死锁回归）"
    );
}

// ============ ③ 探针失败 → 立即回 Open ============

#[tokio::test]
async fn failing_probe_reopens_immediately() {
    let (cb, clock) = make_cb();

    for _ in 0..5 {
        let _: Result<(), LimiteronError> = cb.execute(failing_op()).await;
    }
    assert_eq!(cb.get_state().await, CircuitState::Open);

    clock.advance(Duration::from_secs(31));

    // 超时后的首个探针失败 → 立即重新熔断
    let result: Result<(), LimiteronError> = cb.execute(failing_op()).await;
    assert!(result.is_err());
    assert_eq!(
        cb.get_state().await,
        CircuitState::Open,
        "探针失败应立即回 Open"
    );
}

// ============ ④ 映射层单测 ============

/// 断言两个 NexusFlowError 同 variant 且字段一致（NexusFlowError 未派生
/// PartialEq；Protocol 的 inner 源链不参与比较）。
fn assert_same_error(a: &NexusFlowError, b: &NexusFlowError) {
    match (a, b) {
        (
            NexusFlowError::VendorUnavailable { vendor: x },
            NexusFlowError::VendorUnavailable { vendor: y },
        ) => {
            assert_eq!(x, y)
        }
        (NexusFlowError::Timeout { vendor: x }, NexusFlowError::Timeout { vendor: y }) => {
            assert_eq!(x, y)
        }
        (NexusFlowError::RateLimited { vendor: x }, NexusFlowError::RateLimited { vendor: y }) => {
            assert_eq!(x, y)
        }
        (NexusFlowError::Validation { field: x }, NexusFlowError::Validation { field: y }) => {
            assert_eq!(x, y)
        }
        (
            NexusFlowError::Protocol { source: x, .. },
            NexusFlowError::Protocol { source: y, .. },
        ) => assert_eq!(x, y),
        (NexusFlowError::Config { message: x }, NexusFlowError::Config { message: y }) => {
            assert_eq!(x, y)
        }
        (NexusFlowError::Database { message: x }, NexusFlowError::Database { message: y }) => {
            assert_eq!(x, y)
        }
        (NexusFlowError::Routing { message: x }, NexusFlowError::Routing { message: y }) => {
            assert_eq!(x, y)
        }
        _ => panic!("variant mismatch: {:?} vs {:?}", a, b),
    }
}

#[test]
fn to_and_from_limiteron_roundtrip_all_variants() {
    let cases = vec![
        NexusFlowError::VendorUnavailable {
            vendor: "mock-openai (500: Internal Server Error)".into(),
        },
        NexusFlowError::Timeout {
            vendor: "mock-anthropic".into(),
        },
        NexusFlowError::RateLimited {
            vendor: "mock-openai".into(),
        },
        NexusFlowError::Validation {
            field: "model".into(),
        },
        NexusFlowError::Protocol {
            source: "detail: with: colons".into(),
            inner: None,
        },
        NexusFlowError::Config {
            message: "bad [retry] section".into(),
        },
        NexusFlowError::Database {
            message: "sqlite busy".into(),
        },
        NexusFlowError::Routing {
            message: "no vendor".into(),
        },
    ];

    for original in &cases {
        let mapped = to_limiteron(original);
        // 编码形态：带前缀的 Other
        match &mapped {
            LimiteronError::Other(payload) => {
                assert!(
                    payload.starts_with("nexusflow:"),
                    "payload should carry nexusflow: prefix, got: {}",
                    payload
                );
            }
            other => panic!("to_limiteron should produce Other, got {:?}", other),
        }
        let restored = from_limiteron(mapped, "vendor_a");
        assert_same_error(original, &restored);
    }
}

#[test]
fn from_limiteron_limit_error_maps_to_circuit_open() {
    // CB Open / half-open 准入超限的拒绝串 → "…(circuit open)"
    let err = from_limiteron(
        LimiteronError::LimitError("Circuit breaker open, request rejected".into()),
        "vendor_a",
    );
    match &err {
        NexusFlowError::VendorUnavailable { vendor } => {
            assert_eq!(vendor, "vendor_a (circuit open)");
        }
        other => panic!("expected VendorUnavailable, got {:?}", other),
    }
    assert!(is_circuit_open_error(&err));
    assert!(!is_circuit_open_error(&NexusFlowError::Timeout {
        vendor: "vendor_a".into()
    }));
}

#[test]
fn from_limiteron_degrades_unrecognizable_payloads() {
    // 带前缀但 variant 无法识别 → 降级 VendorUnavailable
    let err = from_limiteron(
        LimiteronError::Other("nexusflow:unknown_variant:boom".into()),
        "vendor_a",
    );
    match &err {
        NexusFlowError::VendorUnavailable { vendor } => {
            assert!(
                vendor.starts_with("vendor_a (circuit error:"),
                "degraded error should keep vendor id, got: {}",
                vendor
            );
            assert!(vendor.contains("unknown_variant:boom"));
        }
        other => panic!("expected degraded VendorUnavailable, got {:?}", other),
    }
    assert!(!is_circuit_open_error(&err));

    // 完全无关的库内错误 → 降级
    let err = from_limiteron(LimiteronError::BanError("banned".into()), "vendor_a");
    assert!(matches!(err, NexusFlowError::VendorUnavailable { .. }));
    assert!(!is_circuit_open_error(&err));
}

#[test]
fn classifier_counts_failures_by_variant_semantics() {
    let classifier = NexusFlowErrorClassifier;
    // 上游故障类：计失败
    assert!(
        classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::VendorUnavailable {
            vendor: "v".into()
        }))
    );
    assert!(
        classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::Timeout {
            vendor: "v".into()
        }))
    );
    // 网关自身保护 / 客户端侧：不计失败
    assert!(
        !classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::RateLimited {
            vendor: "v".into()
        }))
    );
    assert!(
        !classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::Validation {
            field: "model".into()
        }))
    );
    assert!(
        !classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::Protocol {
            source: "bad body".into(),
            inner: None,
        }))
    );
    // 保守计失败：config/database/routing 与未知载荷
    assert!(
        classifier.is_counted_as_failure(&to_limiteron(&NexusFlowError::Config {
            message: "x".into()
        }))
    );
    assert!(classifier.is_counted_as_failure(&LimiteronError::Other("alien".into())));
    // 库自身保护机制错误：不计失败（对齐 DefaultErrorClassifier）
    assert!(!classifier.is_counted_as_failure(&LimiteronError::LimitError("rejected".into())));
    assert!(!classifier.is_counted_as_failure(&LimiteronError::CircuitBreakerError("open".into())));
}
