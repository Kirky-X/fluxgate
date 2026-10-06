// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Integration tests for `nexusflow::utils::retry` module + recId=7 重试升级。
//!
//! 测试覆盖：
//! - `ErrorCategory`、`extract_status_code`（无 feature 门控）
//! - recId=7①：`limiteron::retry::delay_for_attempt` 首试 1s（回归旧实现的
//!   2s 起步偏移）
//! - recId=7②：`retry_delay_for` 纯函数（分类退避）与 `spread_delay`
//!   （Retry-After 值的展宽——G3 后经 RetryDecision::RetryAfter 消费），
//!   不使用 tokio::time::pause（在库先例 vendor_circuit_breaker_test.rs:9-14
//!   已记录 paused clock auto-advance 与真实 mockito IO 冲突，禁用该组合）
//!
//! （G3 审查修复：第二份手写实现 `with_retry`/`RetryStrategy`/
//! `BackoffStrategy`/`is_retryable_error` 已删除——生产调用方为零；
//! vendor HTTP 主循环的行为锁定在 vendor_test.rs 的 mockito 集成测试。）
//!
//! （recId=7⑤ 换 vendor fallback E2E 留 nexusflow tests/retry_test.rs——
//! 经 api/ApiState 全栈驱动，属应用面。）

mod tests {
    use nexusflow::utils::{extract_status_code, ErrorCategory};
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

    // -------- extract_status_code（fallback.rs 生产消费的存量锁定） --------

    #[test]
    fn test_extract_status_code_with_paren_format() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
    }

    #[test]
    fn test_extract_status_code_no_status_returns_default_500() {
        assert_eq!(extract_status_code("no status code here"), 500);
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

    // -------- recId=7② + G3：retry_delay_for 纯函数（分类退避）与
    // spread_delay（Retry-After → RetryDecision::RetryAfter 的时长生产端）--------

    #[cfg(feature = "vendor-management")]
    mod retry_delay_for_fn {
        use super::*;
        use nexusflow::config::RetrySection;
        use nexusflow::utils::retry::{retry_delay_for, spread_delay, RetryDelayPolicy};
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

        /// 等价迁移（原 retry_after_header_takes_precedence）：上游
        /// `Retry-After` 值的时长生产端在 spread_jitter=0 时原样采用上游
        /// 指示（manager 决策闭包将其映射为 RetryDecision::RetryAfter）。
        #[test]
        fn retry_after_spread_zero_is_upstream_passthrough() {
            let delay = spread_delay(
                Duration::from_secs(7),
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

        /// spread_jitter=1.0（默认）下 Retry-After 值必须以 ra 为 base
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

            let delay = spread_delay(ra, &policy, &mut rng);

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

            let delay = retry_delay_for(ErrorCategory::RetryableFixed, 3, &policy, &mut rng);

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

            let delay_after = spread_delay(Duration::from_secs(7), &policy, &mut rng);
            assert_eq!(
                delay_after,
                Duration::from_secs(7),
                "spread_jitter=0：Retry-After 必须原样返回（旧行为）"
            );

            let delay_fixed = retry_delay_for(ErrorCategory::RetryableFixed, 3, &policy, &mut rng);
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
            let d42 = retry_delay_for(ErrorCategory::RetryableFixed, 1, &policy, &mut rng42);
            // StdRng(42).gen_range(0.0..1.0) 的值由 Cargo.lock 固定的 rand
            // 版本决定；此处锁定该精确值（重锁依赖导致漂移时显式暴露）。
            assert_eq!(
                d42,
                Duration::from_nanos(1_526_557_409),
                "固定种子精确值漂移：rand 版本或实现变更，需重新固化"
            );

            let mut rng7 = rand::rngs::StdRng::seed_from_u64(7);
            let d7 = retry_delay_for(ErrorCategory::RetryableFixed, 1, &policy, &mut rng7);
            assert_ne!(d42, d7, "不同种子必须产生不同延迟（证明 RNG 真实参与）");
        }
    }
}

// ============ 覆盖补缺：分类边界 / 状态码解析 / 策略缺省 ============

#[cfg(test)]
mod coverage {
    use nexusflow::utils::extract_status_code;
    use nexusflow::utils::retry::{ErrorCategory, RetryDelayPolicy};
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
}
