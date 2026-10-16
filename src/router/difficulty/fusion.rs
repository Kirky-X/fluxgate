// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 定档纯函数：规则计分 `r`、融合 `S = (w_clf·p + w_rule·r)/(w_clf+w_rule)`、
//! 三档切分与中带策略。全部确定性、无 IO 无锁（规则7：阈值与切分是
//! 确定性逻辑，模型只产出概率）。契约：阈值已过 `validate_difficulty`
//!（score_low < score_high、权重不全零），本层不重复校验、只做有限性防御。

use crate::config::Tier;

use super::features::RuleFeatures;

// ---- 规则计分常量（v1 源码内置，不进 toml；调整走版本迭代）----
/// hard 词命中项：上限 5 次封顶，占 0.40
const W_HARD: f64 = 0.40;
const HARD_CAP: f64 = 5.0;
/// 代码围栏出现：固定 0.15
const W_CODE_FENCE: f64 = 0.15;
/// 数学符号密度：12 个封顶，占 0.15
const W_MATH: f64 = 0.15;
const MATH_CAP: f64 = 12.0;
/// 多问拆解：3 问封顶，占 0.10
const W_QUESTIONS: f64 = 0.10;
const QUESTION_CAP: f64 = 3.0;
/// 结构维（消息数 + tools）：合计 0.10
const W_MESSAGES: f64 = 0.05;
const MESSAGE_CAP: f64 = 10.0;
const W_TOOLS: f64 = 0.05;
/// 请求体长度：8000 字符封顶，占 0.10
const W_LENGTH: f64 = 0.10;
const LENGTH_CAP: f64 = 8000.0;
/// easy 词惩罚：3 次封顶，最多扣 0.30
const W_EASY_PENALTY: f64 = 0.30;
const EASY_CAP: f64 = 3.0;

/// 规则计分 `r ∈ [0,1]`：各信号项加权求和，easy 命中线性扣减，全项封顶
/// 防单信号刷分。输出钳 \[0,1\]
pub fn rule_score(f: &RuleFeatures) -> f64 {
    let hard = (f.hard_hits as f64).min(HARD_CAP) / HARD_CAP * W_HARD;
    let code = if f.code_fence { W_CODE_FENCE } else { 0.0 };
    let math = (f.math_symbols as f64).min(MATH_CAP) / MATH_CAP * W_MATH;
    let questions = (f.question_marks as f64).min(QUESTION_CAP) / QUESTION_CAP * W_QUESTIONS;
    let messages = (f.num_messages as f64).min(MESSAGE_CAP) / MESSAGE_CAP * W_MESSAGES;
    let tools = if f.has_tools { W_TOOLS } else { 0.0 };
    let length = (f.body_chars as f64).min(LENGTH_CAP) / LENGTH_CAP * W_LENGTH;
    let easy_penalty = (f.easy_hits as f64).min(EASY_CAP) / EASY_CAP * W_EASY_PENALTY;

    (hard + code + math + questions + messages + tools + length - easy_penalty).clamp(0.0, 1.0)
}

/// 融合分 `S = (w_clf·p + w_rule·r) / (w_clf + w_rule) ∈ [0,1]`。
/// 权重和为 0 返回 0.0（配置校验 V1 已拒绝，防御分支）；非有限输入按
/// 0 贡献处理（fail-safe，正常路径由调用方错误分类拦截）
pub fn fuse(w_clf: f64, p: f64, w_rule: f64, r: f64) -> f64 {
    let total = w_clf + w_rule;
    if !total.is_finite() || total <= 0.0 {
        return 0.0;
    }
    let p = if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let r = if r.is_finite() {
        r.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let wc = if w_clf.is_finite() && w_clf > 0.0 {
        w_clf
    } else {
        0.0
    };
    let wr = if w_rule.is_finite() && w_rule > 0.0 {
        w_rule
    } else {
        0.0
    };
    let total = wc + wr;
    if total <= 0.0 {
        return 0.0;
    }
    ((wc * p + wr * r) / total).clamp(0.0, 1.0)
}

/// 中带策略（仅 `tiers.mid` 未配置的两档退化形态消费）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidBandPolicy {
    /// 中带 → high（质量优先：错误不对称，不确定时往强档倒）
    Strong,
    /// 中带 → low（成本优先）
    Cheap,
    /// 距哪个阈值近算哪档，平局取 high
    Nearest,
}

/// 策略解析：trim + ASCII 小写；未知名 None（配置校验 V3 拒绝，此处防御）
pub fn parse_mid_band_policy(raw: &str) -> Option<MidBandPolicy> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strong" => Some(MidBandPolicy::Strong),
        "cheap" => Some(MidBandPolicy::Cheap),
        "nearest" => Some(MidBandPolicy::Nearest),
        _ => None,
    }
}

/// 三档切分（边界确定性，钉死测试）：`S ≥ score_high → High`；
/// `S ≤ score_low → Low`；区间内 mid 已配置 → Mid，未配置按策略。
/// 阈值倒置未定义（V2 已拒）；非有限 S 按 0.0（fail-safe → Low）
pub fn decide_band(
    s: f64,
    score_low: f64,
    score_high: f64,
    mid_configured: bool,
    policy: MidBandPolicy,
) -> Tier {
    let s = if s.is_finite() { s } else { 0.0 };
    if s >= score_high {
        return Tier::High;
    }
    if s <= score_low {
        return Tier::Low;
    }
    if mid_configured {
        return Tier::Mid;
    }
    match policy {
        MidBandPolicy::Strong => Tier::High,
        MidBandPolicy::Cheap => Tier::Low,
        MidBandPolicy::Nearest => {
            if s - score_low < score_high - s {
                Tier::Low
            } else {
                Tier::High
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(hard: u32, easy: u32) -> RuleFeatures {
        RuleFeatures {
            hard_hits: hard,
            easy_hits: easy,
            ..RuleFeatures::neutral()
        }
    }

    #[test]
    fn rule_score_bounds_and_easy_penalty() {
        assert_eq!(rule_score(&features(0, 0)), 0.0, "零信号 → 0");
        assert_eq!(rule_score(&features(0, 100)), 0.0, "纯 easy 扣到下限钳 0");
        let hard_only = rule_score(&features(100, 0));
        assert!(
            (hard_only - W_HARD).abs() < 1e-12,
            "hard 封顶单项 = 0.35: {hard_only}"
        );
    }

    #[test]
    fn rule_score_full_signal_reaches_one() {
        let f = RuleFeatures {
            hard_hits: 10,
            easy_hits: 0,
            question_marks: 5,
            math_symbols: 20,
            code_fence: true,
            num_messages: 20,
            has_tools: true,
            body_chars: 20_000,
            stream: false,
            text: String::new(),
        };
        assert_eq!(rule_score(&f), 1.0, "全维满信号钳上限");
    }

    #[test]
    fn fuse_pure_rule_when_w_clf_zero() {
        assert_eq!(fuse(0.0, 0.99, 0.4, 0.5), 0.5, "w_clf=0 退化为纯规则");
        assert_eq!(fuse(0.6, 1.0, 0.4, 1.0), 1.0);
        assert_eq!(fuse(0.6, 0.0, 0.4, 0.0), 0.0);
    }

    #[test]
    fn fuse_normalizes_and_clamps() {
        // 权重和不等于 1 时归一
        let s = fuse(2.0, 0.5, 2.0, 0.5);
        assert_eq!(s, 0.5);
        // 非有限概率按 0 贡献
        let s = fuse(0.6, f64::NAN, 0.4, 1.0);
        assert_eq!(s, 0.4);
    }

    #[test]
    fn decide_band_boundaries_are_deterministic() {
        let pol = MidBandPolicy::Strong;
        assert_eq!(
            decide_band(0.65, 0.35, 0.65, true, pol),
            Tier::High,
            "S==上阈"
        );
        assert_eq!(
            decide_band(0.35, 0.35, 0.65, true, pol),
            Tier::Low,
            "S==下阈"
        );
        assert_eq!(decide_band(0.5, 0.35, 0.65, true, pol), Tier::Mid);
        assert_eq!(
            decide_band(f64::NAN, 0.35, 0.65, true, pol),
            Tier::Low,
            "非有限 fail-safe"
        );
    }

    #[test]
    fn decide_band_mid_absent_policies() {
        let cases = [
            (0.36, MidBandPolicy::Strong, Tier::High),
            (0.64, MidBandPolicy::Strong, Tier::High),
            (0.36, MidBandPolicy::Cheap, Tier::Low),
            (0.64, MidBandPolicy::Cheap, Tier::Low),
            (0.36, MidBandPolicy::Nearest, Tier::Low),
            (0.64, MidBandPolicy::Nearest, Tier::High),
            // 平局：0.5 距两阈相等 → high
            (0.5, MidBandPolicy::Nearest, Tier::High),
        ];
        for (s, policy, want) in cases {
            assert_eq!(
                decide_band(s, 0.35, 0.65, false, policy),
                want,
                "s={s} policy={policy:?}"
            );
        }
    }

    #[test]
    fn parse_mid_band_policy_normalizes() {
        assert_eq!(parse_mid_band_policy("strong"), Some(MidBandPolicy::Strong));
        assert_eq!(parse_mid_band_policy(" CHEAP "), Some(MidBandPolicy::Cheap));
        assert_eq!(
            parse_mid_band_policy("Nearest"),
            Some(MidBandPolicy::Nearest)
        );
        assert_eq!(parse_mid_band_policy("medium"), None);
    }
}
