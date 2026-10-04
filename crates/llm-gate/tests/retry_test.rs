// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Integration tests for `llm_gate::utils::retry` module + recId=7 重试升级。
//!
//! 测试覆盖：
//! - `ErrorCategory`、`RetryStrategy`、`BackoffStrategy`、`extract_status_code`、
//!   `is_retryable_error`（无 feature 门控）
//! - `with_retry`（手写实现，重试延迟已迁 limiteron）
//! - recId=7①：`limiteron::retry::delay_for_attempt` 首试 1s（回归旧实现的
//!   2s 起步偏移）
//! - recId=7②：`retry_delay_for` 纯函数（Retry-After 覆盖 / 分类退避），
//!   不使用 tokio::time::pause（在库先例 vendor_circuit_breaker_test.rs:9-14
//!   已记录 paused clock auto-advance 与真实 mockito IO 冲突，禁用该组合）
//!
//! （recId=7⑤ 换 vendor fallback E2E 留 nexusflow tests/retry_test.rs——
//! 经 api/ApiState 全栈驱动，属应用面。）

#![allow(dead_code)]
mod common;
#[cfg(test)]
mod tests {
    use llm_gate::error::GateError;
    use llm_gate::utils::{
        extract_status_code, is_retryable_error, BackoffStrategy, ErrorCategory, RetryStrategy,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // -------- ErrorCategory --------

    #[test]
    fn test_error_category_from_status_code_fatal() {
        assert_eq!(ErrorCategory::from_status_code(400), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(401), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(403), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(404), ErrorCategory::Fatal);
    }

    #[test]
    fn test_error_category_from_status_code_retryable_exponential() {
        assert_eq!(
            ErrorCategory::from_status_code(429),
            ErrorCategory::RetryableExponential
        );
    }

    #[test]
    fn test_error_category_from_status_code_retryable_fixed() {
        assert_eq!(
            ErrorCategory::from_status_code(500),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(502),
            ErrorCategory::RetryableFixed
        );
        assert_eq!(
            ErrorCategory::from_status_code(503),
            ErrorCategory::RetryableFixed
        );
    }

    #[test]
    fn test_error_category_from_status_code_unknown_defaults_to_fatal() {
        assert_eq!(ErrorCategory::from_status_code(418), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(999), ErrorCategory::Fatal);
    }

    #[test]
    fn test_error_category_from_error_message_timeout() {
        let cat = ErrorCategory::from_error_message("Request timeout");
        assert_eq!(cat, ErrorCategory::RetryableShort);

        let cat = ErrorCategory::from_error_message("Connection timed out");
        assert_eq!(cat, ErrorCategory::RetryableShort);
    }

    #[test]
    fn test_error_category_is_retryable() {
        assert!(!ErrorCategory::Fatal.is_retryable());
        assert!(ErrorCategory::RetryableExponential.is_retryable());
        assert!(ErrorCategory::RetryableFixed.is_retryable());
        assert!(ErrorCategory::RetryableShort.is_retryable());
    }

    // -------- RetryStrategy / BackoffStrategy（存量锁定） --------

    #[test]
    fn test_retry_strategy_for_exponential_category() {
        let strategy = RetryStrategy::for_category(ErrorCategory::RetryableExponential);
        assert_eq!(strategy.max_retries, 5);
        match strategy.backoff {
            BackoffStrategy::Exponential { base_ms, max_ms } => {
                assert_eq!(base_ms, 1000);
                assert_eq!(max_ms, 16000);
            }
            _ => panic!("expected Exponential backoff"),
        }
    }

    #[test]
    fn test_backoff_fixed_delay_constant() {
        let backoff = BackoffStrategy::Fixed { interval_ms: 1000 };
        assert_eq!(backoff.delay(0), Duration::from_millis(1000));
        assert_eq!(backoff.delay(10), Duration::from_millis(1000));
    }

    // -------- extract_status_code / is_retryable_error --------

    #[test]
    fn test_extract_status_code_with_paren_format() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
    }

    #[test]
    fn test_extract_status_code_no_status_returns_default_500() {
        assert_eq!(extract_status_code("no status code here"), 500);
    }

    #[test]
    fn test_is_retryable_error_variants() {
        assert!(is_retryable_error(&GateError::Timeout {
            vendor: "v".into()
        }));
        assert!(is_retryable_error(&GateError::VendorUnavailable {
            vendor: "v (429: Rate limit)".into()
        }));
        assert!(!is_retryable_error(&GateError::VendorUnavailable {
            vendor: "v (400: Bad Request)".into()
        }));
        assert!(!is_retryable_error(&GateError::Config {
            message: "x".into()
        }));
    }

    // -------- with_retry（手写实现，延迟计算已迁 limiteron） --------

    #[tokio::test]
    async fn test_with_retry_success_first_attempt_no_initial_error() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, GateError> = llm_gate::utils::retry::with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(42)
                }
            },
            None,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_with_retry_succeeds_after_retryable_failures() {
        let initial = GateError::Timeout {
            vendor: "test_vendor".into(),
        };
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, GateError> = llm_gate::utils::retry::with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n < 1 {
                        Err(GateError::Timeout {
                            vendor: "test_vendor".into(),
                        })
                    } else {
                        Ok(42)
                    }
                }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    // -------- recId=7①：delay_for_attempt 首试偏移回归 --------

    /// 旧实现「先 retry_count += 1 再 2^retry_count」导致首试 2s，
    /// 与注释 '1s, 2s, 4s' 矛盾。limiteron delay_for_attempt 的 attempt
    /// 从 1 起：attempt=1 → initial × factor^0 = 1s。
    #[cfg(feature = "vendor-management")]
    #[test]
    fn test_delay_for_attempt_first_attempt_is_initial_delay() {
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                1,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(1),
            "首试延迟必须是 initial（1s），回归旧实现 2s 起步偏移"
        );
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                2,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(2)
        );
        assert_eq!(
            limiteron::retry::delay_for_attempt(
                5,
                Duration::from_secs(1),
                2.0,
                Duration::from_secs(16),
                0.0
            ),
            Duration::from_secs(16),
            "指数退避应封顶 max_delay"
        );
    }

    // -------- recId=7②：retry_delay_for 纯函数（无 IO、无 paused clock） --------

    #[cfg(feature = "vendor-management")]
    mod retry_delay_for_fn {
        use super::*;
        use llm_gate::config::RetrySection;
        use llm_gate::utils::retry::{retry_delay_for, RetryDelayPolicy};
        use rand::SeedableRng;

        /// spread_jitter=0 的存量断言不消耗 RNG；种子任意（固定以满足可复现）。
        fn legacy_rng() -> rand::rngs::StdRng {
            rand::rngs::StdRng::seed_from_u64(0)
        }

        fn deterministic_policy() -> RetryDelayPolicy {
            RetryDelayPolicy {
                initial_delay: Duration::from_secs(1),
                factor: 2.0,
                max_delay: Duration::from_secs(16),
                jitter: 0.0,        // 确定性：便于纯函数断言
                spread_jitter: 0.0, // recId=5 回滚阀：存量断言锁定旧确定性语义
            }
        }

        #[test]
        fn retry_after_header_takes_precedence() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(Duration::from_secs(7)),
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(
                delay,
                Duration::from_secs(7),
                "429 带 Retry-After(7s) → 直接采用上游指示"
            );
        }

        #[test]
        fn exponential_category_uses_delay_for_attempt() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                2,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(
                delay,
                Duration::from_secs(2),
                "429 无头 → delay_for_attempt 值（attempt=2 → 2s）"
            );
        }

        #[test]
        fn fixed_category_uses_constant_one_second() {
            let delay = retry_delay_for(
                ErrorCategory::RetryableFixed,
                3,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(delay, Duration::from_secs(1), "5xx → 固定 1s");
        }

        #[test]
        fn fatal_category_zero_delay() {
            let delay = retry_delay_for(
                ErrorCategory::Fatal,
                1,
                None,
                &deterministic_policy(),
                &mut legacy_rng(),
            );
            assert_eq!(delay, Duration::ZERO);
        }

        #[test]
        fn policy_from_config_section() {
            let section = RetrySection {
                initial_delay_ms: 250,
                factor: 3.0,
                max_delay_ms: 4000,
                jitter: 0.1,
                spread_jitter: 0.7,
                ..Default::default()
            };
            let policy = RetryDelayPolicy::from_config(&section);
            assert_eq!(policy.initial_delay, Duration::from_millis(250));
            assert!((policy.factor - 3.0).abs() < 1e-9);
            assert_eq!(policy.max_delay, Duration::from_millis(4000));
            assert!((policy.jitter - 0.1).abs() < 1e-9);
            assert!((policy.spread_jitter - 0.7).abs() < 1e-9);
        }

        /// [retry].spread_jitter 缺省必须为 1.0（修复本体：全范围展宽即
        /// 惊群修复），[retry] 段缺省也必须带出该值。
        #[test]
        fn retry_section_default_spread_jitter_is_one() {
            let section = RetrySection::default();
            assert!((section.spread_jitter - 1.0).abs() < 1e-9);
            let policy = RetryDelayPolicy::from_config(&section);
            assert!((policy.spread_jitter - 1.0).abs() < 1e-9);
        }

        // ======== recId=5：同步惊群展宽 ========

        /// spread_jitter=1.0（默认）下 Retry-After 分支必须以 ra 为 base
        /// 全范围展宽 ∈ [ra, 2×ra)（半开：gen_range(0.0..spread) 永不取上
        /// 界）。修复前恒等于 ra——上游同一 Retry-After 下所有并发在同一
        /// 刻唤醒重试（同步惊群源，Red 实测：delay == 7s）。
        ///
        /// 固定种子（StdRng::seed_from_u64）注入，确定性断言精确值。
        #[test]
        fn retry_after_gets_full_range_spread() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);
            let ra = Duration::from_secs(7);

            let delay = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(ra),
                &policy,
                &mut rng,
            );

            assert!(
                delay >= ra && delay < ra + ra,
                "Retry-After 分支必须展宽到 [ra, 2×ra)，got {:?}",
                delay
            );
            assert!(
                delay > ra,
                "展宽后必须 > ra（修复前恒 == ra），got {:?}",
                delay
            );
        }

        /// spread_jitter=1.0 下 RetryableFixed（5xx）分支必须以 1s 为 base
        /// 展宽 ∈ [1s, 2s)。修复前恒 1s——5xx 惊群下所有实例同步重试
        /// （Red 实测：delay == 1s）。
        #[test]
        fn retryable_fixed_gets_full_range_spread() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);

            let delay = retry_delay_for(ErrorCategory::RetryableFixed, 3, None, &policy, &mut rng);

            assert!(
                delay > Duration::from_secs(1) && delay <= Duration::from_secs(2),
                "5xx 固定间隔分支必须展宽到 (1s, 2s]，got {:?}（修复前恒 = 1s）",
                delay
            );
        }

        /// 回滚阀锁定：spread_jitter=0 完全恢复旧行为——Retry-After 原样
        /// 返回、5xx 固定 1s（且不消耗 RNG：两分支确定性，与旧实现逐位
        /// 一致）。
        #[test]
        fn spread_jitter_zero_restores_legacy_delay() {
            let policy = deterministic_policy(); // spread_jitter = 0.0
            let mut rng = rand::rngs::StdRng::seed_from_u64(42);

            let delay_after = retry_delay_for(
                ErrorCategory::RetryableExponential,
                1,
                Some(Duration::from_secs(7)),
                &policy,
                &mut rng,
            );
            assert_eq!(
                delay_after,
                Duration::from_secs(7),
                "spread_jitter=0：Retry-After 必须原样返回（旧行为）"
            );

            let delay_fixed =
                retry_delay_for(ErrorCategory::RetryableFixed, 3, None, &policy, &mut rng);
            assert_eq!(
                delay_fixed,
                Duration::from_secs(1),
                "spread_jitter=0：5xx 固定间隔必须恒 1s（旧行为）"
            );
        }

        /// 固定种子精确值断言：同种子产出与 RNG 序列一致（漂移检测），
        /// 不同种子产出不同延迟（证明 RNG 真实参与，非伪注入）。
        #[test]
        fn spread_delay_is_seed_deterministic_and_seed_sensitive() {
            let mut policy = deterministic_policy();
            policy.spread_jitter = 1.0;

            let mut rng42 = rand::rngs::StdRng::seed_from_u64(42);
            let d42 = retry_delay_for(ErrorCategory::RetryableFixed, 1, None, &policy, &mut rng42);
            // StdRng(42).gen_range(0.0..1.0) 的值由 Cargo.lock 固定的 rand
            // 版本决定；此处锁定该精确值（重锁依赖导致漂移时显式暴露）。
            assert_eq!(
                d42,
                Duration::from_nanos(1_526_557_409),
                "固定种子精确值漂移：rand 版本或实现变更，需重新固化"
            );

            let mut rng7 = rand::rngs::StdRng::seed_from_u64(7);
            let d7 = retry_delay_for(ErrorCategory::RetryableFixed, 1, None, &policy, &mut rng7);
            assert_ne!(d42, d7, "不同种子必须产生不同延迟（证明 RNG 真实参与）");
        }
    }
}

// ============ 覆盖补缺：分类边界 / 策略构造 / with_retry 错误分支 ============

#[cfg(test)]
mod coverage {
    use llm_gate::error::GateError;
    use llm_gate::utils::extract_status_code;
    use llm_gate::utils::retry::{
        with_retry, BackoffStrategy, ErrorCategory, RetryDelayPolicy, RetryStrategy,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // -------- ErrorCategory 边界 --------

    /// 网络错误关键词的完整覆盖：refused / reset 同判 Short；非网络错误 Fatal。
    #[test]
    fn error_message_connection_variants_are_short_retryable_and_others_fatal() {
        assert_eq!(
            ErrorCategory::from_error_message("Connection refused by peer"),
            ErrorCategory::RetryableShort
        );
        assert_eq!(
            ErrorCategory::from_error_message("CONNECTION RESET"),
            ErrorCategory::RetryableShort,
            "关键词匹配必须大小写不敏感"
        );
        assert_eq!(
            ErrorCategory::from_error_message("invalid api key"),
            ErrorCategory::Fatal,
            "非网络错误不得重试"
        );
    }

    /// max_retries 与分类语义一一对应（PRD：Fatal 0 / 429 5 次 / 5xx 3 次 / 超时 2 次）。
    #[test]
    fn max_retries_matches_category_semantics() {
        assert_eq!(ErrorCategory::Fatal.max_retries(), 0);
        assert_eq!(ErrorCategory::RetryableExponential.max_retries(), 5);
        assert_eq!(ErrorCategory::RetryableFixed.max_retries(), 3);
        assert_eq!(ErrorCategory::RetryableShort.max_retries(), 2);
    }

    /// Fatal 策略：零重试 + 无退避。
    #[test]
    fn strategy_for_fatal_is_zero_retry_with_no_backoff() {
        let s = RetryStrategy::for_category(ErrorCategory::Fatal);
        assert_eq!(s.max_retries, 0);
        assert!(matches!(s.backoff, BackoffStrategy::None));
    }

    /// 5xx 策略：3 次固定 1s 退避（延迟与尝试次数无关）。
    #[test]
    fn strategy_for_fixed_is_three_retries_at_one_second() {
        let s = RetryStrategy::for_category(ErrorCategory::RetryableFixed);
        assert_eq!(s.max_retries, 3);
        assert_eq!(s.backoff.delay(0), Duration::from_millis(1000));
        assert_eq!(s.backoff.delay(2), Duration::from_millis(1000));
    }

    /// 无退避策略延迟恒 0（调用方跳过 sleep）。
    #[test]
    fn backoff_none_yields_zero_delay() {
        assert_eq!(BackoffStrategy::None.delay(0), Duration::ZERO);
        assert_eq!(BackoffStrategy::None.delay(3), Duration::ZERO);
    }

    // -------- extract_status_code 括号内无冒号形态 --------

    /// "(429 rate)" 无冒号 → 空白分词取首个 token 直接解析数字；
    /// 非数字 / 空括号 / 数字紧黏右括号（"429)" 非法 u16）回落 500。
    #[test]
    fn extract_status_code_supports_paren_number_without_colon() {
        assert_eq!(extract_status_code("vendor (429 rate)"), 429);
        assert_eq!(
            extract_status_code("vendor (abc)"),
            500,
            "非数字回落默认 500"
        );
        assert_eq!(extract_status_code("vendor ("), 500, "空括号回落默认");
        assert_eq!(
            extract_status_code("vendor (429)"),
            500,
            "右括号黏在数字后不是合法 u16，必须回落默认而非 panic"
        );
    }

    // -------- RetryDelayPolicy 缺省值锁定 --------

    /// [retry] 段缺省快照：1s / ×2 / 16s 封顶 / 0.2 抖动 / 1.0 展宽。
    #[test]
    fn retry_delay_policy_defaults_match_doc() {
        let p = RetryDelayPolicy::default();
        assert_eq!(p.initial_delay, Duration::from_secs(1));
        assert!((p.factor - 2.0).abs() < 1e-12);
        assert_eq!(p.max_delay, Duration::from_secs(16));
        assert!((p.jitter - 0.2).abs() < 1e-12);
        assert!((p.spread_jitter - 1.0).abs() < 1e-12);
    }

    // -------- with_retry 错误分支（paused clock：sleep 虚拟推进，无 IO） --------

    /// Fatal 初始错误（4xx VendorUnavailable）→ 不调用 f，直接返回初始错误。
    #[tokio::test(start_paused = true)]
    async fn fatal_initial_vendor_error_skips_closure_and_returns_initial() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let initial = GateError::VendorUnavailable {
            vendor: "v (400: bad request)".into(),
        };

        let result: Result<(), GateError> = with_retry(
            || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "Fatal 初始错误不得调用 f");
        assert!(matches!(
            result.unwrap_err(),
            GateError::VendorUnavailable { .. }
        ));
    }

    /// 非 VendorUnavailable/Timeout 的初始错误一律按 Fatal 处理。
    #[tokio::test(start_paused = true)]
    async fn non_vendor_initial_error_is_fatal() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();

        let result: Result<(), GateError> = with_retry(
            || {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
            Some(GateError::Config {
                message: "bad config".into(),
            }),
        )
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(result.unwrap_err(), GateError::Config { .. }));
    }

    /// 429 初始错误 → RetryableExponential 策略：可重试失败后重试成功，
    /// 闭包被再次调用。
    #[tokio::test(start_paused = true)]
    async fn rate_limited_initial_error_retries_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let initial = GateError::VendorUnavailable {
            vendor: "v (429: rate limited)".into(),
        };

        let result: Result<&'static str, GateError> = with_retry(
            || {
                let c = c.clone();
                async move {
                    if c.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(GateError::Timeout { vendor: "v".into() })
                    } else {
                        Ok("done")
                    }
                }
            },
            Some(initial),
        )
        .await;

        assert_eq!(result.unwrap(), "done");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "首败 + 重试成功");
    }

    /// 重试途中出现不可重试错误 → 立即停止返回该错误（不再消耗剩余次数）。
    #[tokio::test(start_paused = true)]
    async fn non_retryable_error_midway_stops_immediately() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let initial = GateError::Timeout { vendor: "v".into() };

        let result: Result<(), GateError> = with_retry(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(GateError::Config {
                        message: "permanent".into(),
                    })
                }
            },
            Some(initial),
        )
        .await;

        match result.unwrap_err() {
            GateError::Config { .. } => {}
            other => panic!("Expected Config error, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "Fatal 错误必须立即停止");
    }

    /// 重试耗尽：RetryableShort 上限 2 次 → 恰调用 2 次，返回最后一次错误。
    #[tokio::test(start_paused = true)]
    async fn retries_exhausted_returns_last_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let initial = GateError::Timeout { vendor: "v".into() };

        let result: Result<(), GateError> = with_retry(
            || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(GateError::Timeout { vendor: "v".into() })
                }
            },
            Some(initial),
        )
        .await;

        assert!(matches!(result.unwrap_err(), GateError::Timeout { .. }));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "RetryableShort 恰重试 2 次"
        );
    }
}
