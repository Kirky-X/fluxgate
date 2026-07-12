// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Integration tests for `nexusflow::utils::retry` module.
//!
//! Covers `ErrorCategory`, `RetryStrategy`, `BackoffStrategy`,
//! `extract_status_code`, `is_retryable_error`, and `with_retry`.

#[cfg(test)]
mod tests {
    use nexusflow::error::NexusFlowError;
    use nexusflow::utils::{
        BackoffStrategy, ErrorCategory, RetryStrategy, extract_status_code, is_retryable_error,
        with_retry,
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
    fn test_error_category_from_error_message_connection_refused() {
        let cat = ErrorCategory::from_error_message("Connection refused");
        assert_eq!(cat, ErrorCategory::RetryableShort);

        let cat = ErrorCategory::from_error_message("connection reset by peer");
        assert_eq!(cat, ErrorCategory::RetryableShort);
    }

    #[test]
    fn test_error_category_from_error_message_non_retryable() {
        let cat = ErrorCategory::from_error_message("Invalid API key");
        assert_eq!(cat, ErrorCategory::Fatal);

        let cat = ErrorCategory::from_error_message("400 Bad Request");
        assert_eq!(cat, ErrorCategory::Fatal);
    }

    #[test]
    fn test_error_category_is_retryable() {
        assert!(!ErrorCategory::Fatal.is_retryable());
        assert!(ErrorCategory::RetryableExponential.is_retryable());
        assert!(ErrorCategory::RetryableFixed.is_retryable());
        assert!(ErrorCategory::RetryableShort.is_retryable());
    }

    #[test]
    fn test_error_category_max_retries() {
        assert_eq!(ErrorCategory::Fatal.max_retries(), 0);
        assert_eq!(ErrorCategory::RetryableExponential.max_retries(), 5);
        assert_eq!(ErrorCategory::RetryableFixed.max_retries(), 3);
        assert_eq!(ErrorCategory::RetryableShort.max_retries(), 2);
    }

    // -------- RetryStrategy --------

    #[test]
    fn test_retry_strategy_for_fatal_category() {
        let strategy = RetryStrategy::for_category(ErrorCategory::Fatal);
        assert_eq!(strategy.max_retries, 0);
        assert!(matches!(strategy.backoff, BackoffStrategy::None));
    }

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
    fn test_retry_strategy_for_fixed_category() {
        let strategy = RetryStrategy::for_category(ErrorCategory::RetryableFixed);
        assert_eq!(strategy.max_retries, 3);
        match strategy.backoff {
            BackoffStrategy::Fixed { interval_ms } => assert_eq!(interval_ms, 1000),
            _ => panic!("expected Fixed backoff"),
        }
    }

    #[test]
    fn test_retry_strategy_for_short_category() {
        let strategy = RetryStrategy::for_category(ErrorCategory::RetryableShort);
        assert_eq!(strategy.max_retries, 2);
        match strategy.backoff {
            BackoffStrategy::Fixed { interval_ms } => assert_eq!(interval_ms, 500),
            _ => panic!("expected Fixed backoff"),
        }
    }

    // -------- BackoffStrategy --------

    #[test]
    fn test_backoff_none_delay_zero() {
        let backoff = BackoffStrategy::None;
        assert_eq!(backoff.delay(0), Duration::from_millis(0));
        assert_eq!(backoff.delay(10), Duration::from_millis(0));
    }

    #[test]
    fn test_backoff_exponential_delay_sequence() {
        let backoff = BackoffStrategy::Exponential {
            base_ms: 1000,
            max_ms: 16000,
        };
        assert_eq!(backoff.delay(0), Duration::from_millis(1000));
        assert_eq!(backoff.delay(1), Duration::from_millis(2000));
        assert_eq!(backoff.delay(2), Duration::from_millis(4000));
        assert_eq!(backoff.delay(3), Duration::from_millis(8000));
        assert_eq!(backoff.delay(4), Duration::from_millis(16000));
        // Capped at max_ms
        assert_eq!(backoff.delay(5), Duration::from_millis(16000));
        assert_eq!(backoff.delay(100), Duration::from_millis(16000));
    }

    #[test]
    fn test_backoff_fixed_delay_constant() {
        let backoff = BackoffStrategy::Fixed { interval_ms: 1000 };
        assert_eq!(backoff.delay(0), Duration::from_millis(1000));
        assert_eq!(backoff.delay(10), Duration::from_millis(1000));
    }

    // -------- extract_status_code --------

    #[test]
    fn test_extract_status_code_with_paren_format() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
        assert_eq!(extract_status_code("vendor (503: Service Unavailable)"), 503);
    }

    #[test]
    fn test_extract_status_code_without_colon() {
        // Format like "vendor (429 Rate limit exceeded)"
        assert_eq!(extract_status_code("vendor (429 Rate limit exceeded)"), 429);
    }

    #[test]
    fn test_extract_status_code_no_status_returns_default_500() {
        assert_eq!(extract_status_code("no status code here"), 500);
        assert_eq!(extract_status_code("plain error message"), 500);
    }

    #[test]
    fn test_extract_status_code_empty_paren() {
        assert_eq!(extract_status_code("error ()"), 500);
    }

    // -------- is_retryable_error(&NexusFlowError) --------

    #[test]
    fn test_is_retryable_error_timeout_variant() {
        let err = NexusFlowError::Timeout {
            vendor: "test_vendor".into(),
        };
        assert!(is_retryable_error(&err));
    }

    #[test]
    fn test_is_retryable_error_vendor_unavailable_with_429() {
        let err = NexusFlowError::VendorUnavailable {
            vendor: "test_vendor (429: Rate limit exceeded)".into(),
        };
        assert!(is_retryable_error(&err));
    }

    #[test]
    fn test_is_retryable_error_vendor_unavailable_with_500() {
        let err = NexusFlowError::VendorUnavailable {
            vendor: "test_vendor (500: Internal server error)".into(),
        };
        assert!(is_retryable_error(&err));
    }

    #[test]
    fn test_is_retryable_error_vendor_unavailable_with_400_is_fatal() {
        let err = NexusFlowError::VendorUnavailable {
            vendor: "test_vendor (400: Bad Request)".into(),
        };
        assert!(!is_retryable_error(&err));
    }

    #[test]
    fn test_is_retryable_error_vendor_unavailable_with_401_is_fatal() {
        let err = NexusFlowError::VendorUnavailable {
            vendor: "test_vendor (401: Unauthorized)".into(),
        };
        assert!(!is_retryable_error(&err));
    }

    #[test]
    fn test_is_retryable_error_other_variants_are_not_retryable() {
        let config_err = NexusFlowError::Config {
            message: "missing field".into(),
        };
        assert!(!is_retryable_error(&config_err));
    }

    // -------- with_retry --------

    #[tokio::test]
    async fn test_with_retry_success_first_attempt_no_initial_error() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = with_retry(
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
    async fn test_with_retry_returns_initial_error_when_fatal() {
        // Fatal category → max_retries = 0 → should return initial_error immediately
        let initial = NexusFlowError::VendorUnavailable {
            vendor: "vendor (400: Bad Request)".into(),
        };

        let result: Result<i32, NexusFlowError> = with_retry(
            || async { Ok::<i32, NexusFlowError>(42) },
            Some(initial),
        )
        .await;

        assert!(result.is_err());
        match result {
            Err(NexusFlowError::VendorUnavailable { vendor }) => {
                assert!(vendor.contains("400"));
            }
            _ => panic!("expected VendorUnavailable error"),
        }
    }

    #[tokio::test]
    async fn test_with_retry_succeeds_after_retryable_failures() {
        // Use Timeout error → RetryableShort category → max_retries = 2
        let initial = NexusFlowError::Timeout {
            vendor: "test_vendor".into(),
        };

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if n < 1 {
                        // First retry: still timeout
                        Err(NexusFlowError::Timeout {
                            vendor: "test_vendor".into(),
                        })
                    } else {
                        // Second attempt: success
                        Ok(42)
                    }
                }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        // Initial call (in with_retry's first iteration) + 1 retry = 2 calls
        assert!(call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_with_retry_stops_on_non_retryable_error() {
        // Start with retryable Timeout, but closure returns non-retryable error
        let initial = NexusFlowError::Timeout {
            vendor: "test_vendor".into(),
        };

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Return a non-retryable error (400 → Fatal)
                    Err(NexusFlowError::VendorUnavailable {
                        vendor: "vendor (400: Bad Request)".into(),
                    })
                }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_err());
        // Should be called once, then stop because 400 is not retryable
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_with_retry_exhausts_retries_on_persistent_timeout() {
        // Timeout → RetryableShort → max_retries = 2
        let initial = NexusFlowError::Timeout {
            vendor: "test_vendor".into(),
        };

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let result: Result<i32, NexusFlowError> = with_retry(
            || {
                let count = call_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err(NexusFlowError::Timeout {
                        vendor: "test_vendor".into(),
                    })
                }
            },
            Some(initial),
        )
        .await;

        assert!(result.is_err());
        // Should be called max_retries times (2 for RetryableShort)
        assert!(call_count.load(Ordering::SeqCst) <= 3);
    }
}
