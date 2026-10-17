// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 熔断器测试— 直接驱动 `limiteron::circuit::CircuitBreaker<GateError>` +
//! 注入假 Clock（手写三态机已删除：其 HalfOpen 死锁一次熔断即永久拒绝）。
//! 1. 5 次可重试失败 → Open（failure_threshold=5）
//! 2. 死锁回归：超时后探针连续成功 3 次 → Closed
//! 3. 探针失败 → 立即回 Open
//! 4. GateError 分类器 variant 语义；5. 业务错误无损穿透
//! 6. from_circuit_call：CB Open 拒绝 → `VendorUnavailable{(circuit open)}`
//! 7. 错误双轨：Display 恒英文规范串，user_message 走 FTL 随 locale 翻转

#![cfg(all(feature = "resilience", feature = "vendor-management"))]

use fluxgate::error::GateError;
use fluxgate::vendor::resilience::{from_circuit_call, is_circuit_open_error, GateErrorClassifier};
use limiteron::circuit::{
    CircuitBreaker, CircuitBreakerConfig, CircuitCallError, FailureClassifier,
};
use limiteron::error::CircuitState;
use std::sync::Arc;
use std::time::Duration;

/// failure_threshold=5 / success_threshold=3 / timeout=30s（生产同款参数，
/// 与 manager::gate_circuit_config 逐位一致），错误分类器同源
/// （GateErrorClassifier）。自定义 E=GateError 经 `with_error_classifier`
/// 构造（limiteron 的 `new` 仅在 LimiteronError 特化上提供），阈值经
/// pub 字段显式对齐旧 `new(5, 3, 30s)`。
fn gate_test_config() -> CircuitBreakerConfig<GateError> {
    let mut config = CircuitBreakerConfig::with_error_classifier(Arc::new(GateErrorClassifier))
        .slow_call_duration_threshold(Duration::from_secs(60))
        .slow_call_rate_threshold(1.0);
    config.failure_threshold = 5;
    config.success_threshold = 3;
    config.timeout = Duration::from_secs(30);
    config
}

fn make_cb() -> (CircuitBreaker<GateError>, Arc<limiteron::MockClock>) {
    let clock = Arc::new(limiteron::MockClock::new());
    let clock_dyn: Arc<dyn limiteron::Clock> = clock.clone();
    (
        CircuitBreaker::with_clock(gate_test_config(), clock_dyn),
        clock,
    )
}

/// 桩异步操作：返回上游不可用错误（计失败）
fn failing_op() -> impl FnOnce() -> std::future::Ready<Result<(), GateError>> {
    || {
        std::future::ready(Err(GateError::VendorUnavailable {
            vendor: "vendor_a".into(),
        }))
    }
}

fn ok_op() -> impl FnOnce() -> std::future::Ready<Result<u32, GateError>> {
    || std::future::ready(Ok(1))
}

// ============ ① 5 次可重试失败 → Open ============

#[tokio::test]
async fn five_retryable_failures_open_the_circuit() {
    let (cb, _clock) = make_cb();

    for i in 0..5 {
        let result: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
        assert!(result.is_err(), "call {} should fail", i);
    }

    assert_eq!(cb.get_state().await, CircuitState::Open);

    // 第 6 次调用被拒绝且不执行操作（生产中 = 无 HTTP 调用）
    let executed = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let executed_c = executed.clone();
    let result: Result<(), CircuitCallError<GateError>> = cb
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
        let result: Result<(), CircuitCallError<GateError>> = cb
            .execute(|| {
                std::future::ready(Err(GateError::RateLimited {
                    vendor: "vendor_a".into(),
                }))
            })
            .await;
        assert!(result.is_err());
    }

    assert_eq!(
        cb.get_state().await,
        CircuitState::Closed,
        "RateLimited 不计失败，不应熔断"
    );
}

/// 阈值下侧边界：失败数恰为 failure_threshold−1（4 次）→ 保持 Closed
/// 不提前熔断。每次失败必须经 `CircuitCallError::Inner` 透传（操作真实
/// 执行，而非 Open 拒绝）；第 5 次（恰为阈值）才翻 Open。
/// 与 ① 的「5 次 → Open」合成阈值两侧的完备边界。
#[tokio::test]
async fn threshold_minus_one_failures_stay_closed_and_keep_executing() {
    let (cb, _clock) = make_cb();

    for i in 0..4 {
        let result: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
        match result {
            Err(CircuitCallError::Inner(GateError::VendorUnavailable { vendor })) => {
                assert_eq!(vendor, "vendor_a", "失败 {i} 必须真实执行并透传原错误");
            }
            other => panic!(
                "failure {i}: threshold−1 下不得拒绝执行（应 Inner 透传），got {:?}",
                other.map(|_| ())
            ),
        }
    }

    assert_eq!(
        cb.get_state().await,
        CircuitState::Closed,
        "4 次（threshold−1）不得提前熔断"
    );

    // 第 5 次（恰为阈值）→ 本次仍执行，随后翻 Open
    let result: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
    assert!(
        matches!(result, Err(CircuitCallError::Inner(_))),
        "阈值次调用本身仍执行（拒绝从下一次开始）"
    );
    assert_eq!(cb.get_state().await, CircuitState::Open);
}

// ============ slow-call-rate 语义隔离证明 ============

/// limiteron 慢调用参数语义证明（rate_threshold=1.0 下「非全慢」不熔断）：
/// 1 快（0ms）+ 2 慢（800ms）→ 慢调用率全程严格 < 1.0 → Closed；
/// 旧默认 rate=0.5 下 1/2 即 Open。check_slow_call_rate 逐调用判定无最小
/// 样本数（total=1 时 rate 恒 1.0），故让快调用先到保持混合态。
/// 生产接线参数由 manager.rs 的 slow_ok_calls_do_not_open_production_circuit
/// 证明（两者合一才完整）；MockClock 无法放大实测时长，真睡眠是必要成本
#[tokio::test]
async fn slow_call_rate_threshold_one_requires_all_slow() {
    let clock = Arc::new(limiteron::MockClock::new());
    let clock_dyn: Arc<dyn limiteron::Clock> = clock.clone();
    let config = CircuitBreakerConfig::with_error_classifier(Arc::new(GateErrorClassifier))
        .slow_call_duration_threshold(Duration::from_millis(100))
        .slow_call_rate_threshold(1.0);
    let cb = CircuitBreaker::with_clock(config, clock_dyn);

    // 快调用先到：rate 0/1 = 0.0
    let result: Result<u32, CircuitCallError<GateError>> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    // 随后 2 次慢调用：rate 1/2 = 0.5、2/3 ≈ 0.67，全程 < 1.0
    for i in 0..2 {
        let result: Result<u32, CircuitCallError<GateError>> = cb
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
        let _: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
    }
    assert_eq!(cb.get_state().await, CircuitState::Open);

    // 推进假时钟越过 open_timeout（30s）
    clock.advance(Duration::from_secs(31));

    // 探针 1：Open → HalfOpen 转换并成功
    let result: Result<u32, CircuitCallError<GateError>> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    assert_eq!(cb.get_state().await, CircuitState::HalfOpen);

    // 探针 2、3 连续成功：success_count 达到阈值 3 → Closed
    // （手写版在 half_open_max_calls=1 下永久卡死于此场景）
    let result: Result<u32, CircuitCallError<GateError>> = cb.execute(ok_op()).await;
    assert!(result.is_ok());
    let result: Result<u32, CircuitCallError<GateError>> = cb.execute(ok_op()).await;
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
        let _: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
    }
    assert_eq!(cb.get_state().await, CircuitState::Open);

    clock.advance(Duration::from_secs(31));

    // 超时后的首个探针失败 → 立即重新熔断
    let result: Result<(), CircuitCallError<GateError>> = cb.execute(failing_op()).await;
    assert!(result.is_err());
    assert_eq!(
        cb.get_state().await,
        CircuitState::Open,
        "探针失败应立即回 Open"
    );
}

// ============ ④ 分类器 variant 语义（直判 GateError） ============

#[test]
fn classifier_counts_failures_by_variant_semantics() {
    let classifier = GateErrorClassifier;
    // 上游故障类：计失败
    assert!(
        classifier.is_counted_as_failure(&GateError::VendorUnavailable { vendor: "v".into() }),
        "VendorUnavailable（上游故障）必须计失败"
    );
    assert!(
        classifier.is_counted_as_failure(&GateError::Timeout { vendor: "v".into() }),
        "Timeout（上游故障）必须计失败"
    );
    // 网关自身保护 / 客户端侧：不计失败
    assert!(
        !classifier.is_counted_as_failure(&GateError::RateLimited { vendor: "v".into() }),
        "RateLimited 是网关自身保护信号，不计失败"
    );
    assert!(
        !classifier.is_counted_as_failure(&GateError::Validation {
            field: "model".into()
        }),
        "Validation 是客户端侧问题，不计失败"
    );
    assert!(
        !classifier.is_counted_as_failure(&GateError::Protocol {
            source: "bad body".into(),
            inner: None,
        }),
        "Protocol 是客户端侧问题，不计失败"
    );
    // 保守计失败：config / routing / all_vendors_throttled
    assert!(classifier.is_counted_as_failure(&GateError::Config {
        message: "x".into()
    }));
    assert!(classifier.is_counted_as_failure(&GateError::Routing {
        message: "x".into()
    }));
    assert!(
        classifier.is_counted_as_failure(&GateError::AllVendorsThrottled {
            retry_after_secs: 1
        })
    );
}

// ============ ⑤ 业务错误无损穿透熔断器（泛型收口） ============

/// 断言两个 GateError 同 variant 且字段一致（GateError 未派生
/// PartialEq；Protocol 的 inner 源链不参与比较）。
fn assert_same_error(a: &GateError, b: &GateError) {
    match (a, b) {
        (
            GateError::VendorUnavailable { vendor: x },
            GateError::VendorUnavailable { vendor: y },
        ) => {
            assert_eq!(x, y)
        }
        (GateError::Timeout { vendor: x }, GateError::Timeout { vendor: y }) => {
            assert_eq!(x, y)
        }
        (GateError::RateLimited { vendor: x }, GateError::RateLimited { vendor: y }) => {
            assert_eq!(x, y)
        }
        (GateError::Validation { field: x }, GateError::Validation { field: y }) => {
            assert_eq!(x, y)
        }
        (GateError::Protocol { source: x, .. }, GateError::Protocol { source: y, .. }) => {
            assert_eq!(x, y)
        }
        (GateError::Config { message: x }, GateError::Config { message: y }) => {
            assert_eq!(x, y)
        }
        (GateError::Routing { message: x }, GateError::Routing { message: y }) => {
            assert_eq!(x, y)
        }
        (
            GateError::AllVendorsThrottled {
                retry_after_secs: x,
            },
            GateError::AllVendorsThrottled {
                retry_after_secs: y,
            },
        ) => {
            assert_eq!(x, y)
        }
        _ => panic!("variant mismatch: {:?} vs {:?}", a, b),
    }
}

/// 泛型收口核心保证：GateError 直接作为 `CircuitBreaker<GateError>` 的
/// 错误类型，execute 出口（`CircuitCallError::Inner`）的错误变体与详情
/// 不变——删除前缀串双向编解码后错误无损传递。此前 `from_limiteron` 对
/// 「无法识别的载荷」有损降级为 `VendorUnavailable`，该风险路径已不复存在。
#[tokio::test]
async fn business_errors_pass_through_breaker_losslessly() {
    // GateError 无 Clone（Protocol 内含 Box<dyn Error>）——每用例经工厂
    // 构造两份：一份作为穿透输入，一份作为出口对照。
    let cases: Vec<Box<dyn Fn() -> GateError>> = vec![
        Box::new(|| GateError::VendorUnavailable {
            vendor: "mock-openai (500: Internal Server Error)".into(),
        }),
        Box::new(|| GateError::Timeout {
            vendor: "mock-anthropic".into(),
        }),
        Box::new(|| GateError::RateLimited {
            vendor: "mock-openai".into(),
        }),
        Box::new(|| GateError::Validation {
            field: "model".into(),
        }),
        Box::new(|| GateError::Protocol {
            source: "detail: with: colons".into(),
            inner: None,
        }),
        Box::new(|| GateError::Config {
            message: "bad [retry] section".into(),
        }),
        Box::new(|| GateError::Routing {
            message: "no vendor".into(),
        }),
        Box::new(|| GateError::AllVendorsThrottled {
            retry_after_secs: 7,
        }),
    ];

    for factory in cases {
        // 每变体独立熔断器：单次失败远低于阈值 5，全程保持 Closed——
        // 错误语义（计数与否）由 ④ 与 manager 侧集成测试承载
        let expected = factory();
        let cb = CircuitBreaker::<GateError>::with_dependencies(gate_test_config());
        let result: Result<(), CircuitCallError<GateError>> =
            cb.execute(move || std::future::ready(Err(factory()))).await;
        match result {
            Err(CircuitCallError::Inner(passthrough)) => {
                assert_same_error(&expected, &passthrough);
            }
            other => panic!(
                "业务错误必须经 Inner 原样透传，实际 {:?}（原始 {:?}）",
                other.map(|_| ()),
                expected
            ),
        }
        assert_eq!(cb.get_state().await, CircuitState::Closed);
    }
}

// ============ ⑥ from_circuit_call：CB Open 拒绝的还原形态 ============

#[test]
fn from_circuit_call_open_maps_to_circuit_open_form() {
    // CB Open / half-open 准入超限的拒绝 → "…(circuit open)"
    let err = from_circuit_call(CircuitCallError::Open, "vendor_a");
    match &err {
        GateError::VendorUnavailable { vendor } => {
            assert_eq!(vendor, "vendor_a (circuit open)");
        }
        other => panic!("expected VendorUnavailable, got {:?}", other),
    }
    assert!(is_circuit_open_error(&err));
    assert!(!is_circuit_open_error(&GateError::Timeout {
        vendor: "vendor_a".into()
    }));
}

/// Inner 臂原样透传（与 ⑤ 的 execute 出口同语义的直接单元证明）。
#[test]
fn from_circuit_call_inner_is_lossless() {
    let err = from_circuit_call(
        CircuitCallError::Inner(GateError::Timeout {
            vendor: "vendor_a".into(),
        }),
        "vendor_a",
    );
    assert_same_error(
        &GateError::Timeout {
            vendor: "vendor_a".into(),
        },
        &err,
    );
}
