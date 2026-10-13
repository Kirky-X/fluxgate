// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! vecboost laya 决策引擎适配（`difficulty-classifier` feature 门控）：
//! 进程内直调 `EngineFactory::create`（task=Decision）+ `InferenceEngine::
//! decide`，不走 HTTP。阻塞推理由 orchestrator 以 `spawn_blocking`+超时包裹；
//! 本层只做：引擎加载、DecisionRequest 构造（有界 state）、P(high) 提取。
//!
//! 接口事实（vecboost 源码核实）：`AnyEngine` 实现 `InferenceEngine`
//!（engine/impl_..rs:31）；choice 答案带逐选项校准概率
//!（`DecisionAnswerBody::Choice.probabilities`）；引擎 token 预算超限
//! **拒绝不截断** → 文本有界性由 [`super::features`] 承诺（≤2KB）。

use vecboost::config::model::{DeviceType, EngineType, ModelConfig, ModelTask};
use vecboost::domain::decision::{
    DecisionAnswerBody, DecisionQuestion, DecisionRequest, DecisionResponse, QuestionType,
};
use vecboost::engine::{EngineFactory, InferenceEngine};

use super::classifier::{ClassifierError, DifficultyClassifier};
use super::features::RuleFeatures;
use std::sync::Arc;

/// choice 题名（答案按名回显，消费按下标取）
const QUESTION_NAME: &str = "difficulty";
/// 选项序：index 0 = low、1 = high（与 [`p_from_answer`] 提取逻辑绑定）
const OPTION_LOW: &str = "low";
const OPTION_HIGH: &str = "high";
/// instructions 钉死英文短句（laya checkpoint 英文口径，192 token 预算内）
const INSTRUCTIONS: &str =
    "Classify the user request difficulty. high = reasoning, math, multi-step code, \
     architecture or debugging; low = chat, translation, summarization, short factual.";

/// laya 决策引擎的 [`DifficultyClassifier`] 适配
pub struct LayaClassifier {
    engine: Arc<dyn InferenceEngine>,
}

impl LayaClassifier {
    /// 加载 laya checkpoint（阻塞：权重读盘 + 首次编译，bootstrap 一次性）。
    /// `engine` ∈ {"candle"（默认）| "onnx"}；onnx 需上游 `vecboost/onnx`
    /// feature（本 crate 未启用该组合时显性报错，不静默回退 candle）
    pub fn load(model_path: &str, engine: &str) -> Result<Self, String> {
        let normalized = engine.trim().to_ascii_lowercase();
        if normalized == "onnx" {
            return Err(
                "engine=onnx requires the vecboost/onnx feature combination; \
                 this build ships the candle decision path only"
                    .to_string(),
            );
        }
        let config = ModelConfig {
            name: "nexusflow-difficulty".to_string(),
            engine_type: EngineType::Candle,
            model_path: model_path.into(),
            task: ModelTask::Decision,
            device: DeviceType::Cpu,
            ..ModelConfig::default()
        };
        let any = EngineFactory::create(EngineType::Candle, &config)
            .map_err(|e| format!("laya engine load failed: {e}"))?;
        Ok(Self {
            engine: Arc::new(any),
        })
    }
}

impl DifficultyClassifier for LayaClassifier {
    fn p_high(&self, features: &RuleFeatures) -> Result<f64, ClassifierError> {
        let request = build_request(features);
        request
            .validate()
            .map_err(|e| ClassifierError::Engine(format!("decision request rejected: {e}")))?;
        let response = self
            .engine
            .decide(&request)
            .map_err(|e| ClassifierError::Engine(format!("laya decide failed: {e}")))?;
        p_from_response(&response).ok_or(ClassifierError::InvalidOutput)
    }
}

/// DecisionRequest 构造：state = 有界文本 + 结构特征（与 laya prompt 构造方
/// 约定的自由 JSON 契约）；单 choice 题，两选项
fn build_request(features: &RuleFeatures) -> DecisionRequest {
    DecisionRequest {
        state: serde_json::json!({
            "text": features.text,
            "num_messages": features.num_messages,
            "body_chars": features.body_chars,
            "has_tools": features.has_tools,
            "stream": features.stream,
            "hard_hits": features.hard_hits,
            "easy_hits": features.easy_hits,
            "question_marks": features.question_marks,
            "math_symbols": features.math_symbols,
            "code_fence": features.code_fence,
        }),
        questions: vec![DecisionQuestion {
            name: QUESTION_NAME.to_string(),
            qtype: QuestionType::Choice,
            instructions: INSTRUCTIONS.to_string(),
            options: vec![OPTION_LOW.to_string(), OPTION_HIGH.to_string()],
        }],
    }
}

/// 从响应提取 P(high)：choice 答案的校准概率表取 "high"；选项缺失/下标
/// 越界/概率非有限 → None（调用方折算 InvalidOutput）
fn p_from_response(response: &DecisionResponse) -> Option<f64> {
    let answer = response
        .answers
        .iter()
        .find(|a| a.question == QUESTION_NAME)?;
    match &answer.answer {
        DecisionAnswerBody::Choice { probabilities, .. } => {
            let p = *probabilities.get(OPTION_HIGH)?;
            if p.is_finite() {
                Some(f64::from(p.clamp(0.0, 1.0)))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape_has_single_choice_question() {
        let f = RuleFeatures::neutral();
        let request = build_request(&f);
        assert_eq!(request.questions.len(), 1);
        assert_eq!(request.questions[0].options, vec!["low", "high"]);
        assert!(request.validate().is_ok(), "中性特征也必须过 vecboost 校验");
    }

    /// DecisionResponse 为 Serialize-only（出站契约），测试用结构体字面量构造
    fn choice_response(question: &str, probabilities: &[(&str, f32)]) -> DecisionResponse {
        use std::collections::BTreeMap;
        DecisionResponse {
            answers: vec![vecboost::domain::decision::DecisionAnswer {
                question: question.to_string(),
                answer: DecisionAnswerBody::Choice {
                    index: 0,
                    option: "low".to_string(),
                    probabilities: probabilities
                        .iter()
                        .map(|(k, v)| (k.to_string(), *v))
                        .collect::<BTreeMap<_, _>>(),
                },
            }],
            processing_time_ms: 1,
        }
    }

    #[test]
    fn p_extraction_from_choice_answer() {
        let p = p_from_response(&choice_response(
            "difficulty",
            &[("low", 0.2), ("high", 0.8)],
        ))
        .expect("P(high)");
        assert!((p - 0.8).abs() < 1e-6);
    }

    #[test]
    fn p_extraction_missing_or_invalid() {
        // 缺 "high" 键 → None
        assert_eq!(
            p_from_response(&choice_response("difficulty", &[("low", 1.0)])),
            None
        );
        // 非本题名 → None
        assert_eq!(
            p_from_response(&choice_response("other", &[("low", 0.5), ("high", 0.5)])),
            None
        );
    }
}
