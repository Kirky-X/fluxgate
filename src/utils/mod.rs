pub mod retry;

#[allow(dead_code)]
pub async fn with_retry<T, F, Fut>(mut f: F, max_retries: u32) -> Result<T, anyhow::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
{
    let mut last_error = None;
    
    for attempt in 0..max_retries {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);
                if attempt < max_retries - 1 {
                    let delay = std::time::Duration::from_secs(2_u64.pow(attempt as u32));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Unknown error")))
}

/// 重试配置
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub exponential_base: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30000,
            exponential_base: 2,
        }
    }
}

/// 检查错误是否可重试
#[allow(dead_code)]
pub fn is_retryable_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("timeout") 
        || lower.contains("rate limited")
        || lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("connection refused")
}

/// 带配置的重试函数
#[allow(dead_code)]
pub async fn retry_with_config<T, E, F, Fut>(
    mut f: F,
    config: &RetryConfig,
    error_extractor: impl Fn(&E) -> bool,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut last_error: Option<E> = None;
    
    for attempt in 0..config.max_retries {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if !error_extractor(&e) {
                    return Err(e);
                }
                last_error = Some(e);
                
                if attempt < config.max_retries - 1 {
                    let delay = std::cmp::min(
                        config.base_delay_ms * config.exponential_base.pow(attempt),
                        config.max_delay_ms
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }
    }
    
    Err(last_error.unwrap())
}

/// 简化的重试函数
#[allow(dead_code)]
pub async fn retry<T, E, F, Fut>(
    f: F,
    max_retries: u32,
    error_extractor: impl Fn(&E) -> bool,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let config = RetryConfig {
        max_retries,
        ..Default::default()
    };
    retry_with_config(f, &config, error_extractor).await
}
