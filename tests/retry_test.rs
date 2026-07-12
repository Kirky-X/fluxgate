// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

#[cfg(test)]
mod tests {
    use nexusflow::utils::{RetryConfig, is_retryable_error, retry, retry_with_config};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 30000);
        assert_eq!(config.exponential_base, 2);
    }

    #[test]
    fn test_is_retryable_error_timeout() {
        assert!(is_retryable_error("Request timeout"));
        assert!(is_retryable_error("Connection timeout"));
    }

    #[test]
    fn test_is_retryable_error_rate_limit() {
        assert!(is_retryable_error("Rate limited"));
        assert!(is_retryable_error("429 Too Many Requests"));
    }

    #[test]
    fn test_is_retryable_error_server_error() {
        assert!(is_retryable_error("503 Service Unavailable"));
        assert!(is_retryable_error("502 Bad Gateway"));
    }

    #[test]
    fn test_is_retryable_error_connection() {
        assert!(is_retryable_error("Connection refused"));
    }

    #[test]
    fn test_is_retryable_error_non_retryable() {
        assert!(!is_retryable_error("400 Bad Request"));
        assert!(!is_retryable_error("401 Unauthorized"));
        assert!(!is_retryable_error("Invalid API key"));
    }

    #[tokio::test]
    async fn test_retry_success_first_attempt() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        
        let result = retry(
            || async {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                Ok::<i32, &str>(42)
            },
            3,
            |_| false,
        ).await;
        
        assert_eq!(result, Ok(42));
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_success_after_failures() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        
        let result: Result<i32, &str> = retry(
            || async {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                let count = call_count_clone.load(Ordering::SeqCst);
                if count < 3 {
                    Err("retryable error")
                } else {
                    Ok(42)
                }
            },
            5,
            |e| *e == "retryable error",
        ).await;
        
        assert_eq!(result, Ok(42));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_all_failures() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        
        let result: Result<i32, &str> = retry(
            || async {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                Err("retryable error")
            },
            3,
            |e| *e == "retryable error",
        ).await;
        
        assert_eq!(result, Err("retryable error"));
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_non_retryable_error() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        
        let result: Result<i32, &str> = retry(
            || async {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                Err("non-retryable error")
            },
            3,
            |e| *e == "retryable error", // Only retry "retryable error"
        ).await;
        
        assert_eq!(result, Err("non-retryable error"));
        assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should not retry
    }

    #[tokio::test]
    async fn test_retry_with_config_custom_params() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        
        let config = RetryConfig {
            max_retries: 2,
            base_delay_ms: 10,
            max_delay_ms: 100,
            exponential_base: 2,
        };
        
        let result = retry_with_config(
            || async {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                if call_count_clone.load(Ordering::SeqCst) < 2 {
                    Err("retryable")
                } else {
                    Ok("success")
                }
            },
            &config,
            |e| *e == "retryable",
        ).await;
        
        assert_eq!(result, Ok("success"));
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_retry_config_clone() {
        let config = RetryConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.max_retries, config.max_retries);
        assert_eq!(cloned.base_delay_ms, config.base_delay_ms);
    }
}
