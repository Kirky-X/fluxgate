use nexusflow::vendor::resilience::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, IsRetryable, State,
};
use std::time::Duration;

#[derive(Debug, Clone)]
struct RetryableError;

impl IsRetryable for RetryableError {
    fn is_retryable(&self) -> bool {
        true
    }
}

impl std::fmt::Display for RetryableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retryable error")
    }
}

impl std::error::Error for RetryableError {}

#[derive(Debug, Clone)]
struct NonRetryableError;

impl IsRetryable for NonRetryableError {
    fn is_retryable(&self) -> bool {
        false
    }
}

impl std::fmt::Display for NonRetryableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "non-retryable error")
    }
}

impl std::error::Error for NonRetryableError {}

fn fast_config() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 3,
        open_timeout: Duration::from_millis(10),
        half_open_max_calls: 1,
    }
}

#[tokio::test]
async fn five_failures_trigger_open() {
    let cb = CircuitBreaker::new(fast_config());

    for i in 0..5 {
        let result: Result<(), _> = cb
            .call(|| async { Err::<(), RetryableError>(RetryableError) })
            .await;
        assert!(result.is_err(), "call {} should succeed (error wrapped)", i);
    }

    // 6th call should be rejected with Open
    let result = cb.call(|| async { Err::<(), RetryableError>(RetryableError) }).await;
    assert!(matches!(result, Err(CircuitBreakerError::Open)));
    assert!(matches!(cb.state(), State::Open { .. }));
}

#[tokio::test]
async fn open_state_rejects_immediately() {
    let cb = CircuitBreaker::new(fast_config());
    cb.force_open();

    let result: Result<(), CircuitBreakerError<RetryableError>> =
        cb.call(|| async { Ok::<(), RetryableError>(()) }).await;
    assert!(matches!(result, Err(CircuitBreakerError::Open)));
}

#[tokio::test]
async fn open_transitions_to_half_open_after_timeout() {
    let cb = CircuitBreaker::new(fast_config());
    cb.force_open();

    // Wait for timeout (10ms)
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Next call should transition to HalfOpen and execute
    let result: Result<(), _> = cb.call(|| async { Ok::<(), RetryableError>(()) }).await;
    assert!(result.is_ok());
    assert!(matches!(cb.state(), State::HalfOpen { .. }));
}

#[tokio::test]
async fn half_open_three_successes_close() {
    let cb = CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: 5,
        success_threshold: 3,
        open_timeout: Duration::from_millis(10),
        half_open_max_calls: 10,
    });
    cb.force_half_open();

    // Need 3 successful calls to close
    for _ in 0..3 {
        let result: Result<(), _> = cb.call(|| async { Ok::<(), RetryableError>(()) }).await;
        assert!(result.is_ok());
    }

    assert!(matches!(cb.state(), State::Closed));
}

#[tokio::test]
async fn half_open_failure_reopens() {
    let cb = CircuitBreaker::new(fast_config());
    cb.force_half_open();

    let result: Result<(), _> = cb
        .call(|| async { Err::<(), RetryableError>(RetryableError) })
        .await;
    assert!(result.is_err());

    assert!(matches!(cb.state(), State::Open { .. }));
}

#[tokio::test]
async fn non_retryable_error_does_not_count() {
    let cb = CircuitBreaker::new(fast_config());

    for _ in 0..10 {
        let _: Result<(), _> = cb
            .call(|| async { Err::<(), NonRetryableError>(NonRetryableError) })
            .await;
    }

    assert!(matches!(cb.state(), State::Closed));
}

#[tokio::test]
async fn success_resets_failure_count() {
    let cb = CircuitBreaker::new(fast_config());

    // 4 failures (below threshold)
    for _ in 0..4 {
        let _: Result<(), _> = cb
            .call(|| async { Err::<(), RetryableError>(RetryableError) })
            .await;
    }

    // Success resets counter
    let _: Result<(), _> = cb.call(|| async { Ok::<(), RetryableError>(()) }).await;

    // 4 more failures should not trigger Open (counter was reset)
    for _ in 0..4 {
        let result: Result<(), _> = cb
            .call(|| async { Err::<(), RetryableError>(RetryableError) })
            .await;
        assert!(matches!(result, Err(CircuitBreakerError::Inner(_))));
    }

    assert!(matches!(cb.state(), State::Closed));
}
