// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

mod status {
    use nexusflow::vendor::{VendorState, VendorStatus};

    #[test]
    fn test_vendor_state_new() {
        let state = VendorState::new();
        assert_eq!(state.get_status(), VendorStatus::Available);
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_set_available() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Available);
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_set_busy() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Busy);
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_set_throttled() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Throttled);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_set_unavailable() {
        let state = VendorState::new();
        state.set_status(VendorStatus::Unavailable);
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_inc_error() {
        let state = VendorState::new();
        assert_eq!(state.get_error_count(), 0);

        state.inc_error();
        assert_eq!(state.get_error_count(), 1);

        state.inc_error();
        assert_eq!(state.get_error_count(), 2);
    }

    #[test]
    fn test_vendor_state_reset_error() {
        let state = VendorState::new();

        state.inc_error();
        state.inc_error();
        assert_eq!(state.get_error_count(), 2);

        state.reset_error();
        assert_eq!(state.get_error_count(), 0);
    }

    #[test]
    fn test_vendor_state_mark_busy() {
        let state = VendorState::new();
        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);
    }

    #[test]
    fn test_vendor_state_mark_available() {
        let state = VendorState::new();
        state.mark_busy();
        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);
    }

    #[test]
    fn test_vendor_state_mark_throttled() {
        let state = VendorState::new();
        let backoff = std::time::Duration::from_secs(60);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);
    }

    #[test]
    fn test_vendor_state_mark_unavailable() {
        let state = VendorState::new();
        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_state_status_transitions() {
        let state = VendorState::new();

        state.mark_busy();
        assert_eq!(state.get_status(), VendorStatus::Busy);

        let backoff = std::time::Duration::from_secs(30);
        state.mark_throttled(backoff);
        assert_eq!(state.get_status(), VendorStatus::Throttled);

        state.mark_available();
        assert_eq!(state.get_status(), VendorStatus::Available);

        state.mark_unavailable();
        assert_eq!(state.get_status(), VendorStatus::Unavailable);
    }

    #[test]
    fn test_vendor_status_from_u8() {
        assert_eq!(VendorStatus::from_u8(0), VendorStatus::Available);
        assert_eq!(VendorStatus::from_u8(1), VendorStatus::Busy);
        assert_eq!(VendorStatus::from_u8(2), VendorStatus::Throttled);
        assert_eq!(VendorStatus::from_u8(3), VendorStatus::Unavailable);
        assert_eq!(VendorStatus::from_u8(99), VendorStatus::Available);
    }
}

mod rate_limiter {
    use nexusflow::vendor::{ConcurrencyLimiter, TokenBucket};
    use std::time::Duration;

    #[tokio::test]
    async fn test_token_bucket_acquire_succeeds() {
        let bucket = TokenBucket::new(5, 10);
        // Should succeed immediately — bucket starts full
        let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_token_bucket_acquires_until_exhausted() {
        let bucket = TokenBucket::new(3, 1);
        // Consume all 3 tokens
        for _ in 0..3 {
            let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
            assert!(result.is_ok(), "acquire should succeed within timeout");
        }
        // 4th acquire should block (no tokens left, refill rate is 1/sec)
        let result = tokio::time::timeout(Duration::from_millis(50), bucket.acquire()).await;
        assert!(result.is_err(), "acquire should timeout when bucket is empty");
    }

    #[tokio::test]
    async fn test_token_bucket_refills_over_time() {
        let bucket = TokenBucket::new(1, 100); // 100 tokens/sec refill
        // Consume the only token
        bucket.acquire().await;
        // Wait for refill
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Should succeed now
        let result = tokio::time::timeout(Duration::from_millis(100), bucket.acquire()).await;
        assert!(result.is_ok(), "acquire should succeed after refill");
    }

    #[tokio::test]
    async fn test_concurrency_limiter_acquire() {
        let limiter = ConcurrencyLimiter::new(2);
        let _p1 = limiter.acquire().await;
        let _p2 = limiter.acquire().await;
        // 3rd acquire should block
        let result = tokio::time::timeout(Duration::from_millis(50), limiter.acquire()).await;
        assert!(result.is_err(), "3rd acquire should block");
    }

    #[tokio::test]
    async fn test_concurrency_limiter_release_on_drop() {
        let limiter = ConcurrencyLimiter::new(1);
        {
            let _permit = limiter.acquire().await;
        }
        // After permit dropped, should be able to acquire again
        let result = tokio::time::timeout(Duration::from_millis(100), limiter.acquire()).await;
        assert!(result.is_ok(), "acquire should succeed after permit dropped");
    }
}
