// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::sync::atomic::{AtomicUsize, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use crate::config::RateLimitConfig;
use chrono::{Local, NaiveTime};

pub struct TokenBucket {
    capacity: usize,
    tokens: AtomicUsize,
    #[allow(dead_code)]
    refill_rate: usize,
    last_refill: AtomicU64,
}

impl TokenBucket {
    pub fn new(capacity: usize, requests_per_second: u32) -> Self {
        Self {
            capacity,
            tokens: AtomicUsize::new(capacity),
            refill_rate: requests_per_second as usize * 60,
            last_refill: AtomicU64::new(0),
        }
    }
    
    pub fn try_acquire(&self) -> bool {
        self.refill();
        let current = self.tokens.load(Ordering::Relaxed);
        if current > 0 {
            self.tokens.fetch_sub(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
    
    #[allow(dead_code)]
    pub async fn acquire(&self) {
        while !self.try_acquire() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    
    fn refill(&self) {
        let now = SystemTime::now();
        let last = self.last_refill.load(Ordering::Relaxed);
        
        if last == 0 {
            self.last_refill.store(
                now.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                Ordering::Relaxed,
            );
            return;
        }
        
        let elapsed = now
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - last;
        
        if elapsed == 0 {
            return;
        }
        
        let refill = (elapsed as usize * self.refill_rate) / 60;
        if refill > 0 {
            let current = self.tokens.load(Ordering::Relaxed);
            let new_tokens = (current + refill).min(self.capacity);
            self.tokens.store(new_tokens, Ordering::Relaxed);
            self.last_refill.store(
                now.duration_since(UNIX_EPOCH).unwrap().as_secs(),
                Ordering::Relaxed,
            );
        }
    }
    
    #[allow(dead_code)]
    pub fn remaining(&self) -> usize {
        self.refill();
        self.tokens.load(Ordering::Relaxed)
    }
    
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.tokens.store(self.capacity, Ordering::Relaxed);
        self.last_refill.store(0, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct ConcurrencyLimiter {
    #[allow(dead_code)]
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    _max_concurrency: usize,
}

impl ConcurrencyLimiter {
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency)),
            _max_concurrency: max_concurrency,
        }
    }
    
    #[allow(dead_code)]
    pub async fn acquire(&self) -> ConcurrencyPermit {
        let permit = self.semaphore.clone().acquire_owned().await
            .expect("Semaphore closed");
        ConcurrencyPermit { _permit: permit }
    }
    
    #[allow(dead_code)]
    pub fn try_acquire(&self) -> Option<ConcurrencyPermit> {
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(ConcurrencyPermit { _permit: permit }),
            Err(_) => None,
        }
    }
    
    #[allow(dead_code)]
    pub fn available(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[allow(dead_code)]
pub struct ConcurrencyPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

// 高级限流器实现
pub enum AdvancedRateLimiter {
    TimeWindow {
        start: NaiveTime,
        end: NaiveTime,
    },
    Interval {
        interval_secs: u64,
        last_reset: AtomicU64,
    },
    Token {
        max_tokens: u64,
        used_tokens: AtomicU64,
        last_reset: AtomicU64, // Used for daily/interval reset if needed, but for now just global accumulation
    },
    None,
}

impl AdvancedRateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        match config {
            RateLimitConfig::TimeWindow { start, end } => {
                let start_time = NaiveTime::parse_from_str(start, "%H:%M").unwrap_or_else(|_| NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                let end_time = NaiveTime::parse_from_str(end, "%H:%M").unwrap_or_else(|_| NaiveTime::from_hms_opt(23, 59, 59).unwrap());
                Self::TimeWindow { start: start_time, end: end_time }
            },
            RateLimitConfig::Interval { hours } => {
                Self::Interval {
                    interval_secs: hours * 3600,
                    last_reset: AtomicU64::new(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()),
                }
            },
            RateLimitConfig::Token { max_tokens } => {
                Self::Token {
                    max_tokens: *max_tokens,
                    used_tokens: AtomicU64::new(0),
                    last_reset: AtomicU64::new(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()),
                }
            },
        }
    }

    pub fn check(&self) -> bool {
        match self {
            Self::TimeWindow { start, end } => {
                let now = Local::now().time();
                now >= *start && now < *end
            },
            Self::Interval { interval_secs, last_reset } => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                let last = last_reset.load(Ordering::Relaxed);
                if now - last >= *interval_secs {
                    // 应该重置，但这通常由后台任务做，或者懒惰重置。
                    // 这里我们假设如果超过间隔，就允许请求（并更新重置时间）
                    // 但对于 Interval 类型，仅仅是"间隔"本身没有意义，通常意味着"间隔内限制请求数"。
                    // 但 PRD 中 config 只有 hours。
                    // 假设 Interval 模式下，总是 Available，除非配合其他限制？
                    // 或者 PRD 意思是 "每隔 X 小时重置状态"。
                    // 暂时认为 Interval 模式总是通过检查，具体的重置逻辑由 reset worker 处理。
                    true
                } else {
                    true
                }
            },
            Self::Token { max_tokens, used_tokens, .. } => {
                used_tokens.load(Ordering::Relaxed) < *max_tokens
            },
            Self::None => true,
        }
    }

    pub fn consume_tokens(&self, tokens: u64) {
        if let Self::Token { used_tokens, .. } = self {
            used_tokens.fetch_add(tokens, Ordering::Relaxed);
        }
    }
    
    pub fn reset(&self) {
        match self {
            Self::Interval { last_reset, .. } => {
                last_reset.store(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(), Ordering::Relaxed);
            },
            Self::Token { used_tokens, last_reset, .. } => {
                used_tokens.store(0, Ordering::Relaxed);
                last_reset.store(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(), Ordering::Relaxed);
            },
            _ => {}
        }
    }
    
    // Check if should reset (for Interval type mostly)
    pub fn should_reset(&self) -> bool {
        match self {
             Self::Interval { interval_secs, last_reset } => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                let last = last_reset.load(Ordering::Relaxed);
                now - last >= *interval_secs
            },
            _ => false
        }
    }
}
