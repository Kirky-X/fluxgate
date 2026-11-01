// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 规则检测器：组装 inklog [`SecretScanGate`]（扩展形态表 + 熵扫描）与
//! 自定义词表，把命中归因到 [`MaskKind`](super::MaskKind)。
//!
//! - 内置形态走 inklog `SecretPatternRegistry::with_extended()`（9 条裸
//!   secret + email/password 赋值等扩展形态——「宁可多掩」的非日志消费面）；
//! - 熵扫描补无形态高熵 token（归因 [`MaskKind::Token`]）；
//! - `rules` 自定义正则注册进同一张表（归因 [`MaskKind::Custom`]），编译
//!   失败构造期 `Err`（启动拒绝）；
//! - `words` 词表不做正则（词面精确匹配 / ASCII 大小写不敏感两态），命中
//!   归因 [`MaskKind::Custom`]。
//!
//! [`MaskDetector::detect`] 纯读：只报告命中，不改写、不计数。

use super::{MaskDetector, MaskError, MaskHit, MaskKind};
use inklog::{EntropyScanner, SecretPatternRegistry, SecretScanGate};
use std::ops::Range;

/// inklog 检测面 pattern 名 → 种类归因（未知名兜底 [`MaskKind::Custom`]，
/// 未来 inklog 新增形态时默认走自定义档，不静默丢检）。
fn pattern_kind(pattern: &str) -> MaskKind {
    match pattern {
        "anthropic_sk" | "openai_sk" | "github_token" | "slack_token" | "aws_access_key"
        | "google_api_key" | "stripe_key" | "prefixed_key" | "pk_key" => MaskKind::ApiKey,
        "pem_private_block" => MaskKind::PrivateKey,
        "jwt" | "high_entropy" => MaskKind::Token,
        "email" => MaskKind::Email,
        _ => MaskKind::Custom,
    }
}

/// 词表命中扫描：字节滑窗精确匹配（`any_case` 时 ASCII 大小写不敏感，
/// 多字节词不受影响）；命中区间必须落在 UTF-8 字符边界上。
fn find_word_spans(text: &str, word: &str, any_case: bool) -> Vec<Range<usize>> {
    let (tb, wb) = (text.as_bytes(), word.as_bytes());
    if wb.is_empty() || wb.len() > tb.len() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i + wb.len() <= tb.len() {
        let hit = if any_case {
            tb[i..i + wb.len()]
                .iter()
                .zip(wb)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        } else {
            &tb[i..i + wb.len()] == wb
        };
        if hit && text.is_char_boundary(i) && text.is_char_boundary(i + wb.len()) {
            spans.push(i..i + wb.len());
            i += wb.len();
        } else {
            i += 1;
        }
    }
    spans
}

/// 规则检测器：inklog 门 + 词表。构造后不可变（模式表冻结），`Clone` 共享
/// inklog 门计数器。
#[derive(Clone, Debug)]
pub struct RuleDetector {
    gate: SecretScanGate,
    words: Vec<String>,
    words_any_case: bool,
}

impl RuleDetector {
    /// 构造检测器：`words` 词表、`words_any_case` 大小写开关、`rules`
    /// 自定义正则。正则编译失败返回 [`MaskError::InvalidRule`]（启动拒绝）。
    pub fn build(
        words: &[String],
        words_any_case: bool,
        rules: &[String],
    ) -> Result<Self, MaskError> {
        let mut registry = SecretPatternRegistry::with_extended();
        for (idx, rule) in rules.iter().enumerate() {
            registry
                .register(&format!("custom_rule_{idx}"), rule, "***MASKED_CUSTOM***")
                .map_err(|e| MaskError::InvalidRule {
                    detail: e.to_string(),
                })?;
        }
        let gate = SecretScanGate::new(&registry).with_entropy(EntropyScanner::new());
        Ok(Self {
            gate,
            words: words.to_vec(),
            words_any_case,
        })
    }

    /// 校验规则集可编译（loader 加载/热更两处调用的启动拒绝面；
    /// `config` 侧不依赖 `masking` feature，仅 feature 启用时接入）。
    pub fn validate_rules(rules: &[String]) -> Result<(), MaskError> {
        let mut registry = SecretPatternRegistry::default();
        for (idx, rule) in rules.iter().enumerate() {
            registry
                .register(&format!("custom_rule_{idx}"), rule, "***MASKED_CUSTOM***")
                .map_err(|e| MaskError::InvalidRule {
                    detail: e.to_string(),
                })?;
        }
        Ok(())
    }
}

impl MaskDetector for RuleDetector {
    fn detect(&self, text: &str) -> Vec<MaskHit> {
        // inklog 检测面（纯读不计数）→ 种类归因
        let mut hits: Vec<MaskHit> = self
            .gate
            .scan(text)
            .into_iter()
            .map(|m| MaskHit {
                kind: pattern_kind(&m.pattern),
                span: m.start..m.end,
            })
            .collect();
        // 词表命中 → Custom
        for word in &self.words {
            for span in find_word_spans(text, word, self.words_any_case) {
                hits.push(MaskHit {
                    kind: MaskKind::Custom,
                    span,
                });
            }
        }
        // 区间归并：起点升序、长命中优先；同 span 全等平局自定义优先
        //（操作者声明的词/正则表达本地分类知识，压过内置形态归因）；
        // 重叠丢弃后者（确定性单替换面）
        hits.sort_by(|a, b| {
            a.span
                .start
                .cmp(&b.span.start)
                .then(b.span.len().cmp(&a.span.len()))
                .then_with(|| (b.kind == MaskKind::Custom).cmp(&(a.kind == MaskKind::Custom)))
        });
        let mut merged: Vec<MaskHit> = Vec::with_capacity(hits.len());
        for hit in hits {
            match merged.last() {
                Some(prev) if hit.span.start < prev.span.end => continue,
                _ => merged.push(hit),
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_kinds_attributed() {
        let d = RuleDetector::build(&[], false, &[]).expect("builtin detector");
        let text = "k sk-proj-abcdefghijklmnopqrstuvwxyz123456 t";
        let hits = d.detect(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MaskKind::ApiKey);
        assert_eq!(
            &text[hits[0].span.clone()],
            "sk-proj-abcdefghijklmnopqrstuvwxyz123456"
        );
    }

    #[test]
    fn word_scan_exact_and_any_case() {
        let d = RuleDetector::build(&["内部门牌".to_string()], false, &[]).unwrap();
        let hits = d.detect("前缀内部门牌后缀");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MaskKind::Custom);
        assert_eq!(&"前缀内部门牌后缀"[hits[0].span.clone()], "内部门牌");

        let ci = RuleDetector::build(&["Token".to_string()], true, &[]).unwrap();
        assert_eq!(ci.detect("a tOkE b").len(), 0, "部分匹配不命中");
        assert_eq!(ci.detect("a ToKeN b").len(), 1, "any_case 命中");
        let cs = RuleDetector::build(&["Token".to_string()], false, &[]).unwrap();
        assert_eq!(cs.detect("a ToKeN b").len(), 0, "精确匹配不命中变体");
    }

    #[test]
    fn custom_rule_compiles_and_hits() {
        let d = RuleDetector::build(&[], false, &["AKIA[0-9A-Z]{16}".to_string()]).unwrap();
        let text = "id AKIAIOSFODNN7EXAMPLE end";
        let hits = d.detect(text);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, MaskKind::Custom);
        assert_eq!(&text[hits[0].span.clone()], "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn invalid_rule_rejected_at_build() {
        let err = RuleDetector::build(&[], false, &["[invalid".to_string()]).unwrap_err();
        assert!(matches!(err, MaskError::InvalidRule { .. }), "{err:?}");
        assert!(RuleDetector::validate_rules(&["ok-[a-z]+".to_string()]).is_ok());
        assert!(RuleDetector::validate_rules(&["(bad".to_string()]).is_err());
    }

    #[test]
    fn overlapping_spans_keep_longest_first() {
        // PEM 块内叠加词命中：先到的长命中保留，重叠的丢弃
        let d = RuleDetector::build(
            &["PRIVATE".to_string()],
            false,
            &[r"-----BEGIN RSA PRIVATE KEY-----".to_string()],
        )
        .unwrap();
        let text = "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----";
        let hits = d.detect(text);
        assert!(
            hits.iter().any(|h| h.kind == MaskKind::PrivateKey),
            "PEM 块必须命中: {hits:?}"
        );
        // 归并后任意两命中不重叠
        for pair in hits.windows(2) {
            assert!(
                pair[0].span.end <= pair[1].span.start,
                "命中区间不得重叠: {pair:?}"
            );
        }
    }
}
