// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

pub mod affinity;
pub mod bandit;
pub mod difficulty;
#[allow(clippy::module_inception)]
pub mod router;

pub use affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
pub use difficulty::{CachedDecision, DecisionCache, DifficultyRouter, TierDecision, TierPath};
pub use router::{
    prefix_candidates, prefix_hash_key, RouteCtx, Router, RoutingDecision, RoutingStrategy,
};

/// 权重纯函数导出 — 集成测试与运维调试的确定性断言入口（无 RNG、无 IO）。
pub use router::compute_weight;

/// 在途相对化纯函数导出 — load_signal="in_flight_relative" 的换算逻辑。
pub use router::relative_in_flight_to_len;

/// 亲和 boost 乘子纯函数导出 — soft 模式命中权重的确定性单测断言入口。
pub use router::apply_affinity_boost;

/// SLA 虚拟队列打分纯函数导出 — 惩罚因子 / 对偶更新 / 缺价中位数。
pub use router::{sla_dual_update, sla_median_cost, sla_penalty_factor};

/// LinUCB 打分导出 — 状态、特征提取与 d 维纯函数（d=2 手算参照单测入口）。
pub use bandit::{
    linucb_arm_key, linucb_arm_update, linucb_diag_inverse, linucb_features,
    linucb_features_from_body, linucb_invert, linucb_ucb_from_arm, BanditState, LINUCB_DIM,
};
