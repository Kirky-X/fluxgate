// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 出站 JSON 掩码遍历：对最终出站 body 的全部字符串值（messages/system/
//! tools/tool_result 等协议无关面）做检测 + 按 kind 三档动作（Mask 替换 /
//! Warn 只计数 / Block 整请求拒绝），命中值经 [`PlaceholderRegistry`]
//! 替换为确定性占位符。
//!
//! 约定：`allow` 白名单（精确值或「条目为值前缀」）跳过；`kinds` 未启用的
//! 种类零动作零元数据；Block 命中立即返回 [`MaskError::Blocked`]，body 保持
//! 发现 Block 前已处理的状态（调用方拒发整个请求，不部分发送）。

use super::{MaskAction, MaskDetector, MaskError, MaskKind, MaskTicket, PlaceholderRegistry};
use serde_json::Value;
use std::collections::HashMap;

/// 单次掩码命中元数据（种类/长度）——admin 统计面口径，不含值本身
///（值前缀也不携带：任何出口面都不落原值片段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskedHitInfo {
    pub kind: MaskKind,
    /// 原值字节长度
    pub len: usize,
}

/// kind → 动作映射（`[masking].actions` 消费面）；未配置种类缺省
/// [`MaskAction::Mask`]。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionMap {
    actions: HashMap<MaskKind, MaskAction>,
}

impl ActionMap {
    /// 登记某种类动作（配置侧 kind 串经 [`MaskKind::parse`] 解析后写入）。
    pub fn set(&mut self, kind: MaskKind, action: MaskAction) {
        self.actions.insert(kind, action);
    }

    /// 解析某种类动作；未登记缺省 [`MaskAction::Mask`]。
    pub fn resolve(&self, kind: MaskKind) -> MaskAction {
        self.actions.get(&kind).copied().unwrap_or(MaskAction::Mask)
    }
}

/// `allow` 白名单：条目精确命中或作为值前缀时跳过掩码。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowList {
    entries: Vec<String>,
}

impl AllowList {
    /// 由配置串构造（`[masking].allow`）。
    pub fn from_values(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            entries: values.into_iter().collect(),
        }
    }

    /// 值是否豁免：与任一条目全等，或以任一条目为前缀。
    pub fn is_allowed(&self, value: &str) -> bool {
        self.entries
            .iter()
            .any(|e| value == e || value.starts_with(e.as_str()))
    }
}

/// 启用种类集（`[masking].kinds` 消费面）；缺省全启用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindSet {
    enabled: Vec<MaskKind>,
}

impl KindSet {
    /// 全种类集。
    pub fn all() -> Self {
        Self {
            enabled: MaskKind::all().to_vec(),
        }
    }

    /// 指定种类集（配置串经 [`MaskKind::parse`] 解析后写入）。
    pub fn from_kinds(kinds: impl IntoIterator<Item = MaskKind>) -> Self {
        Self {
            enabled: kinds.into_iter().collect(),
        }
    }

    /// 种类是否启用。
    pub fn contains(&self, kind: MaskKind) -> bool {
        self.enabled.contains(&kind)
    }
}

/// 对出站 body 做掩码遍历。返回命中元数据列表（种类/长度）；`Block` 动作
/// 命中返回 [`MaskError::Blocked`]。所有生成的占位符同步登记进 `ticket`
///（回程还原的放行判据）。
pub fn mask_json(
    body: &mut Value,
    detector: &dyn MaskDetector,
    actions: &ActionMap,
    kinds: &KindSet,
    allow: &AllowList,
    registry: &PlaceholderRegistry,
    ticket: &MaskTicket,
) -> Result<Vec<MaskedHitInfo>, MaskError> {
    let mut infos = Vec::new();
    walk_value(
        body, detector, actions, kinds, allow, registry, ticket, &mut infos,
    )?;
    Ok(infos)
}

/// 递归下钻：字符串值处理，数组/对象遍历，标量不动。
#[allow(clippy::too_many_arguments)]
fn walk_value(
    value: &mut Value,
    detector: &dyn MaskDetector,
    actions: &ActionMap,
    kinds: &KindSet,
    allow: &AllowList,
    registry: &PlaceholderRegistry,
    ticket: &MaskTicket,
    infos: &mut Vec<MaskedHitInfo>,
) -> Result<(), MaskError> {
    match value {
        Value::String(s) => {
            mask_string(s, detector, actions, kinds, allow, registry, ticket, infos)
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                walk_value(
                    item, detector, actions, kinds, allow, registry, ticket, infos,
                )?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                walk_value(v, detector, actions, kinds, allow, registry, ticket, infos)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 单字符串处理：白名单豁免 → 检测 → 种类过滤 → 逐命中三档动作。
/// 替换按区间**从右向左**套用（字节偏移不受左侧替换影响）。
#[allow(clippy::too_many_arguments)]
fn mask_string(
    s: &mut String,
    detector: &dyn MaskDetector,
    actions: &ActionMap,
    kinds: &KindSet,
    allow: &AllowList,
    registry: &PlaceholderRegistry,
    ticket: &MaskTicket,
    infos: &mut Vec<MaskedHitInfo>,
) -> Result<(), MaskError> {
    if s.is_empty() {
        return Ok(());
    }
    let hits = detector.detect(s);
    // 逐命中白名单豁免（条目精确值或值前缀）+ 种类过滤
    let applied: Vec<_> = hits
        .into_iter()
        .filter(|hit| kinds.contains(hit.kind) && !allow.is_allowed(&s[hit.span.clone()]))
        .collect();
    if applied.is_empty() {
        return Ok(());
    }
    // Block 短路在改写之前：整请求拒绝，不做部分替换
    for hit in &applied {
        if actions.resolve(hit.kind) == MaskAction::Block {
            return Err(MaskError::Blocked { kind: hit.kind });
        }
    }
    let original = s.clone();
    for hit in applied.iter().rev() {
        let value = &original[hit.span.clone()];
        infos.push(MaskedHitInfo {
            kind: hit.kind,
            len: value.len(),
        });
        if actions.resolve(hit.kind) == MaskAction::Warn {
            continue;
        }
        let placeholder = registry.mask(hit.kind, value);
        ticket.record(&placeholder);
        s.replace_range(hit.span.clone(), &placeholder);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masking::RuleDetector;
    use serde_json::json;
    use std::sync::Arc;

    fn detector() -> impl MaskDetector {
        RuleDetector::build(&[], false, &[]).expect("detector")
    }

    #[test]
    fn scalar_and_empty_string_untouched() {
        let mut body = json!({"n": 1, "b": true, "nil": null, "s": "", "arr": [1.5, false]});
        let infos = mask_json(
            &mut body,
            &detector(),
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::default(),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .unwrap();
        assert_eq!(infos.len(), 0);
        assert_eq!(
            body,
            json!({"n": 1, "b": true, "nil": null, "s": "", "arr": [1.5, false]})
        );
    }

    #[test]
    fn whole_string_allow_fast_path() {
        let mut body = json!({"s": "sk-proj-abcdefghijklmnopqrstuvwxyz123456"});
        let infos = mask_json(
            &mut body,
            &detector(),
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::from_values(["sk-proj-abcdefghijklmnopqrstuvwxyz123456".to_string()]),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .unwrap();
        assert_eq!(infos.len(), 0);
        assert_eq!(
            body,
            json!({"s": "sk-proj-abcdefghijklmnopqrstuvwxyz123456"})
        );
    }

    #[test]
    fn prefix_allow_entry_skips_longer_value() {
        let mut body = json!({"s": "sk-proj-abcdefghijklmnopqrstuvwxyz123456"});
        let infos = mask_json(
            &mut body,
            &detector(),
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::from_values(["sk-proj-".to_string()]),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .unwrap();
        assert_eq!(infos.len(), 0, "前缀条目豁免更长值");
    }

    #[test]
    fn dyn_detector_and_block_priority_before_rewrite() {
        let d: Arc<dyn MaskDetector> = Arc::new(detector());
        let mut body = json!({"a": "x sk-proj-abcdefghijklmnopqrstuvwxyz123456 y"});
        let mut actions = ActionMap::default();
        actions.set(MaskKind::ApiKey, MaskAction::Block);
        let err = mask_json(
            &mut body,
            d.as_ref(),
            &actions,
            &KindSet::all(),
            &AllowList::default(),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            MaskError::Blocked {
                kind: MaskKind::ApiKey
            }
        );
    }
}
