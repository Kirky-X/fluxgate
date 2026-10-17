// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 难度分类器 trait：定档融合的分类概率源。生产实现为
//! `vecboost_engine`（`difficulty-classifier` feature 门控）；
//! [`MockDifficultyClassifier`] 供确定性单测与降级路径测试。

use super::features::RuleFeatures;

/// 分类错误分类（F5/F6 降级判定的输入）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierError {
    /// 推理超时（classifier.timeout_ms）
    Timeout,
    /// 引擎错误（加载失败/推理失败，含 F3 的不可用态）
    Engine(String),
    /// 输出非法：非有限/越界 \[0,1\]/选项缺失
    InvalidOutput,
    /// 熔断 open 期未调用直接拒绝（Guard 语义，非引擎错误）
    Unavailable,
}

/// 难度分类器：输出 P(high) ∈ \[0,1\]。实现方为潜在阻塞推理
///（laya），调用方以 `spawn_blocking` + 超时包裹（见 vecboost_engine）
pub trait DifficultyClassifier: Send + Sync {
    fn p_high(&self, features: &RuleFeatures) -> Result<f64, ClassifierError>;
}

/// 确定性 Mock：按预置脚本逐次返回（queue 语义），空脚本恒 Unavailable。
/// 测试用 `std::sync::Mutex<Vec<Result>>` 驱动，不引入 tokio 依赖
#[derive(Default)]
pub struct MockDifficultyClassifier {
    script: std::sync::Mutex<Vec<Result<f64, ClassifierError>>>,
    pub calls: std::sync::atomic::AtomicUsize,
}

impl MockDifficultyClassifier {
    pub fn new(script: Vec<Result<f64, ClassifierError>>) -> Self {
        Self {
            script: std::sync::Mutex::new(script),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn ok(value: f64) -> Self {
        Self::new(vec![Ok(value)])
    }
}

impl DifficultyClassifier for MockDifficultyClassifier {
    fn p_high(&self, _features: &RuleFeatures) -> Result<f64, ClassifierError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self
            .script
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 空脚本 = 引擎不可用；脚本耗尽 = 持续可用（末值 sticky）
        if guard.is_empty() {
            return Err(ClassifierError::Unavailable);
        }
        if guard.len() == 1 {
            return guard[0].clone();
        }
        guard.remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features() -> RuleFeatures {
        RuleFeatures::neutral()
    }

    #[test]
    fn mock_script_queue_then_sticky() {
        let mock =
            MockDifficultyClassifier::new(vec![Ok(0.9), Err(ClassifierError::Timeout), Ok(0.1)]);
        assert_eq!(mock.p_high(&features()), Ok(0.9));
        assert_eq!(mock.p_high(&features()), Err(ClassifierError::Timeout));
        assert_eq!(mock.p_high(&features()), Ok(0.1));
        // 耗尽后 sticky 末值（模拟持续可用引擎）
        assert_eq!(mock.p_high(&features()), Ok(0.1));
        assert_eq!(mock.calls.load(std::sync::atomic::Ordering::Relaxed), 4);
    }

    #[test]
    fn mock_empty_script_unavailable() {
        let mock = MockDifficultyClassifier::default();
        assert_eq!(mock.p_high(&features()), Err(ClassifierError::Unavailable));
    }
}
