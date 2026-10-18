// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK 重试策略：指数退避（含抖动）、`Retry-After` 解析。
//!
//! 默认参数对齐 OpenAI 官方 SDK：最多 2 次重试、初始 0.5s、退避上限 8s、
//! `Retry-After` 提示上限 120s。

use std::time::Duration;

use reqwest::header::HeaderMap;

use crate::sdk::error::header_str;

/// 重试策略。
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial: Duration,
    pub max: Duration,
    /// 上游 `Retry-After` 提示的截断上限（防恶意超长值拖死客户端）。
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial: Duration::from_millis(500),
            max: Duration::from_secs(8),
            max_retry_after: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// 本轮退避：`retry_after` 提示优先（截断到 [`Self::max_retry_after`]），
    /// 缺省走指数退避 + 抖动。`jitter_seed` 由调用方注入（生产取系统纳秒，
    /// 测试固定值保证确定性）。
    pub(crate) fn delay_for(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        jitter_seed: u64,
    ) -> Duration {
        match retry_after {
            Some(hint) => hint.min(self.max_retry_after),
            None => backoff(self, attempt, jitter_seed),
        }
    }
}

/// 解析 `Retry-After` 头：`retry-after-ms`（毫秒）优先；`retry-after`
/// 支持秒数（RFC 7231 delta-seconds）与 HTTP-date（IMF-fixdate）。解析失败
/// 或时间已过的日期 → `None`。
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    if let Some(millis) = header_str(headers, "retry-after-ms").and_then(|v| v.parse::<u64>().ok())
    {
        return Some(Duration::from_millis(millis));
    }
    let raw = header_str(headers, "retry-after")?;
    if let Ok(seconds) = raw.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    // HTTP-date（IMF-fixdate 与 RFC 2822 同构："Sun, 06 Nov 1994 08:49:37 GMT"）
    let date = chrono::DateTime::parse_from_rfc2822(raw.trim()).ok()?;
    let remaining = date.with_timezone(&chrono::Utc) - chrono::Utc::now();
    remaining.to_std().ok()
}

/// 指数退避 + 抖动：`min(initial * 2^attempt, max) × factor`，其中
/// `factor = 1 - 0.25 × seed_fraction ∈ [0.75, 1.0]`——对齐官方
/// `1 - 0.25 * random()`（抖动只减速不加速，避免重试风暴叠加）。
pub(crate) fn backoff(policy: &RetryPolicy, attempt: u32, jitter_seed: u64) -> Duration {
    let multiplier = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
    let capped = policy.initial.saturating_mul(multiplier).min(policy.max);
    let seed_fraction = (jitter_seed % 10_000) as f64 / 10_000.0;
    capped.mul_f64(1.0 - 0.25 * seed_fraction)
}

/// 生产注入：系统时钟纳秒作抖动种子（仅作熵源，无需加密质量）。
pub(crate) fn system_jitter_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_exponential_sequence_with_full_factor() {
        let policy = RetryPolicy::default();
        // seed % 10000 == 0 → factor = 1.0，序列精确可断言
        assert_eq!(backoff(&policy, 0, 0), Duration::from_millis(500));
        assert_eq!(backoff(&policy, 1, 0), Duration::from_secs(1));
        assert_eq!(backoff(&policy, 2, 0), Duration::from_secs(2));
        assert_eq!(backoff(&policy, 3, 0), Duration::from_secs(4));
        // 8s 上限
        assert_eq!(backoff(&policy, 4, 0), Duration::from_secs(8));
        assert_eq!(backoff(&policy, 31, 0), Duration::from_secs(8));
        assert_eq!(backoff(&policy, u32::MAX, 0), Duration::from_secs(8));
    }

    #[test]
    fn backoff_jitter_is_bounded_and_deterministic() {
        let policy = RetryPolicy::default();
        // 固定 seed 恒定输出（确定性）
        assert_eq!(backoff(&policy, 1, 5000), backoff(&policy, 1, 5000));
        // seed_fraction = 0.5 → factor 0.875：1s × 0.875 = 875ms
        assert_eq!(backoff(&policy, 1, 5000), Duration::from_millis(875));

        for seed in [0, 1, 999, 2500, 9999, 12_345, u64::MAX] {
            let base = Duration::from_secs(2);
            let jittered = backoff(&policy, 2, seed);
            assert!(
                jittered >= base.mul_f64(0.75) && jittered <= base,
                "seed {seed} 越界: {jittered:?}"
            );
        }
    }

    #[test]
    fn retry_after_hint_wins_and_is_truncated() {
        let policy = RetryPolicy::default();
        // 提示优先于退避
        assert_eq!(
            policy.delay_for(0, Some(Duration::from_secs(3)), 0),
            Duration::from_secs(3)
        );
        // 截断到 max_retry_after（120s）
        assert_eq!(
            policy.delay_for(0, Some(Duration::from_secs(9999)), 0),
            Duration::from_secs(120)
        );
        // 无提示 → 退避
        assert_eq!(policy.delay_for(0, None, 0), Duration::from_millis(500));
    }

    #[test]
    fn parse_retry_after_seconds_and_millis_priority() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(7)));

        headers.insert("retry-after-ms", "2500".parse().unwrap());
        assert_eq!(
            parse_retry_after(&headers),
            Some(Duration::from_millis(2500)),
            "retry-after-ms 优先"
        );

        let empty = HeaderMap::new();
        assert_eq!(parse_retry_after(&empty), None);

        let mut invalid = HeaderMap::new();
        invalid.insert("retry-after", "soon".parse().unwrap());
        assert_eq!(parse_retry_after(&invalid), None);
    }
}
