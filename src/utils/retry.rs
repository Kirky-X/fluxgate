// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 智能重试机制模块
//!
//! 根据 PRD 设计，实现按状态码分类的重试策略：
//! - 400, 401, 403, 404: 标记厂商 Unavailable，不重试
//! - 429: 指数回退重试（1s, 2s, 4s, 8s, 16s）
//! - 500, 502, 503: 重试 3 次，间隔 1s
//! - 网络超时: 重试 2 次
//!
//! # Feature 门控
//!
//! `with_retry` 为 v0.1 手写实现（按 `ErrorCategory` 分类，签名
//! `with_retry(f, initial_error)`）。重试延迟计算已迁移
//! `limiteron::retry::delay_for_attempt`（recId=7，见 [`retry_delay_for`]）。

use crate::error::NexusFlowError;
// 仅 vendor-management 分支使用（spread_delay/retry_delay_for 同在该 feature 下）
#[cfg(feature = "vendor-management")]
use rand::Rng;
use std::time::Duration;

/// 错误分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// 不可重试 - 标记厂商不可用
    Fatal,
    /// 可重试 - 指数回退（429）
    RetryableExponential,
    /// 可重试 - 固定间隔（500/502/503）
    RetryableFixed,
    /// 可重试 - 简短重试（超时）
    RetryableShort,
}

impl ErrorCategory {
    /// 根据 HTTP 状态码判断错误分类
    pub fn from_status_code(status: u16) -> Self {
        match status {
            // 客户端错误 - 不重试
            400 | 401 | 403 | 404 => Self::Fatal,
            // 限流 - 指数回退
            429 => Self::RetryableExponential,
            // 服务器错误 - 固定间隔重试
            500 | 502 | 503 => Self::RetryableFixed,
            // 其他错误
            _ => Self::Fatal,
        }
    }

    /// 判断是否为超时错误
    pub fn from_error_message(msg: &str) -> Self {
        let lower = msg.to_lowercase();
        if lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("connection refused")
            || lower.contains("connection reset")
        {
            Self::RetryableShort
        } else {
            Self::Fatal
        }
    }

    /// 是否可重试
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::Fatal)
    }

    /// 最大重试次数
    pub fn max_retries(&self) -> u32 {
        match self {
            Self::Fatal => 0,
            Self::RetryableExponential => 5,
            Self::RetryableFixed => 3,
            Self::RetryableShort => 2,
        }
    }
}

/// 重试策略
#[derive(Debug, Clone)]
pub struct RetryStrategy {
    pub max_retries: u32,
    pub backoff: BackoffStrategy,
}

impl RetryStrategy {
    /// 根据错误分类创建重试策略
    pub fn for_category(category: ErrorCategory) -> Self {
        match category {
            ErrorCategory::Fatal => Self {
                max_retries: 0,
                backoff: BackoffStrategy::None,
            },
            ErrorCategory::RetryableExponential => Self {
                max_retries: 5,
                backoff: BackoffStrategy::Exponential {
                    base_ms: 1000,
                    max_ms: 16000,
                },
            },
            ErrorCategory::RetryableFixed => Self {
                max_retries: 3,
                backoff: BackoffStrategy::Fixed { interval_ms: 1000 },
            },
            ErrorCategory::RetryableShort => Self {
                max_retries: 2,
                backoff: BackoffStrategy::Fixed { interval_ms: 500 },
            },
        }
    }
}

/// 回退策略
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// 无回退
    None,
    /// 指数回退
    Exponential { base_ms: u64, max_ms: u64 },
    /// 固定间隔
    Fixed { interval_ms: u64 },
}

impl BackoffStrategy {
    /// 计算指定尝试次数的延迟
    pub fn delay(&self, attempt: u32) -> Duration {
        match self {
            Self::None => Duration::from_millis(0),
            Self::Exponential { base_ms, max_ms } => {
                let exp_delay = base_ms * 2u64.pow(attempt.min(4));
                Duration::from_millis(exp_delay.min(*max_ms))
            }
            Self::Fixed { interval_ms } => Duration::from_millis(*interval_ms),
        }
    }
}

/// 提取 HTTP 状态码
pub fn extract_status_code(error: &str) -> u16 {
    // 尝试从错误消息中提取状态码
    // 例如: "VendorUnavailable: anthropic_main (429: Rate limit exceeded)"
    if let Some(start) = error.find('(') {
        let rest = &error[start + 1..];
        if let Some(end) = rest.find(':') {
            if let Ok(code) = rest[..end].trim().parse::<u16>() {
                return code;
            }
        }
        // 尝试直接解析括号内的数字
        if let Ok(code) = rest.split_whitespace().next().unwrap_or("").parse::<u16>() {
            return code;
        }
    }
    500 // 默认返回服务器错误
}

/// 判断错误是否可重试
pub fn is_retryable_error(error: &NexusFlowError) -> bool {
    match error {
        NexusFlowError::VendorUnavailable { vendor, .. } => {
            let code = extract_status_code(vendor);
            let category = ErrorCategory::from_status_code(code);
            category.is_retryable()
        }
        NexusFlowError::Timeout { .. } => true,
        _ => false,
    }
}

/// 重试延迟策略参数（recId=7：来自 Config [retry] 段的热读快照）
#[derive(Debug, Clone, PartialEq)]
pub struct RetryDelayPolicy {
    /// 首次重试前延迟
    pub initial_delay: Duration,
    /// 退避乘数
    pub factor: f64,
    /// 单次等待上限
    pub max_delay: Duration,
    /// 退避抖动比例（0.0–1.0）
    pub jitter: f64,
    /// 同步惊群展宽比例（recId=5）：`Retry-After` 与 5xx 固定间隔分支的
    /// 延迟 = base + U(0, spread_jitter) × base（base 为 Retry-After 值
    /// 或 1s）。1.0 = 全范围展宽 [base, 2×base)；0.0 = 完全恢复旧行为
    /// （Retry-After 原样返回、5xx 固定 1s）——回滚阀。
    pub spread_jitter: f64,
}

impl Default for RetryDelayPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            factor: 2.0,
            max_delay: Duration::from_secs(16),
            jitter: 0.2,
            spread_jitter: 1.0,
        }
    }
}

impl RetryDelayPolicy {
    /// 从热配置的 [retry] 段构建
    pub fn from_config(retry: &crate::config::RetrySection) -> Self {
        Self {
            initial_delay: Duration::from_millis(retry.initial_delay_ms),
            factor: retry.factor,
            max_delay: Duration::from_millis(retry.max_delay_ms),
            jitter: retry.jitter,
            spread_jitter: retry.spread_jitter,
        }
    }
}

/// 纯延迟选择函数（recId=7③）：`Retry-After` 优先覆盖，否则按分类取退避。
///
/// - 429 带 `Retry-After` 头 → 以上游指示为 base 全范围展宽
///   `[ra, ra + spread_jitter × ra)`（优先级最高；spread_jitter=0 时
///   原样采用）
/// - `RetryableExponential`（429 无头）→ `limiteron::retry::delay_for_attempt`
///   （指数 + jitter + max_delay 封顶；jitter=0 时确定性可单测）
/// - `RetryableFixed`（5xx）→ `[1s, 1s + spread_jitter × 1s)`
///   （spread_jitter=0 时固定 1s）
/// - `RetryableShort`（网络错误）→ 500ms × attempt
/// - `Fatal` → 0（调用方不应重试）
///
/// recId=5：`Retry-After` 与 5xx 两个分支补全范围抖动——修复前延迟为
/// 常量（恒 `ra` / 恒 1s），上游同一 `Retry-After` 下所有并发在同一刻
/// 唤醒重试（同步惊群）。RNG 经 `&mut dyn rand::RngCore` 注入（对齐
/// `router::weighted_select_with` 的注入模式）：spread_jitter=0 时不消耗
/// RNG，确定性可单测；生产传 `rand::thread_rng()`。
///
/// 纯函数无 IO，可脱离 HTTP 栈单测（规避 tests/vendor_circuit_breaker_test.rs
/// 书面记录的 paused-clock 与 mockito IO 冲突陷阱）。
///
/// 注（recId=7③ 如实注明）：`RetryPolicy::execute` 无 per-attempt 延迟
/// 注入点（limiteron src/retry.rs:147 签名固定），429 的 Retry-After
/// 分支因此落在自管薄循环中调用本函数，而非 RetryPolicy。
///
/// Feature 门控：指数分支依赖 `limiteron::retry`（vendor-management 引入），
/// 唯一调用方 `vendor::manager` 同在该 feature 下。
#[cfg(feature = "vendor-management")]
pub fn retry_delay_for(
    category: ErrorCategory,
    attempt: u32,
    retry_after: Option<Duration>,
    policy: &RetryDelayPolicy,
    rng: &mut dyn rand::RngCore,
) -> Duration {
    if let Some(ra) = retry_after {
        return spread_delay(ra, policy, rng);
    }
    match category {
        ErrorCategory::RetryableExponential => limiteron::retry::delay_for_attempt(
            attempt.max(1),
            policy.initial_delay,
            policy.factor,
            policy.max_delay,
            policy.jitter,
        ),
        ErrorCategory::RetryableFixed => spread_delay(Duration::from_secs(1), policy, rng),
        ErrorCategory::RetryableShort => {
            Duration::from_millis(500u64.saturating_mul(attempt.max(1) as u64))
        }
        ErrorCategory::Fatal => Duration::ZERO,
    }
}

/// recId=5：`base + U(0, spread_jitter) × base` 的全范围展宽。
///
/// spread_jitter <= 0 恒等 base（回滚阀：旧行为确定性可单测）；半开区间
/// 上界由 `gen_range(0.0..spread)` 保证（永不取 spread 本身）。
/// 全范围抖动（recId=5）：`base + U(0, spread_jitter)×base`，消除
/// Retry-After / 固定间隔分支的同步惊群。唯一调用方 retry_delay_for
/// 在 vendor-management 下（其唯一调用方 vendor::manager 同在该 feature）。
#[cfg(feature = "vendor-management")]
fn spread_delay(
    base: Duration,
    policy: &RetryDelayPolicy,
    rng: &mut dyn rand::RngCore,
) -> Duration {
    if policy.spread_jitter <= 0.0 {
        return base;
    }
    let extra = base.mul_f64(rng.gen_range(0.0..policy.spread_jitter));
    base.saturating_add(extra)
}

/// recId=9（W5）：重试放大系数 `amplification = 1/(1 − r·f)` —
/// `λ_eff = λ/(1 − r·f)` 的放大因子形态（r=重试请求数/失败请求数，
/// f=失败率）。`r·f ≥ 1`（分母 ≤ 0）→ None：队列发散（亚稳态），无
/// 有限放大系数可言。纯函数，脱离 HTTP 栈单测。
pub fn amplification(r: f64, f: f64) -> Option<f64> {
    let denom = 1.0 - r * f;
    if !denom.is_finite() || denom <= f64::EPSILON {
        return None;
    }
    Some(1.0 / denom)
}

/// recId=9（W5）：临界失败率 `f_c = (C − λ)/(r·C)` — 失败率超过 f_c 时
/// 重试放大把系统推入亚稳态（C=全网并发上限，λ=入站速率）。r ≤ 0
///（无重试放大）或 C ≤ 0 → None（无从收紧）；C < λ（容量饱和）→ 0.0
///（任何失败率都超临界）。纯函数。
pub fn critical_failure_fraction(r: f64, c: f64, lambda: f64) -> Option<f64> {
    if r <= 0.0 || c <= 0.0 {
        return None;
    }
    Some(((c - lambda) / (r * c)).max(0.0))
}

/// recId=9（W5）：重试时间预算判定 — 「锚点已耗时 + 下次等待」是否超出
/// 预算。锚点必须用 `tokio::time::Instant`（std Instant 不随 paused clock
/// 推进，`#[tokio::test(start_paused = true)]` 的自动推进依赖 tokio 时钟）。
/// 纯函数无 IO，规避 tests/vendor_circuit_breaker_test.rs 书面记录的
/// paused-clock 与 mockito IO 冲突陷阱（retry_delay_for 同款理由）。
pub fn budget_exceeded(
    anchor: tokio::time::Instant,
    budget: Duration,
    next_delay: Duration,
) -> bool {
    anchor.elapsed() + next_delay > budget
}

/// recId=9（W5）：重试放大守门器判定（纯函数）— 返回 `(收紧到 1, 发散告警)`。
///
/// - 收紧：`f > 0.8·f_c`（失败率闯入临界失败率的亚稳态带）。f_c 无解
///   （r≤0 无重试放大 / C≤0 无容量估计）→ 不收紧（fail-open，只可能
///   告警）；C<λ 容量饱和 → f_c=0.0 → 任何失败率都收紧（容量已打满，
///   退化单尝试方向 fail-safe）。
/// - 发散：`r·f > 0.95`（λ_eff=λ/(1−r·f) 无界预警，只 warn）。
///
/// 数学事实：发散阈值 0.95/r 恒大于 0.8·f_c（因 0.8(1−λ/C) ≤ 0.8 < 0.95），
/// f_c 有解时发散必然已处收紧带——发散告警的意义在 f_c 无解（C=0）域。
pub fn amplification_guard(r: f64, f: f64, c: f64, lambda: f64) -> (bool, bool) {
    let diverge = r * f > 0.95;
    let tighten = match critical_failure_fraction(r, c, lambda) {
        Some(f_c) => f > 0.8 * f_c,
        None => false,
    };
    (tighten, diverge)
}

/// 执行带重试的请求
///
/// v0.1 手写实现 — 按 `ErrorCategory` 分类决定是否重试与退避。
#[allow(clippy::too_many_arguments)]
pub async fn with_retry<F, Fut, T>(
    mut f: F,
    initial_error: Option<NexusFlowError>,
) -> Result<T, NexusFlowError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, NexusFlowError>>,
{
    // 确定初始错误类别
    let initial_category = if let Some(ref err) = initial_error {
        match err {
            NexusFlowError::Timeout { .. } => ErrorCategory::RetryableShort,
            NexusFlowError::VendorUnavailable { vendor, .. } => {
                let code = extract_status_code(vendor);
                ErrorCategory::from_status_code(code)
            }
            _ => ErrorCategory::Fatal,
        }
    } else {
        return (f)().await;
    };

    let strategy = RetryStrategy::for_category(initial_category);

    if strategy.max_retries == 0 {
        return Err(initial_error.unwrap_or(NexusFlowError::Protocol {
            source: "Unknown error".into(),
            inner: None,
        }));
    }

    let mut last_error = initial_error;

    for attempt in 0..strategy.max_retries {
        // 执行请求
        match (f)().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                // 检查错误是否仍然可重试
                if !is_retryable_error(&e) {
                    return Err(e);
                }
                last_error = Some(e);

                // 计算延迟
                let delay = strategy.backoff.delay(attempt);
                if delay > Duration::from_secs(0) {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or(NexusFlowError::Protocol {
        source: "Max retries exceeded".into(),
        inner: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_category_from_status() {
        assert_eq!(ErrorCategory::from_status_code(400), ErrorCategory::Fatal);
        assert_eq!(ErrorCategory::from_status_code(401), ErrorCategory::Fatal);
        assert_eq!(
            ErrorCategory::from_status_code(429),
            ErrorCategory::RetryableExponential
        );
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
    fn test_backoff_delay() {
        let exponential = BackoffStrategy::Exponential {
            base_ms: 1000,
            max_ms: 16000,
        };

        assert_eq!(exponential.delay(0), Duration::from_millis(1000));
        assert_eq!(exponential.delay(1), Duration::from_millis(2000));
        assert_eq!(exponential.delay(2), Duration::from_millis(4000));
        assert_eq!(exponential.delay(3), Duration::from_millis(8000));
        assert_eq!(exponential.delay(4), Duration::from_millis(16000));
        assert_eq!(exponential.delay(5), Duration::from_millis(16000)); // capped

        let fixed = BackoffStrategy::Fixed { interval_ms: 1000 };
        assert_eq!(fixed.delay(0), Duration::from_millis(1000));
        assert_eq!(fixed.delay(10), Duration::from_millis(1000));
    }

    #[test]
    fn test_extract_status_code() {
        assert_eq!(extract_status_code("anthropic_1 (429: Rate limit)"), 429);
        assert_eq!(extract_status_code("vendor (500: Internal error)"), 500);
        assert_eq!(extract_status_code("no status code here"), 500); // default
    }

    // ============ recId=9（W5）tests：重试放大守门器纯函数 ============

    /// amplification(0.3, 0.4) = 1/(1−0.12) ≈ 1.136（评审给定的数值对照）。
    #[test]
    fn amplification_matches_reference_value() {
        let a = amplification(0.3, 0.4).expect("rf<1 finite");
        assert!((a - 1.0 / 0.88).abs() < 1e-9, "1/(1−0.12)={a}");
        assert!((a - 1.136).abs() < 1e-3);
    }

    /// r·f → 1 发散：恰好 1 与超过 1 都必须返回 None（无有限放大系数）。
    #[test]
    fn amplification_diverges_when_rf_reaches_one() {
        assert!(amplification(2.0, 0.5).is_none(), "r·f == 1 发散");
        assert!(amplification(3.0, 0.5).is_none(), "r·f > 1 发散");
        assert!(amplification(0.0, 0.9).is_some(), "r=0 → 放大恒 1");
        assert!((amplification(0.0, 0.9).unwrap() - 1.0).abs() < 1e-12);
    }

    /// f_c 反解数值断言：C=100、λ=20、r=2 → f_c = (100−20)/(2×100) = 0.4；
    /// 边界：r=0 / C=0 → None（无从收紧）；C < λ（容量饱和）→ 0.0。
    #[test]
    fn critical_failure_fraction_solves_reference_values() {
        let f_c = critical_failure_fraction(2.0, 100.0, 20.0).expect("r>0 c>0");
        assert!((f_c - 0.4).abs() < 1e-9, "(C−λ)/(rC)={f_c}");
        assert!(
            critical_failure_fraction(0.0, 100.0, 20.0).is_none(),
            "r=0 无重试放大"
        );
        assert!(
            critical_failure_fraction(2.0, 0.0, 20.0).is_none(),
            "C=0 无容量"
        );
        assert_eq!(
            critical_failure_fraction(2.0, 10.0, 20.0),
            Some(0.0),
            "C < λ 容量饱和 → 任何失败率都超临界"
        );
    }

    /// 预算判定随 paused clock 推进（tokio Instant 语义）：advance 2s 后
    /// elapsed+delay 超出 1s 预算。manager 的循环级预算测试见
    /// vendor/manager.rs（同一 tokio 时钟域）。
    #[tokio::test(start_paused = true)]
    async fn budget_exceeded_advances_with_paused_clock() {
        let anchor = tokio::time::Instant::now();
        let budget = Duration::from_secs(1);
        // 刚起步：0 + 500ms ≤ 1s，不超预算
        assert!(!budget_exceeded(anchor, budget, Duration::from_millis(500)));
        // 虚拟时间推进 2s（生产路径的 tokio::time::sleep 由同一时钟驱动）
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(budget_exceeded(anchor, budget, Duration::from_millis(500)));
        assert!(budget_exceeded(anchor, budget, Duration::ZERO));
    }

    /// 守门器判定（收紧迫近临界失败率）：r=2、C=100、λ=20 → f_c=0.4、
    /// 0.8·f_c=0.32；f=0.35 收紧且 rf=0.7 不发散；f=0.30 不收紧；
    /// C<λ 容量饱和（f_c=0.0）→ 任何失败率都收紧。
    #[test]
    fn amplification_guard_tightens_near_critical_failure_fraction() {
        let (tighten, diverge) = amplification_guard(2.0, 0.35, 100.0, 20.0);
        assert!(tighten, "f=0.35 > 0.8·f_c=0.32 必须收紧");
        assert!(!diverge, "r·f=0.70 ≤ 0.95 不发散");

        let (tighten, diverge) = amplification_guard(2.0, 0.30, 100.0, 20.0);
        assert!(!tighten, "f=0.30 ≤ 0.8·f_c=0.32 不收紧");
        assert!(!diverge);

        let (tighten, _) = amplification_guard(2.0, 0.05, 10.0, 20.0);
        assert!(
            tighten,
            "C<λ 饱和域任何失败率都超临界（退化单尝试 fail-safe）"
        );
    }

    /// 发散告警：r·f > 0.95 → diverge；f_c 有解时收紧同时成立（发散阈值
    /// 0.95/r 恒大于 0.8·f_c，发散域必然已处收紧带）。
    #[test]
    fn amplification_guard_warns_divergence_and_still_tightens() {
        // r=1.5、f=0.8 → rf=1.2 发散；f_c=(100−20)/(1.5×100)≈0.533，f 超阈收紧
        let (tighten, diverge) = amplification_guard(1.5, 0.8, 100.0, 20.0);
        assert!(
            tighten && diverge,
            "发散域必须同时收紧: {tighten}/{diverge}"
        );
    }

    /// C=0（全网无显式并发上限）→ f_c 无解 → 不收紧只告警（fail-open）；
    /// rf>0.95 时发散告警仍要出；rf≤0.95 无信号。
    #[test]
    fn amplification_guard_without_capacity_cap_only_warns() {
        let (tighten, diverge) = amplification_guard(2.0, 0.6, 0.0, 5.0);
        assert!(!tighten, "C=0 无容量估计不得收紧");
        assert!(diverge, "r·f=1.2 > 0.95 发散告警");

        let (tighten, diverge) = amplification_guard(1.0, 0.5, 0.0, 5.0);
        assert!(!tighten && !diverge, "rf=0.5 无信号");
    }
}
