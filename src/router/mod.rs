// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

pub mod affinity;
pub mod bandit;
#[allow(clippy::module_inception)]
pub mod router;

pub use affinity::{AffinityTable, DEFAULT_AFFINITY_TTL};
pub use router::{Router, RoutingDecision, RoutingStrategy};

/// recId=10：权重纯函数导出 — 集成测试（tests/router_test.rs）与运维调试
/// 的确定性断言入口（无 RNG、无 IO，给定系数与因子输入可精确复现）。
pub use router::compute_weight;

/// recId=6：在途相对化纯函数导出 — load_signal="in_flight_relative" 的
/// 换算逻辑（production-stack relative_load → queue_factor 0..100 输入域），
/// 单测断言入口。
pub use router::relative_in_flight_to_len;

/// recId=10(1)：亲和 boost 乘子纯函数导出 — soft 模式命中权重的确定性
/// 单测断言入口（StdRng 固定种子选择断言的前置）。
pub use router::apply_affinity_boost;

/// recId=14：SLA 虚拟队列打分纯函数导出 — 惩罚因子 / 对偶更新 / 缺价
/// 中位数的确定性单测断言入口（compute_weight 同款先例）。
pub use router::{sla_dual_update, sla_median_cost, sla_penalty_factor};

/// recId=15：LinUCB 打分导出 — 状态（Router 经 with_bandit 接线、ApiState
/// 持有供奖励回传）、特征提取与 d 维纯函数（d=2 手算参照单测入口）。
pub use bandit::{
    linucb_arm_key, linucb_arm_update, linucb_diag_inverse, linucb_features,
    linucb_features_from_body, linucb_invert, linucb_ucb_from_arm, BanditState, LINUCB_DIM,
};
