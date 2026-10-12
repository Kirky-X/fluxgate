// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! LinUCB 打分项（零模型上下文 bandit，selection 第三档）— 每臂 =
//! `(abstract_model, vendor_id)` 一个线性模型，选择按 UCB argmax（闭式
//! 最小二乘 θ̂，r = 成功 1 / 失败 0 代理奖励）。per-臂 Mutex + 臂表
//! RwLock（读侧容忍稍旧值），状态重启归零。默认关闭：`[routing].selection`
//! 为 "weighted" 时不咨询、无状态更新。

use rand::RngExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// 人工特征维度（起步集）：[1（截距）, ln(1+消息数),
/// ln(1+body 字符数), 是否含 tools, 是否流式]。
pub const LINUCB_DIM: usize = 5;

/// 对角退化近似的最小分母（防除零；正则初值下实际不可达）
const DIAG_EPS: f64 = 1e-12;

/// 臂表类型 — (abstract_model, vendor_id) → 共享臂（per-臂 Mutex）。
type ArmMap = HashMap<(String, String), Arc<Mutex<Arm>>>;

/// 单臂线性模型 — `A`（d×d 行主序）、`b`（d 维）。私有，经
/// [`BanditState`] 的 per-臂 Mutex 访问。
struct Arm {
    a: Vec<f64>,
    b: Vec<f64>,
}

impl Arm {
    /// 正则初值 A₀ = α²·I（α≤0/非有限取 1.0 兜底，保 A 恒正定可逆）；b₀ = 0。
    fn new(alpha_reg: f64) -> Self {
        let mut a = vec![0.0; LINUCB_DIM * LINUCB_DIM];
        for i in 0..LINUCB_DIM {
            a[i * LINUCB_DIM + i] = alpha_reg;
        }
        Self {
            a,
            b: vec![0.0; LINUCB_DIM],
        }
    }
}

/// LinUCB 臂键归一（选择侧与奖励侧共用，保证两端同键）：
/// auto → "__global__"，trim + 小写。
pub fn linucb_arm_key(raw_model: &str) -> String {
    let normalized = raw_model.trim().to_lowercase();
    if normalized == "auto" {
        "__global__".to_string()
    } else {
        normalized
    }
}

/// 人工 5 维特征 — 对数压缩的规模量（ln(1+n) 有界增长）+
/// 两个 0/1 标志。`pub` 供确定性单测与 handler 特征提取。
pub fn linucb_features(
    num_messages: usize,
    body_chars: usize,
    has_tools: bool,
    stream: bool,
) -> [f64; LINUCB_DIM] {
    [
        1.0,
        (1.0 + num_messages as f64).ln(),
        (1.0 + body_chars as f64).ln(),
        if has_tools { 1.0 } else { 0.0 },
        if stream { 1.0 } else { 0.0 },
    ]
}

/// 从请求体提取特征（chat 端点共用）：messages 数、body 序列化
/// 字符数、tools 非空、stream 标志。
pub fn linucb_features_from_body(body: &serde_json::Value) -> [f64; LINUCB_DIM] {
    let num_messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let body_chars = serde_json::to_string(body).map(|s| s.len()).unwrap_or(0);
    let has_tools = body
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    linucb_features(num_messages, body_chars, has_tools, stream)
}

/// 部分主元高斯-约当求逆（d×d 行主序）。奇异（主元幅度低于
/// 阈值）或含非有限元素时返回 None（调用方退化对角近似）。`pub` 供确定性单测。
pub fn linucb_invert(a: &[f64], d: usize) -> Option<Vec<f64>> {
    if d == 0 || a.len() != d * d {
        return None;
    }
    if a.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut m = augment_with_identity(a, d);
    gauss_jordan_eliminate(&mut m, d)?;
    extract_inverse(&m, d)
}

/// 增广矩阵 [A | I]（2d 列行主序）。
fn augment_with_identity(a: &[f64], d: usize) -> Vec<f64> {
    let w = 2 * d;
    let mut m = vec![0.0; d * w];
    for i in 0..d {
        for j in 0..d {
            m[i * w + j] = a[i * d + j];
        }
        m[i * w + d + i] = 1.0;
    }
    m
}

/// 部分主元：`col` 列对角线以下（含）绝对值最大的行 (row, |m[row,col]|)。
fn select_pivot(m: &[f64], d: usize, w: usize, col: usize) -> (usize, f64) {
    (col..d)
        .map(|r| (r, m[r * w + col].abs()))
        .fold(
            (col, 0.0_f64),
            |(bi, bm), (r, mag)| {
                if mag > bm {
                    (r, mag)
                } else {
                    (bi, bm)
                }
            },
        )
}

/// 部分主元高斯-约当消元（就地）。主元幅度低于阈值（奇异）或主元非有限
/// 时返回 None（正则初值下仅 NaN 污染可触发）。
fn gauss_jordan_eliminate(m: &mut [f64], d: usize) -> Option<()> {
    let w = 2 * d;
    const PIVOT_EPS: f64 = 1e-12;
    for col in 0..d {
        // 部分主元：当前列绝对值最大的行
        let (pivot_row, pivot_mag) = select_pivot(m, d, w, col);
        if !pivot_mag.is_finite() || pivot_mag < PIVOT_EPS {
            return None; // 奇异
        }
        if pivot_row != col {
            for j in 0..w {
                m.swap(col * w + j, pivot_row * w + j);
            }
        }
        let piv = m[col * w + col];
        for j in 0..w {
            m[col * w + j] /= piv;
        }
        for r in 0..d {
            if r == col {
                continue;
            }
            let factor = m[r * w + col];
            if factor == 0.0 {
                continue;
            }
            for j in 0..w {
                m[r * w + j] -= factor * m[col * w + j];
            }
        }
    }
    Some(())
}

/// 提取右半 [I] 部分为逆矩阵；消元中途溢出/污染（非有限元素）返回 None。
fn extract_inverse(m: &[f64], d: usize) -> Option<Vec<f64>> {
    let w = 2 * d;
    let mut inv = vec![0.0; d * d];
    for i in 0..d {
        for j in 0..d {
            let v = m[i * w + d + j];
            if !v.is_finite() {
                return None; // 消元中途溢出/污染
            }
            inv[i * d + j] = v;
        }
    }
    Some(inv)
}

/// 对角退化近似 — `inv(A) ≈ diag(1/max(|a_ii|, ε))`。求逆失败
/// （奇异/非有限）时的 fail-safe：保留量级信号、方向信息丢失。
pub fn linucb_diag_inverse(a: &[f64], d: usize) -> Vec<f64> {
    let mut inv = vec![0.0; d * d];
    for i in 0..d {
        inv[i * d + i] = 1.0 / a[i * d + i].abs().max(DIAG_EPS);
    }
    inv
}

/// UCB 打分：`⟨x, A⁻¹b⟩ + α√(xᵀA⁻¹x)`；α 非有限按 0（纯贪心），
/// 二次型 max(0,·) 防负数开方。pub 供确定性单测。
pub fn linucb_ucb_from_arm(a_inv: &[f64], b: &[f64], x: &[f64], alpha: f64, d: usize) -> f64 {
    let alpha = if alpha.is_finite() {
        alpha.max(0.0)
    } else {
        0.0
    };
    // tmp = A⁻¹·b 与 A⁻¹·x 共用一趟矩阵-向量乘
    let mut ab = vec![0.0; d];
    let mut ax = vec![0.0; d];
    for i in 0..d {
        let (mut si, mut ti) = (0.0, 0.0);
        for j in 0..d {
            si += a_inv[i * d + j] * b[j];
            ti += a_inv[i * d + j] * x[j];
        }
        ab[i] = si;
        ax[i] = ti;
    }
    let (mut mean, mut quad) = (0.0, 0.0);
    for i in 0..d {
        mean += x[i] * ab[i];
        quad += x[i] * ax[i];
    }
    mean + alpha * quad.max(0.0).sqrt()
}

/// 闭式更新（只动被选臂）— `A += xxᵀ`、`b += r·x`。
/// x 长度必须 == d（调用方门控）。`pub` 供确定性单测。
pub fn linucb_arm_update(a: &mut [f64], b: &mut [f64], x: &[f64], r: f64, d: usize) {
    for i in 0..d {
        for j in 0..d {
            a[i * d + j] += x[i] * x[j];
        }
        b[i] += r * x[i];
    }
}

/// LinUCB 状态：臂表键 = `(abstract_model, vendor_id)`，臂按需惰性创建
///（先验 = 正则初值）；per-臂 Mutex + 臂表 RwLock。
pub struct BanditState {
    arms: RwLock<ArmMap>,
    /// 正则初值（max(α², 1)·I；构造期定死，热更 α 只影响探索半径）
    alpha_reg: f64,
    /// 奇异/非有限退化对角近似的进程内一次性 warn（防刷屏）
    diag_fallback_warned: AtomicBool,
}

impl BanditState {
    pub fn new(ucb_alpha: f64) -> Self {
        let alpha_reg = if ucb_alpha.is_finite() && ucb_alpha > 0.0 {
            (ucb_alpha * ucb_alpha).max(1.0)
        } else {
            1.0
        };
        Self {
            arms: RwLock::new(HashMap::new()),
            alpha_reg,
            diag_fallback_warned: AtomicBool::new(false),
        }
    }

    /// 臂惰性获取：读快路径命中 clone，未命中落写锁 or_insert（并发首触只建一次）
    fn arm(&self, key: &(String, String)) -> Arc<Mutex<Arm>> {
        if let Some(a) = self
            .arms
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
        {
            return a.clone();
        }
        self.arms
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Arm::new(self.alpha_reg))))
            .clone()
    }

    /// 候选臂的 UCB 打分（读侧；容忍稍旧值）。特征长度/有限性不符时
    /// 中性 0.0 打分（不获得 argmax 优势也不 panic）。
    pub fn ucb(&self, model: &str, vendor: &str, x: &[f64], alpha: f64) -> f64 {
        if x.len() != LINUCB_DIM || !x.iter().all(|v| v.is_finite()) {
            return 0.0;
        }
        let key = (model.to_string(), vendor.to_string());
        let arm = self.arm(&key);
        let guard = arm.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match linucb_invert(&guard.a, LINUCB_DIM) {
            Some(inv) => linucb_ucb_from_arm(&inv, &guard.b, x, alpha, LINUCB_DIM),
            None => {
                if !self.diag_fallback_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        model,
                        vendor,
                        "linucb arm matrix singular/non-finite; diagonal approximation engaged"
                    );
                }
                let inv = linucb_diag_inverse(&guard.a, LINUCB_DIM);
                linucb_ucb_from_arm(&inv, &guard.b, x, alpha, LINUCB_DIM)
            }
        }
    }

    /// 闭式更新（只动被选臂；r = 成功 1 / 失败 0）。特征长度/有限性不符
    /// 或 r 非有限时丢弃（fail-safe：脏样本不进 A/b）。
    pub fn update(&self, model: &str, vendor: &str, x: &[f64], r: f64) {
        if x.len() != LINUCB_DIM || !x.iter().all(|v| v.is_finite()) || !r.is_finite() {
            return;
        }
        let key = (model.to_string(), vendor.to_string());
        let arm = self.arm(&key);
        let mut guard = arm.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Arm { a, b } = &mut *guard;
        linucb_arm_update(a, b, x, r, LINUCB_DIM);
    }

    /// 选择核心 — 全部候选算 UCB 取 argmax，平局（|Δ|≤1e-9）rng 决断。
    /// 返回候选索引；全空候选集 None；分数全非有限时均匀随机兜底。
    pub fn argmax(
        &self,
        model: &str,
        vendor_ids: &[&str],
        x: &[f64],
        alpha: f64,
        rng: &mut dyn rand::Rng,
    ) -> Option<usize> {
        if vendor_ids.is_empty() {
            return None;
        }
        let scores: Vec<f64> = vendor_ids
            .iter()
            .map(|v| self.ucb(model, v, x, alpha))
            .collect();
        let best = scores.iter().fold(f64::NEG_INFINITY, |m, s| m.max(*s));
        if !best.is_finite() {
            return Some(rng.random_range(0..vendor_ids.len()));
        }
        const TIE_EPS: f64 = 1e-9;
        let tied: Vec<usize> = scores
            .iter()
            .enumerate()
            .filter(|(_, s)| (**s - best).abs() <= TIE_EPS)
            .map(|(i, _)| i)
            .collect();
        Some(tied[rng.random_range(0..tied.len())])
    }

    /// 臂存在性（测试观测：选择/奖励两端键对齐的断言入口）
    pub fn has_arm(&self, model: &str, vendor: &str) -> bool {
        self.arms
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&(model.to_string(), vendor.to_string()))
    }

    /// 已创建臂的数量（测试与观测用）。
    pub fn arm_count(&self) -> usize {
        self.arms
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// d=2 手算参照：A₀=I，update(x=[1,0], r=1) → A=[[2,0],[0,1]]、
    /// b=[1,0] → θ̂=[0.5,0]；ucb([1,0], α=1) = 0.5 + √0.5。
    #[test]
    fn d2_closed_form_update_and_ucb_exact() {
        let mut a = vec![1.0, 0.0, 0.0, 1.0];
        let mut b = vec![0.0, 0.0];
        linucb_arm_update(&mut a, &mut b, &[1.0, 0.0], 1.0, 2);
        assert_eq!(a, vec![2.0, 0.0, 0.0, 1.0]);
        assert_eq!(b, vec![1.0, 0.0]);

        let inv = linucb_invert(&a, 2).expect("A 可逆");
        assert!((inv[0] - 0.5).abs() < 1e-12 && inv[1].abs() < 1e-12);
        assert!(inv[2].abs() < 1e-12 && (inv[3] - 1.0).abs() < 1e-12);

        let u = linucb_ucb_from_arm(&inv, &b, &[1.0, 0.0], 1.0, 2);
        assert!((u - (0.5 + 0.5_f64.sqrt())).abs() < 1e-12, "ucb={u}");
        // 正交方向无样本：θ̂ 分量为 0，纯探索项 √(xᵀA⁻¹x)=√1
        let u_orth = linucb_ucb_from_arm(&inv, &b, &[0.0, 1.0], 1.0, 2);
        assert!((u_orth - 1.0).abs() < 1e-12, "ucb_orth={u_orth}");

        // 第二笔 update(x=[0,1], r=0)：A=[[2,0],[0,2]]，b 不变
        linucb_arm_update(&mut a, &mut b, &[0.0, 1.0], 0.0, 2);
        let inv2 = linucb_invert(&a, 2).expect("A 可逆");
        let u2 = linucb_ucb_from_arm(&inv2, &b, &[0.0, 1.0], 1.0, 2);
        assert!(
            (u2 - 0.5_f64.sqrt()).abs() < 1e-12,
            "采样后探索半径收缩: {u2}"
        );
    }

    /// 特征提取：对数压缩 + 0/1 标志，[1, ln(1+msgs), ln(1+chars), tools, stream]。
    #[test]
    fn features_manual_dimensions() {
        let f = linucb_features(3, 100, true, false);
        assert_eq!(f[0], 1.0);
        assert!((f[1] - 4.0_f64.ln()).abs() < 1e-12);
        assert!((f[2] - 101.0_f64.ln()).abs() < 1e-12);
        assert_eq!(f[3], 1.0);
        assert_eq!(f[4], 0.0);
        // 零请求（空 body 边界）：ln(1)=0
        let z = linucb_features(0, 0, false, false);
        assert_eq!(z[1], 0.0);
        assert_eq!(z[2], 0.0);
    }

    /// 从 body 提取：messages 数、tools 非空、stream 标志。
    #[test]
    fn features_from_body_extracts_dimensions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function"}]
        });
        let f = linucb_features_from_body(&body);
        assert_eq!(f[0], 1.0);
        assert!((f[1] - 2.0_f64.ln()).abs() < 1e-12, "1 条消息");
        assert_eq!(f[3], 1.0, "tools 非空");
        assert_eq!(f[4], 1.0, "stream=true");
        assert!(f[2] > 0.0, "body 序列化非空");
    }

    /// 奇异/非有限矩阵：求退返回 None；对角退化近似正确；UCB 全链路不 panic。
    #[test]
    fn singular_matrix_falls_back_without_panic() {
        // 行线性相关 → 奇异
        assert!(linucb_invert(&[1.0, 2.0, 2.0, 4.0], 2).is_none());
        // NaN/Inf 污染 → None
        assert!(linucb_invert(&[f64::NAN, 0.0, 0.0, 1.0], 2).is_none());
        assert!(linucb_invert(&[f64::INFINITY, 0.0, 0.0, 1.0], 2).is_none());
        // 零矩阵 → 对角退化取 1/ε
        let diag = linucb_diag_inverse(&[0.0, 0.0, 0.0, 0.0], 2);
        assert!((diag[0] - 1.0 / DIAG_EPS).abs() < 1e-9);
        // 退化近似喂进 UCB 不 panic、结果有限
        let u = linucb_ucb_from_arm(&diag, &[0.0, 0.0], &[1.0, 1.0], 1.0, 2);
        assert!(u.is_finite());
    }

    /// 冷启动探索：一臂多次失败更新（θ̂[0]→0、探索半径收缩），另一臂
    /// 保持先验（探索半径 1）→ argmax 必选未更新臂。
    #[test]
    fn cold_start_explores_unsampled_arm() {
        let state = BanditState::new(1.0);
        let x = vec![1.0, 0.0, 0.0, 0.0, 0.0];
        for _ in 0..5 {
            state.update("m", "bad", &x, 0.0);
        }
        let mut rng = StdRng::seed_from_u64(42);
        let pick = state
            .argmax("m", &["bad", "fresh"], &x, 1.0, &mut rng)
            .expect("候选非空");
        assert_eq!(pick, 1, "未采样臂 UCB 更高（探索语义）");
        // 反向：fresh 臂更新成功多次后反超
        for _ in 0..5 {
            state.update("m", "fresh", &x, 1.0);
        }
        let mut rng = StdRng::seed_from_u64(42);
        let pick = state
            .argmax("m", &["bad", "fresh"], &x, 1.0, &mut rng)
            .unwrap();
        assert_eq!(pick, 1, "成功臂 mean 项占优");
    }

    /// 平局 rng 决断：两臂同先验（全零 b）同特征 → UCB 相等，固定 seed
    /// 下选择可复现，且两臂都可能被选（不同 seed）。
    #[test]
    fn tie_broken_by_rng_reproducibly() {
        let state = BanditState::new(1.0);
        let x = vec![1.0, 0.0, 0.0, 0.0, 0.0];
        let mut rng_a = StdRng::seed_from_u64(7);
        let mut rng_b = StdRng::seed_from_u64(7);
        let mut rng_c = StdRng::seed_from_u64(8);
        let a = state
            .argmax("m", &["v1", "v2"], &x, 1.0, &mut rng_a)
            .unwrap();
        let b = state
            .argmax("m", &["v1", "v2"], &x, 1.0, &mut rng_b)
            .unwrap();
        let c = state
            .argmax("m", &["v1", "v2"], &x, 1.0, &mut rng_c)
            .unwrap();
        assert_eq!(a, b, "同 seed 同选择");
        let _ = c; // 不同 seed 允许不同（不断言具体值，只断言域内）
        assert!(a < 2 && c < 2);
    }

    /// 奖励更新改变 UCB：update 前后同臂同特征打分可变（状态面断言）。
    #[test]
    fn reward_update_changes_ucb() {
        let state = BanditState::new(1.0);
        let x = vec![1.0, 0.0, 0.0, 0.0, 0.0];
        let before = state.ucb("m", "v1", &x, 1.0);
        state.update("m", "v1", &x, 0.0);
        let after_fail = state.ucb("m", "v1", &x, 1.0);
        assert!(
            after_fail < before,
            "失败奖励压低 UCB: {after_fail} !< {before}"
        );
        state.update("m", "v1", &x, 1.0);
        let after_success = state.ucb("m", "v1", &x, 1.0);
        assert!(after_success > after_fail, "成功奖励抬高 UCB");
    }

    /// 臂键归一：auto→"__global__"、大小写、trim
    #[test]
    fn linucb_arm_key_normalizes_selection_and_reward() {
        assert_eq!(linucb_arm_key("auto"), "__global__");
        assert_eq!(linucb_arm_key("AUTO"), "__global__");
        assert_eq!(linucb_arm_key("GPT-4"), "gpt-4");
        assert_eq!(linucb_arm_key("gpt-4"), "gpt-4");
        assert_eq!(linucb_arm_key("  Claude-3  "), "claude-3");
    }

    /// has_arm 观测：键精确匹配
    #[test]
    fn has_arm_exact_key_match() {
        let state = BanditState::new(1.0);
        let x = vec![1.0, 0.0, 0.0, 0.0, 0.0];
        state.update("gpt-4", "v1", &x, 1.0);
        assert!(state.has_arm("gpt-4", "v1"));
        assert!(!state.has_arm("GPT-4", "v1"), "键不归一（调用方负责归一）");
        assert!(!state.has_arm("gpt-4", "v2"));
    }

    /// 臂惰性创建：update/ucb 触达即建臂；arm_count 反映状态规模。
    #[test]
    fn arms_created_lazily_per_model_vendor() {
        let state = BanditState::new(1.0);
        assert_eq!(state.arm_count(), 0);
        let x = vec![1.0, 0.0, 0.0, 0.0, 0.0];
        state.update("mA", "v1", &x, 1.0);
        state.update("mA", "v2", &x, 1.0);
        state.update("mB", "v1", &x, 1.0);
        assert_eq!(state.arm_count(), 3, "(model, vendor) 各一臂");
        let _ = state.ucb("mA", "v1", &x, 1.0);
        assert_eq!(state.arm_count(), 3, "已存在臂不重复创建");
    }
}
