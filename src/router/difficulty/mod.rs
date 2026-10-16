// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 难度定档路由：规则计分 + 进程内分类概率加权融合 → 三档切分 → 改写
//! 目标抽象模型名。带内厂商选择不在本模块职责内（复用 `Router` 现有链）。
//!
//! 模块布局（规则14：本文件仅接口与 re-export）：
//! - [`features`]：特征提取（纯函数，有界）
//! - [`fusion`]：规则计分 + 融合 + 三档切分（纯函数，确定性）
//! - [`classifier`]：分类器 trait + Mock
//! - [`guard`]：熔断降级状态机
//! - `cache`：定档结果缓存
//! - `vecboost_engine`：`difficulty-classifier` feature 门控的 laya 适配
//! - `orchestrator`：[`DifficultyRouter`] 编排与旁路判定

pub mod classifier;
pub mod features;
pub mod fusion;
pub mod guard;

#[cfg(feature = "difficulty-classifier")]
pub mod vecboost_engine;

mod cache;
mod orchestrator;

pub use cache::{CachedDecision, DecisionCache};
pub use classifier::{ClassifierError, DifficultyClassifier, MockDifficultyClassifier};
pub use features::RuleFeatures;
pub use fusion::{decide_band, fuse, rule_score, MidBandPolicy};
pub use guard::Guard;
pub use orchestrator::{resolve_tier, DifficultyRouter, TierDecision, TierPath};

/// 档位机器标签（X-Difficulty-Tier 头与观测计数用，稳定不本地化）
pub fn tier_label(tier: crate::config::Tier) -> &'static str {
    match tier {
        crate::config::Tier::Low => "low",
        crate::config::Tier::Mid => "mid",
        crate::config::Tier::High => "high",
    }
}
