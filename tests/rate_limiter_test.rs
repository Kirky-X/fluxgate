#[cfg(test)]
mod tests {
    use smart_llm_proxy::vendor::TokenBucket;

    #[tokio::test]
    async fn test_token_bucket_new() {
        let bucket = TokenBucket::new(10, 5);
        assert_eq!(bucket.remaining(), 10);
    }

    #[tokio::test]
    async fn test_token_bucket_try_acquire() {
        let bucket = TokenBucket::new(2, 10);
        
        // First two acquires should succeed
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        
        // Third should fail
        assert!(!bucket.try_acquire());
    }

    #[tokio::test]
    async fn test_token_bucket_remaining() {
        let bucket = TokenBucket::new(5, 10);
        
        assert_eq!(bucket.remaining(), 5);
        
        bucket.try_acquire();
        assert_eq!(bucket.remaining(), 4);
        
        bucket.try_acquire();
        assert_eq!(bucket.remaining(), 3);
    }

    #[tokio::test]
    async fn test_token_bucket_reset() {
        let bucket = TokenBucket::new(5, 10);
        
        // Acquire all tokens
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        // Now should be empty
        assert!(!bucket.try_acquire());
        
        bucket.reset();
        assert_eq!(bucket.remaining(), 5);
    }

    #[tokio::test]
    async fn test_token_bucket_zero_capacity() {
        let bucket = TokenBucket::new(0, 10);
        assert!(!bucket.try_acquire());
    }

    #[tokio::test]
    async fn test_token_bucket_single_request() {
        let bucket = TokenBucket::new(1, 1);
        
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());
    }
}
