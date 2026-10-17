// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 嵌入兼容组 pin 表 + 响应维度守门记录 — 进程内存状态。
//!
//! 核心不变量：一个抽象嵌入模型 = 一个向量空间 = 一个兼容组。
//! - pin 表（abstract_id → 组键）：首写者胜（语义对齐 `AffinityTable::bind_if_absent`）；
//!   组键 = 条目声明的 `compat_group`（trim 后非空）或单例键
//!   `"{vendor_id}/::{model_id}"`（未声明组 = 不与任何厂商互换的安全默认）；
//! - 维度记录（(vendor_id, model_id, dims_param_key) → 首见维度）：嵌入成功响应
//!   首见记录放行，后续不一致返回 `Err((旧维度, 新维度))` 供上层拦截；
//! - 热载成功路径 `clear_all` 清空全部状态重新粘着/重新观察；不持久化、
//!   不跨实例同步（实例间一致性由配置层面的组声明保证）。

use std::collections::HashMap;
use std::sync::RwLock;

/// 嵌入粘着表：兼容组 pin + 响应维度守门记录
#[derive(Default)]
pub struct EmbeddingPin {
    /// abstract_id（路由归一后的查找键）→ 组键
    pins: RwLock<HashMap<String, String>>,
    /// (vendor_id, model_id, dims_param_key) → 首见响应维度；
    /// dims_param_key 为请求 `dimensions` 参数值或 `"-"`（无参数）
    dims: RwLock<HashMap<(String, String, String), u32>>,
}

impl EmbeddingPin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 组键解析：声明 `compat_group`（trim 后非空）用声明值；未声明或空白
    /// 条目 = 单例组键 `"{vendor_id}/::{model_id}"`（与配置校验同口径）
    pub fn group_key(vendor_id: &str, model_id: &str, compat_group: Option<&str>) -> String {
        match compat_group
            .map(str::trim)
            .filter(|group| !group.is_empty())
        {
            Some(group) => group.to_string(),
            None => format!("{vendor_id}/::{model_id}"),
        }
    }

    /// 查询 abstract 当前的 pin 组键
    pub fn pin_for(&self, abstract_id: &str) -> Option<String> {
        self.pins
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(abstract_id)
            .cloned()
    }

    /// 首写者胜：未 pin → 写入候选组键返回 `(组键, true)`；已 pin → 返回
    /// 存量组键与 `false`。并发首请求只有一个写入者胜出，全体请求收敛到
    /// 同一组键（写锁串行化 entry 判空与插入，无 check-then-act 窗口）
    pub fn pin_or_get(&self, abstract_id: &str, group_key: &str) -> (String, bool) {
        use std::collections::hash_map::Entry;

        let mut pins = self
            .pins
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match pins.entry(abstract_id.to_string()) {
            Entry::Vacant(entry) => {
                entry.insert(group_key.to_string());
                (group_key.to_string(), true)
            }
            Entry::Occupied(entry) => (entry.get().clone(), false),
        }
    }

    /// 维度守门：首见记录并放行（`Ok`）；已有记录且一致放行；不一致返回
    /// `Err((旧维度, 新维度))`，由上层换算为协议错误（不在此改写记录 —
    /// 保持旧值为基准，持续暴露漂移）。
    /// `dims_param_key` 只接受受控字面量（现调用点恒 `"-"`）；若未来接入
    /// 用户可控的 `dimensions` 参数值，必须先数值化归一（限数字串）再作键
    /// ——裸用户输入会使键空间无界（内存耗尽面）
    pub fn record_and_check(
        &self,
        vendor_id: &str,
        model_id: &str,
        dims_param_key: &str,
        dims: u32,
    ) -> Result<(), (u32, u32)> {
        use std::collections::hash_map::Entry;

        let key = (
            vendor_id.to_string(),
            model_id.to_string(),
            dims_param_key.to_string(),
        );
        let mut dims_map = self
            .dims
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match dims_map.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(dims);
                Ok(())
            }
            Entry::Occupied(entry) => {
                let old = *entry.get();
                if old == dims {
                    Ok(())
                } else {
                    Err((old, dims))
                }
            }
        }
    }

    /// 清空全部状态（pin 表 + 维度记录）— 配置应用成功路径（`ConfigManager`
    /// 应用成功钩子）调用：变更后的厂商/模型拓扑与声明维度可能已变，旧配置
    /// 时代的记录一并失效，新请求按新配置重新粘着、重新观察。
    /// 返回被清除的 pin 条数（供重置计数；维度记录条数不在返回值内）
    pub fn clear_all(&self) -> usize {
        let cleared = {
            let mut pins = self
                .pins
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let count = pins.len();
            pins.clear();
            count
        };
        self.dims
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        cleared
    }

    /// 单条重置：该 abstract 存在 pin 并移除返回 true；不存在幂等返回
    /// false（admin 重置路径据此计数「实际移除」）
    pub fn clear_one(&self, abstract_id: &str) -> bool {
        self.pins
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(abstract_id)
            .is_some()
    }

    /// 清除指定 (vendor, model) 的维度守门基准（admin 重置级联：上游模型
    /// 确已变更时，仅清 pin 不清基准会让下一请求立刻再 502）。返回是否
    /// 存在并移除了记录
    pub fn clear_dims_for(&self, vendor_id: &str, model_id: &str) -> bool {
        self.dims
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(vendor_id.to_string(), model_id.to_string(), "-".to_string()))
            .is_some()
    }

    /// pin 列表快照（abstract_id 升序 — 响应稳定，测试与管理面口径一致）
    pub fn list(&self) -> Vec<(String, String)> {
        let pins = self
            .pins
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries: Vec<(String, String)> = pins
            .iter()
            .map(|(abstract_id, group)| (abstract_id.clone(), group.clone()))
            .collect();
        entries.sort();
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ============ 组键解析 ============

    #[test]
    fn group_key_uses_declared_group_trimmed_or_singleton() {
        assert_eq!(
            EmbeddingPin::group_key("v1", "text-embedding-3-large", Some(" g1 ")),
            "g1",
            "声明的组名 trim 后生效"
        );
        assert_eq!(
            EmbeddingPin::group_key("v1", "text-embedding-3-large", None),
            "v1/::text-embedding-3-large",
            "未声明 = 单例组键（不与任何厂商互换）"
        );
        assert_eq!(
            EmbeddingPin::group_key("v1", "m", Some("  ")),
            "v1/::m",
            "空白组名与未声明同语义"
        );
    }

    // ============ pin_or_get 首写者胜 ============

    #[test]
    fn pin_or_get_converges_first_writing() {
        let pin = EmbeddingPin::new();
        assert_eq!(pin.pin_for("emb-a"), None, "未 pin 返回 None");

        let (winner, newly_set) = pin.pin_or_get("emb-a", "g1");
        assert!(newly_set, "未 pin 应写入");
        assert_eq!(winner, "g1");

        let (winner, newly_set) = pin.pin_or_get("emb-a", "g2");
        assert!(!newly_set, "已 pin 不得覆盖");
        assert_eq!(winner, "g1", "首写收敛：后到请求不得改写");
        assert_eq!(pin.pin_for("emb-a").as_deref(), Some("g1"));

        // abstract 维度隔离：另一 abstract 独立首写
        let (winner, newly_set) = pin.pin_or_get("emb-b", "g2");
        assert!(newly_set);
        assert_eq!(winner, "g2");
    }

    /// 并发首请求：32 个线程同时首写 → 恰好一个写入者胜出，最终 pin 即
    /// 胜出者写入的组键（写锁 entry 语义，无后写覆盖）
    #[test]
    fn pin_or_get_first_writer_wins_under_concurrency() {
        let pin = Arc::new(EmbeddingPin::new());
        let mut handles = Vec::new();
        for i in 0..32 {
            let pin = Arc::clone(&pin);
            handles.push(std::thread::spawn(move || {
                let group = format!("group-{i}");
                let (winner, newly_set) = pin.pin_or_get("emb-race", &group);
                (winner, newly_set)
            }));
        }
        let mut winners = Vec::new();
        for handle in handles {
            let (winner, newly_set) = handle.join().expect("并发任务不得 panic");
            if newly_set {
                winners.push(winner);
            }
        }
        assert_eq!(winners.len(), 1, "并发首写只允许一个写入者胜出");
        assert_eq!(
            pin.pin_for("emb-race").as_deref(),
            Some(winners[0].as_str()),
            "最终 pin 必须是胜出者写入的组键"
        );
    }

    // ============ 维度 record_and_check ============

    #[test]
    fn record_and_check_first_seen_passes_then_mismatch_errs_with_both_values() {
        let pin = EmbeddingPin::new();
        assert!(
            pin.record_and_check("v1", "m1", "-", 1024).is_ok(),
            "首见记录放行"
        );
        assert!(
            pin.record_and_check("v1", "m1", "-", 1024).is_ok(),
            "同维度重复放行"
        );
        let error = pin
            .record_and_check("v1", "m1", "-", 1536)
            .expect_err("维度漂移必须返回 Err");
        assert_eq!(error, (1024, 1536), "Err 含（旧维度, 新维度）");
        // 记录保持旧值为基准，持续暴露漂移
        assert_eq!(
            pin.record_and_check("v1", "m1", "-", 1536),
            Err((1024, 1536)),
            "不一致不更新记录，后续仍以旧值为基准"
        );
    }

    #[test]
    fn record_and_check_keys_are_independent() {
        let pin = EmbeddingPin::new();
        assert!(pin.record_and_check("v1", "m1", "-", 1024).is_ok());

        // 不同 vendor / model / dims 参数键各自首见，互不干扰
        assert!(
            pin.record_and_check("v2", "m1", "-", 1536).is_ok(),
            "不同 vendor 独立记录"
        );
        assert!(
            pin.record_and_check("v1", "m2", "-", 3072).is_ok(),
            "不同 model 独立记录"
        );
        assert!(
            pin.record_and_check("v1", "m1", "512", 512).is_ok(),
            "不同 dimensions 参数键独立记录"
        );
    }

    // ============ clear_all / clear_one / list ============

    #[test]
    fn clear_all_resets_pins_and_dimension_records() {
        let pin = EmbeddingPin::new();
        pin.pin_or_get("emb-a", "g1");
        pin.pin_or_get("emb-b", "g2");
        pin.record_and_check("v1", "m1", "-", 1024)
            .expect("首见放行");

        assert_eq!(pin.clear_all(), 2, "返回被清除的 pin 条数（重置计数用）");

        assert_eq!(pin.pin_for("emb-a"), None, "pin 全清");
        assert!(pin.list().is_empty(), "列表全清");
        assert!(
            pin.record_and_check("v1", "m1", "-", 1536).is_ok(),
            "维度记录一并清空：热载后重新观察，旧基准不残留"
        );
        assert_eq!(pin.clear_all(), 0, "已空再清返回 0（幂等）");
    }

    #[test]
    fn clear_one_removes_single_pin_and_is_idempotent() {
        let pin = EmbeddingPin::new();
        pin.pin_or_get("emb-a", "g1");
        pin.pin_or_get("emb-b", "g2");

        assert!(pin.clear_one("emb-a"), "存在并移除返回 true");
        assert_eq!(pin.pin_for("emb-a"), None);
        assert_eq!(
            pin.pin_for("emb-b").as_deref(),
            Some("g2"),
            "其余 pin 不受影响"
        );
        assert!(!pin.clear_one("emb-a"), "重复重置幂等返回 false");
        assert!(
            !pin.clear_one("emb-missing"),
            "不存在的 abstract 幂等返回 false（不报错）"
        );

        // 重置后同 abstract 可重新首写（换取新组键）
        let (winner, newly_set) = pin.pin_or_get("emb-a", "g3");
        assert!(newly_set);
        assert_eq!(winner, "g3");
    }

    #[test]
    fn list_returns_sorted_snapshot() {
        let pin = EmbeddingPin::new();
        pin.pin_or_get("emb-c", "gc");
        pin.pin_or_get("emb-a", "ga");
        pin.pin_or_get("emb-b", "gb");

        assert_eq!(
            pin.list(),
            vec![
                ("emb-a".to_string(), "ga".to_string()),
                ("emb-b".to_string(), "gb".to_string()),
                ("emb-c".to_string(), "gc".to_string()),
            ],
            "列表按 abstract_id 升序稳定输出"
        );
    }
}
