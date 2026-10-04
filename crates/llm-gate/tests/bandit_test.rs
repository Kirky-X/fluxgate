// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! LinUCB bandit 纯函数与状态的集成侧单测——覆盖 d 维求逆
//! 防御分支、部分主元交换、特征提取边界、argmax 的 NaN 兜底与平局决断。

#![cfg(feature = "router")]

#[cfg(test)]
mod bandit_units {
    use llm_gate::router::bandit::{
        linucb_diag_inverse, linucb_features, linucb_features_from_body, linucb_invert,
        linucb_ucb_from_arm, BanditState, LINUCB_DIM,
    };
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    // ---- linucb_invert：防御分支与成功路径 ----

    /// d=0、形状不匹配、非有限输入 → None（调用方据此走对角退化）。
    #[test]
    fn invert_rejects_zero_dim_shape_mismatch_and_non_finite() {
        assert!(linucb_invert(&[], 0).is_none(), "d=0 必须拒绝");
        assert!(
            linucb_invert(&[1.0, 0.0, 0.0, 1.0], 3).is_none(),
            "len != d*d 必须拒绝"
        );
        assert!(linucb_invert(&[1.0; 9], 2).is_none(), "len 9 != 4 必须拒绝");
        assert!(
            linucb_invert(&[f64::NAN, 0.0, 0.0, 1.0], 2).is_none(),
            "NaN 必须拒绝"
        );
        assert!(
            linucb_invert(&[f64::INFINITY, 0.0, 0.0, 1.0], 2).is_none(),
            "inf 必须拒绝"
        );
    }

    /// d=3 对角阵求逆精确（归一 / 消元 / 回填全链路），且非对角元素为 0。
    #[test]
    fn invert_exact_inverse_for_d3_diagonal() {
        let a = [2.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 8.0];
        let inv = linucb_invert(&a, 3).expect("对角阵可逆");
        let expect = [0.5, 0.0, 0.0, 0.0, 0.25, 0.0, 0.0, 0.0, 0.125];
        for (got, want) in inv.iter().zip(expect) {
            assert!((got - want).abs() < 1e-12, "inv {got} != {want}");
        }
    }

    /// 部分主元选择：主元不在对角位时交换行（swap 分支），结果仍正确。
    #[test]
    fn invert_partial_pivot_swaps_rows() {
        // 首列主元为 0，必须交换行后才能消元
        let a = [0.0, 1.0, 1.0, 0.0];
        let inv = linucb_invert(&a, 2).expect("交换矩阵可逆");
        // 置换矩阵的逆 = 自身
        let expect = [0.0, 1.0, 1.0, 0.0];
        for (got, want) in inv.iter().zip(expect) {
            assert!((got - want).abs() < 1e-12, "inv {got} != {want}");
        }
    }

    /// d=3 非对角稠密阵求逆：与 A·A⁻¹ = I 的乘积校验（消元数值正确性）。
    #[test]
    fn invert_dense_d3_satisfies_a_times_inv_equals_identity() {
        let a = [4.0, 1.0, 0.0, 1.0, 3.0, 1.0, 0.0, 1.0, 5.0];
        let inv = linucb_invert(&a, 3).expect("正定阵可逆");
        for i in 0..3 {
            for j in 0..3 {
                let mut dot = 0.0;
                for k in 0..3 {
                    dot += a[i * 3 + k] * inv[k * 3 + j];
                }
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(
                    (dot - want).abs() < 1e-9,
                    "A·A⁻¹[{i}][{j}] = {dot} != {want}"
                );
            }
        }
    }

    /// 对角退化近似：零矩阵对角取 1/ε（保留量级信号、方向信息丢失）。
    #[test]
    fn diag_inverse_uses_epsilon_floor() {
        let diag = linucb_diag_inverse(&[0.0; 9], 3);
        for i in 0..3 {
            assert!(diag[i * 3 + i] > 1e11, "零对角必须取 1/ε 量级");
        }
        // 非零对角按绝对值倒数
        let diag2 = linucb_diag_inverse(&[4.0, 0.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 8.0], 3);
        assert!((diag2[0] - 0.25).abs() < 1e-12);
        assert!((diag2[4] - 0.5).abs() < 1e-12, "负对角按 |a_ii| 倒数");
        assert!((diag2[8] - 0.125).abs() < 1e-12);
    }

    // ---- 特征提取边界 ----

    /// 手工 5 维特征与 LINUCB_DIM 一致；空请求（全零边界）各维取值正确。
    #[test]
    fn features_match_dim_and_edge_values() {
        assert_eq!(LINUCB_DIM, 5);
        let f = linucb_features(0, 0, false, false);
        assert_eq!(f, [1.0, 0.0, 0.0, 0.0, 0.0], "零请求边界：截距 1、其余 0");
        let g = linucb_features(1, 1, true, true);
        assert_eq!(g[0], 1.0);
        assert!((g[1] - 2.0_f64.ln()).abs() < 1e-12);
        assert!((g[2] - 2.0_f64.ln()).abs() < 1e-12);
        assert_eq!(g[3], 1.0);
        assert_eq!(g[4], 1.0);
    }

    /// 从 body 提取：messages 数、body 序列化长度、tools 非空、stream 标志；
    /// 空 body 与 tools 空数组走缺省分支。
    #[test]
    fn features_from_body_extracts_all_variants() {
        // 空 body：序列化 "{}" 2 字符 → body_chars = ln(3)；其余维度 0
        let empty = linucb_features_from_body(&serde_json::json!({}));
        assert_eq!(empty[0], 1.0);
        assert_eq!(empty[1], 0.0, "无 messages → 0 条");
        assert!(
            (empty[2] - 3.0_f64.ln()).abs() < 1e-12,
            "body_chars = ln(1+2)"
        );
        assert_eq!(empty[3], 0.0);
        assert_eq!(empty[4], 0.0);

        // tools 空数组 → has_tools=false；stream 缺省 false
        let no_tools = linucb_features_from_body(&serde_json::json!({
            "messages": [],
            "tools": []
        }));
        assert_eq!(no_tools[3], 0.0, "tools 空数组不算 has_tools");
        assert_eq!(no_tools[4], 0.0, "stream 缺省 false");

        // 齐全字段
        let full = linucb_features_from_body(&serde_json::json!({
            "model": "m",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function"}]
        }));
        assert!((full[1] - 2.0_f64.ln()).abs() < 1e-12, "1 条消息");
        assert!(full[2] > 0.0, "序列化非空");
        assert_eq!(full[3], 1.0);
        assert_eq!(full[4], 1.0);
    }

    /// UCB 打分的 α 防御：非有限 α 按 0（纯贪心 mean 项），负 α 同样钳 0。
    #[test]
    fn ucb_clamps_non_finite_alpha_to_zero() {
        // A=I、b=0 → mean=0；探索项 α·√(xᵀx)：α=2 → 2·1
        let identity = [1.0, 0.0, 0.0, 1.0];
        let b = [0.0, 0.0];
        let x = [1.0, 0.0];
        let u_alpha2 = linucb_ucb_from_arm(&identity, &b, &x, 2.0, 2);
        assert!((u_alpha2 - 2.0).abs() < 1e-12, "探索项 = α·√quad");
        for alpha in [f64::NAN, f64::INFINITY, -1.0] {
            let u = linucb_ucb_from_arm(&identity, &b, &x, alpha, 2);
            assert_eq!(u, 0.0, "α={alpha} 必须钳 0（纯贪心）");
        }
    }

    // ---- BanditState 生命周期与 argmax 决断 ----

    /// 状态生命周期：new → 无臂；首次 update/ucb 惰性建臂；has_arm 精确键匹配；
    /// arm_count 反映规模。
    #[test]
    fn bandit_state_lifecycle_and_observers() {
        let state = BanditState::new(1.0);
        assert_eq!(state.arm_count(), 0, "初始无臂");

        let x = [1.0, 0.0, 0.0, 0.0, 0.0];
        state.update("m", "v1", &x, 1.0);
        assert_eq!(state.arm_count(), 1, "首次 update 惰性建臂");
        assert!(state.has_arm("m", "v1"));
        assert!(!state.has_arm("m", "v2"), "键 (model, vendor) 精确匹配");
        assert!(!state.has_arm("M", "v1"), "键不归一（调用方负责归一）");

        // ucb 触达已存在臂不重复建臂
        let before = state.arm_count();
        let u = state.ucb("m", "v1", &x, 1.0);
        assert!(u.is_finite());
        assert_eq!(state.arm_count(), before, "已存在臂不重复创建");

        // ucb 触达新 (model, vendor) 建新臂
        let _ = state.ucb("m", "v2", &x, 1.0);
        assert_eq!(state.arm_count(), 2);
    }

    /// argmax 空候选集返回 None。
    #[test]
    fn argmax_empty_candidates_returns_none() {
        let state = BanditState::new(1.0);
        let mut rng = StdRng::seed_from_u64(1);
        assert!(state
            .argmax("m", &[], &[1.0, 0.0, 0.0, 0.0, 0.0], 1.0, &mut rng)
            .is_none());
    }

    /// 分数溢出（inf）兜底：特征量级大 + 已采样臂的 A⁻¹b 非零 → quad 项
    /// 溢出为 inf → argmax 走均匀随机兜底（两候选都可出现，不 panic）。
    #[test]
    fn argmax_non_finite_scores_falls_back_to_uniform() {
        let state = BanditState::new(1.0);
        let unit = [1.0, 0.0, 0.0, 0.0, 0.0];
        state.update("m", "v1", &unit, 1.0);

        // ucb(v1)：quad = MAX × (MAX/2) = inf → 探索项 sqrt(inf) → ucb = inf
        let huge = [f64::MAX, 0.0, 0.0, 0.0, 0.0];
        let mut saw_a = false;
        let mut saw_b = false;
        for seed in 0..40u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let pick = state
                .argmax("m", &["v1", "v2"], &huge, 1.0, &mut rng)
                .expect("候选非空");
            assert!(pick < 2);
            saw_a |= pick == 0;
            saw_b |= pick == 1;
        }
        assert!(saw_a && saw_b, "非有限分数必须均匀随机兜底（两候选都出现）");
    }

    /// 平局 rng 决断：两臂同先验同特征 → UCB 相等，固定 seed 可复现。
    #[test]
    fn argmax_tie_broken_by_rng_reproducibly() {
        let state = BanditState::new(1.0);
        let x = [1.0, 0.0, 0.0, 0.0, 0.0];
        let mut rng_a = StdRng::seed_from_u64(7);
        let mut rng_b = StdRng::seed_from_u64(7);
        let a = state
            .argmax("m", &["v1", "v2"], &x, 1.0, &mut rng_a)
            .unwrap();
        let b = state
            .argmax("m", &["v1", "v2"], &x, 1.0, &mut rng_b)
            .unwrap();
        assert_eq!(a, b, "同 seed 同选择");

        // 多 seed 下两臂都可能出现（平局不固定偏向）
        let mut saw_v1 = false;
        let mut saw_v2 = false;
        for seed in 0..40u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let pick = state.argmax("m", &["v1", "v2"], &x, 1.0, &mut rng).unwrap();
            saw_v1 |= pick == 0;
            saw_v2 |= pick == 1;
        }
        assert!(saw_v1 && saw_v2, "平局决断必须两臂都可被选中");
    }

    /// 特征非法（长度/非有限）时 ucb 中性 0.0、update 丢弃脏样本（A/b 不被
    /// 污染——污染后打分仍有限）。
    #[test]
    fn invalid_features_are_neutral_and_discarded() {
        let state = BanditState::new(1.0);
        let x = [1.0, 0.0, 0.0, 0.0, 0.0];

        // ucb：长度不符 → 0.0
        assert_eq!(state.ucb("m", "v1", &[1.0, 2.0], 1.0), 0.0);
        // ucb：NaN 特征 → 0.0
        assert_eq!(
            state.ucb("m", "v1", &[1.0, f64::NAN, 0.0, 0.0, 0.0], 1.0),
            0.0
        );

        // update：长度不符 / NaN 特征 / NaN 奖励均丢弃
        state.update("m", "v1", &[1.0, 2.0], 1.0);
        state.update("m", "v1", &[f64::NAN, 0.0, 0.0, 0.0, 0.0], 1.0);
        state.update("m", "v1", &x, f64::NAN);
        assert!(!state.has_arm("m", "v1"), "脏样本不得建臂");

        // 干净样本照常建臂、打分有限
        state.update("m", "v1", &x, 1.0);
        assert!(state.has_arm("m", "v1"));
        assert!(state.ucb("m", "v1", &x, 1.0).is_finite());
    }
}
