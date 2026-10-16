// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! vendor 内 key 池 — env 名池解析、轮换游标与按 key 指纹的
//! 熔断状态隔离。指纹为进程内随机密钥哈希（进程内稳定、跨进程不可复算），
//! env 缺失/截断/回退均显性告警，日志只报序号+值长度不回显配置原文。

use serde::Serialize;
use std::collections::HashSet;
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::config::VendorConfig;

static FINGERPRINT_KEYS: OnceLock<RandomState> = OnceLock::new();

/// 单 vendor key 池上限（截断显性 warn，禁止静默）。
pub const MAX_KEY_POOL: usize = 8;

/// 合并去重定序的 key 池 env 名（纯函数）：`api_key_envs` 显式列表在前，
/// `api_key_env`（向后兼容，可逗号分隔）兜底；trim、去空、保留首现位置。
pub(crate) fn merged_key_env_names(config: &VendorConfig) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut push_list = |raw: &str| {
        for part in raw.split(',') {
            let name = part.trim();
            if !name.is_empty() && seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        }
    };
    for name in &config.api_key_envs {
        push_list(name);
    }
    push_list(&config.api_key_env);
    names
}

/// 池解析结果；Debug 手写脱敏——`keys` 只出数量，env 名与截断计数可观测。
#[derive(Default, PartialEq)]
pub(crate) struct ResolvedPool {
    /// 实际 key 串（顺序 = env 名顺序；缺失/空值跳过）
    pub keys: Vec<String>,
    pub missing: Vec<String>,
    /// 超出 [`MAX_KEY_POOL`] 被截断的 env 名条数
    pub truncated: usize,
}

impl std::fmt::Debug for ResolvedPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // missing 只出计数——防误填的 key 原文经 Debug 回显
        f.debug_struct("ResolvedPool")
            .field("keys", &format!("[REDACTED {} keys]", self.keys.len()))
            .field("missing", &self.missing.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// 池解析核（env 经 `lookup` 注入，可测）：先截断到 [`MAX_KEY_POOL`]，缺失/空值跳过不中断后续。
pub(crate) fn resolve_key_pool(
    names: &[String],
    lookup: impl Fn(&str) -> Option<String>,
) -> ResolvedPool {
    let truncated = names.len().saturating_sub(MAX_KEY_POOL);
    let mut pool = ResolvedPool::default();
    for name in names.iter().take(MAX_KEY_POOL) {
        match lookup(name) {
            Some(value) if !value.trim().is_empty() => pool.keys.push(value),
            _ => pool.missing.push(name.clone()),
        }
    }
    pool.truncated = truncated;
    pool
}

/// key 指纹 — 16 位十六进制：进程内稳定（共享熔断计数前提）、跨进程不可复算；禁止外泄。
pub(crate) fn fingerprint_of(key: &str) -> String {
    let state = FINGERPRINT_KEYS.get_or_init(RandomState::new);
    format!("{:016x}", state.hash_one(key))
}

/// 请求级凭证 — 出站 key 选择结果；Debug 手写脱敏（`Pool` 臂不回显 key 原文，指纹保留可观测）。
#[derive(Clone, PartialEq)]
pub(crate) enum RequestCredential {
    /// 池命中：本次请求固定使用该 key（指纹参与熔断状态键）。
    Pool { key: String, fingerprint: String },
    /// 池空回退：`api_key_env` 单 key 行为（build 时读 env，缺失报 Config 错误）。
    LegacyEnv,
}

impl std::fmt::Debug for RequestCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestCredential::Pool { key, fingerprint } => f
                .debug_struct("Pool")
                .field("key", &format!("[REDACTED {} chars]", key.len()))
                .field("fingerprint", fingerprint)
                .finish(),
            RequestCredential::LegacyEnv => f.debug_struct("LegacyEnv").finish(),
        }
    }
}

impl RequestCredential {
    /// 池命中时的 key 原文；`LegacyEnv` 为 `None`（build 时读 env）。
    pub(crate) fn api_key(&self) -> Option<&str> {
        match self {
            RequestCredential::Pool { key, .. } => Some(key),
            RequestCredential::LegacyEnv => None,
        }
    }

    /// 池命中时的 key 指纹；`LegacyEnv` 为 `None`（用 vendor 级熔断器）。
    pub(crate) fn fingerprint(&self) -> Option<&str> {
        match self {
            RequestCredential::Pool { fingerprint, .. } => Some(fingerprint),
            RequestCredential::LegacyEnv => None,
        }
    }
}

/// key 池观测快照（admin 面）——只暴露 env 名 / 数量 / 下标 / 指纹，绝不暴露 key 原文。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyPoolInfo {
    /// 合并去重定序后的 env 名列表（env 名非机密，同 config 暴露面）
    pub envs: Vec<String>,
    /// 解析成功的 key 数量（0 = 池耗尽/全缺 env，已回退 legacy 单 key）
    pub size: usize,
    /// 当前游标下标（下一次出站请求将使用的 key；size=0 时恒 0）
    pub current_index: usize,
    /// 各 key 的 16 位十六进制指纹（下标与池顺序一致）
    pub fingerprints: Vec<String>,
}

/// 解析快照（env 模式专属；内联注入模式见 [`PoolSnapshot`]）。Debug 手写脱敏同 [`ResolvedPool`]。
#[derive(Default, PartialEq)]
struct ResolvedSnapshot {
    /// 最近一次解析的合并 env 名（变更检测键：名单变 → 全量重解析）
    names: Vec<String>,
    /// 解析后的 key 池（空 = 池耗尽/全缺 env）
    keys: Vec<String>,
    missing: usize,
    truncated: usize,
}

impl std::fmt::Debug for ResolvedSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // names/missing 只出计数（同 ResolvedPool 脱敏口径）。
        f.debug_struct("ResolvedSnapshot")
            .field("names", &self.names.len())
            .field("keys", &format!("[REDACTED {} keys]", self.keys.len()))
            .field("missing", &self.missing)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// 预解析 key 池快照（编程式注入面）：由调用方以 key 原文列表构造，注入后 `select`
/// 跳过 env 解析；空池等价 env 池耗尽，回退 legacy 单 key 行为。生命周期由调用方管理。
/// Debug 手写脱敏，`keys` 只出数量。
#[derive(Clone, PartialEq)]
pub struct PoolSnapshot {
    keys: Vec<String>,
}

impl std::fmt::Debug for PoolSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolSnapshot")
            .field("keys", &format!("[REDACTED {} keys]", self.keys.len()))
            .finish()
    }
}

impl PoolSnapshot {
    /// 由 key 原文列表构造（顺序即轮换序；空白剔除；上界 [`MAX_KEY_POOL`] 截断）。
    pub fn from_keys<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let keys = keys
            .into_iter()
            .map(|k| k.as_ref().trim().to_string())
            .filter(|k| !k.is_empty())
            .take(MAX_KEY_POOL)
            .collect();
        Self { keys }
    }

    /// 池内 key 数（0 = 空池 → legacy 回退）。
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// key 原文只读视图（crate 内观测面——admin info 组装用，绝不出库）。
    pub(crate) fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

pub(crate) struct KeyPool {
    snapshot: Mutex<ResolvedSnapshot>,
    /// 内联注入池（Some 时 select 全程跳过 env resolve，见 [`PoolSnapshot`]）。
    inline: Mutex<Option<PoolSnapshot>>,
    /// 轮换游标：401/403 认证类失败 `fetch_add(1)`，出站取 keys[cursor % len]。
    cursor: AtomicUsize,
    /// 黑名单：永久错误 key 指纹集，不参与轮换、不随时间恢复；仅热更裁剪或管理面重置可清。
    blacklist: Mutex<HashSet<String>>,
}

impl KeyPool {
    /// 实例构造期解析一次；后续每次 [`Self::select`] 按需刷新。
    pub(crate) fn new(config: &VendorConfig, vendor_id: &str) -> Self {
        let pool = Self {
            snapshot: Mutex::new(ResolvedSnapshot::default()),
            inline: Mutex::new(None),
            cursor: AtomicUsize::new(0),
            blacklist: Mutex::new(HashSet::new()),
        };
        pool.select(config, vendor_id);
        pool
    }

    /// 单测基座：空快照池（select_with 按注入 lookup 解析，不触真实 env）。
    #[cfg(test)]
    fn default_for_test() -> Self {
        Self {
            snapshot: Mutex::new(ResolvedSnapshot::default()),
            inline: Mutex::new(None),
            cursor: AtomicUsize::new(0),
            blacklist: Mutex::new(HashSet::new()),
        }
    }

    /// 注入内联池（见 [`PoolSnapshot`]）：此后 select 跳过 env resolve，重复注入以最后一次为准。
    pub(crate) fn set_inline(&self, snapshot: PoolSnapshot) {
        *self.inline.lock().unwrap() = Some(snapshot.clone());
        self.prune_blacklist(&snapshot.keys);
    }

    /// 撤除内联池（置 None）：后续 select 恢复 env 名单解析路径（legacy 兜底）。
    pub(crate) fn clear_inline(&self) {
        *self.inline.lock().unwrap() = None;
    }

    /// 请求级凭证选择——执行路径统一入口。env 模式名单变（热更新）→ 全量重解析并
    /// 显性告警；名单不变 → env 值现读，仅结果变化时落快照。池空回退 `LegacyEnv`。
    pub(crate) fn select(&self, config: &VendorConfig, vendor_id: &str) -> RequestCredential {
        self.select_with(config, vendor_id, &|name| std::env::var(name).ok())
    }

    /// [`Self::select`] 的可注入核（env 经 `lookup` 注入，可测；按引用传入以便用例内复用）。
    pub(crate) fn select_with(
        &self,
        config: &VendorConfig,
        vendor_id: &str,
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> RequestCredential {
        // 内联池快路径：跳过 env 名单解析与变更检测（见 [`PoolSnapshot`]）
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            // 黑名单跳过（游标语义不变）；全黑 ≡ 池耗尽 → legacy 回退。
            return self.select_from_keys(inline.keys.clone());
        }

        let names = merged_key_env_names(config);
        // 快照刷新：名单变化全量重解析；名单不变则 env 值现读、变化才落快照。
        // 黑名单裁剪仅在集合变化分支执行（集合未变时为恒等空操作，避免每请求空转）。
        let keys = {
            let mut guard = self.snapshot.lock().unwrap();
            if guard.names != names {
                let resolved = resolve_key_pool(&names, |name| lookup(name));
                Self::log_resolution(vendor_id, &names, &resolved);
                *guard = ResolvedSnapshot {
                    names,
                    keys: resolved.keys,
                    missing: resolved.missing.len(),
                    truncated: resolved.truncated,
                };
                self.prune_blacklist(&guard.keys);
                guard.keys.clone()
            } else {
                // 名单未变：env 值现读，仅结果变化时落快照（热更新立即生效）；收缩与全缺
                // 均显性告警，明细只报数量不回显 env 名。
                let resolved = resolve_key_pool(&names, |name| lookup(name));
                if resolved.keys != guard.keys {
                    if resolved.keys.len() < guard.keys.len() {
                        tracing::warn!(
                            vendor = %vendor_id,
                            before = guard.keys.len(),
                            after = resolved.keys.len(),
                            missing = resolved.missing.len(),
                            "{}",
                            crate::i18n::t_args(
                                "vendor-key-pool-shrunk",
                                &[
                                    ("vendor", vendor_id.to_string()),
                                    ("before", guard.keys.len().to_string()),
                                    ("after", resolved.keys.len().to_string()),
                                    ("missing", resolved.missing.len().to_string()),
                                ],
                            )
                        );
                    }
                    if resolved.keys.is_empty() {
                        tracing::warn!(
                            vendor = %vendor_id,
                            "{}",
                            crate::i18n::t_args(
                                "vendor-key-pool-fallback-legacy",
                                &[("vendor", vendor_id.to_string())],
                            )
                        );
                    }
                    guard.keys = resolved.keys;
                    guard.missing = resolved.missing.len();
                    self.prune_blacklist(&guard.keys);
                }
                guard.keys.clone()
            }
        };
        self.select_from_keys(keys)
    }

    /// 黑名单裁剪：仅保留仍在当前 key 集合中的指纹（key 集合变化即重置对应状态）。
    /// 仅在 [`Self::select_with`] 的集合变化分支与 [`Self::set_inline`] 注入点执行。
    fn prune_blacklist(&self, keys: &[String]) {
        let live: HashSet<String> = keys.iter().map(|k| fingerprint_of(k)).collect();
        self.blacklist
            .lock()
            .unwrap()
            .retain(|fp| live.contains(fp));
    }

    /// 由 key 集合选出站凭证：游标位起按序扫描首个非黑名单 key（仅跳过，不重排轮换）；空/全黑 → legacy 回退。
    fn select_from_keys(&self, keys: Vec<String>) -> RequestCredential {
        if keys.is_empty() {
            return RequestCredential::LegacyEnv;
        }
        let blacklist = self.blacklist.lock().unwrap();
        let start = self.cursor.load(Ordering::Relaxed) % keys.len();
        for offset in 0..keys.len() {
            let key = &keys[(start + offset) % keys.len()];
            if !blacklist.contains(&fingerprint_of(key)) {
                return RequestCredential::Pool {
                    key: key.clone(),
                    fingerprint: fingerprint_of(key),
                };
            }
        }
        RequestCredential::LegacyEnv
    }

    /// 永久错误 key 进黑名单：不参与轮换、不随时间恢复。
    #[cfg(any(feature = "resilience", test))]
    pub(crate) fn mark_blacklisted(&self, fingerprint: &str) {
        self.blacklist
            .lock()
            .unwrap()
            .insert(fingerprint.to_string());
    }

    /// 池内全部 key 是否均已入黑名单（explain 判定面）。池空（legacy 回退形态）返回
    /// false——回退凭证不受黑名单约束，路由仍可出站。
    pub(crate) fn all_blacklisted(&self) -> bool {
        let keys = self.keys_snapshot();
        if keys.is_empty() {
            return false;
        }
        let blacklist = self.blacklist.lock().unwrap();
        !blacklist.is_empty() && keys.iter().all(|k| blacklist.contains(&fingerprint_of(k)))
    }

    /// 黑名单指纹快照（探针候选面；顺序不定）。
    pub(crate) fn blacklisted_fingerprints(&self) -> Vec<String> {
        self.blacklist.lock().unwrap().iter().cloned().collect()
    }

    /// 解黑名单单个指纹（探针恢复通道；不存在为 no-op）。
    #[cfg(feature = "resilience")]
    pub(crate) fn remove_blacklisted(&self, fingerprint: &str) {
        self.blacklist.lock().unwrap().remove(fingerprint);
    }

    /// key 明文快照（探针 HTTP 构造专用；在 crate 内即用即弃，绝不记日志）。
    pub(crate) fn keys_snapshot(&self) -> Vec<String> {
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            return inline.keys.clone();
        }
        self.snapshot.lock().unwrap().keys.clone()
    }

    /// 当前轮换游标所指 key（free 探测的出站凭证；None = 池空/legacy）。
    pub(crate) fn current_key(&self) -> Option<(String, String)> {
        let keys = self.keys_snapshot();
        if keys.is_empty() {
            return None;
        }
        let index = self.cursor.load(Ordering::Relaxed) % keys.len();
        let key = &keys[index];
        Some((key.clone(), fingerprint_of(key)))
    }

    /// 管理面/恢复通道重置：清空黑名单并回绕游标。
    pub(crate) fn reset_key_state(&self) {
        self.blacklist.lock().unwrap().clear();
        self.cursor.store(0, Ordering::Relaxed);
    }

    /// 黑名单内 key 数（单测断言用）。
    #[cfg(test)]
    pub(crate) fn blacklisted_len(&self) -> usize {
        self.blacklist.lock().unwrap().len()
    }

    /// 解析显性化（仅名单变更时调用一次，禁止每请求刷屏）：逐缺失条目 warn、截断 warn、
    /// 全缺回退 warn。脱敏：缺失条目不回显配置原文，只报序号 + 值长度。
    fn log_resolution(vendor_id: &str, names: &[String], resolved: &ResolvedPool) {
        for (index, env) in resolved.missing.iter().enumerate() {
            tracing::warn!(
                vendor = %vendor_id,
                index,
                length = env.len(),
                "{}",
                crate::i18n::t_args(
                    "vendor-key-pool-env-missing",
                    &[
                        ("vendor", vendor_id.to_string()),
                        ("index", index.to_string()),
                        ("length", env.len().to_string()),
                    ],
                )
            );
        }
        if resolved.truncated > 0 {
            tracing::warn!(
                vendor = %vendor_id,
                cap = MAX_KEY_POOL,
                dropped = resolved.truncated,
                "{}",
                crate::i18n::t_args(
                    "vendor-key-pool-truncated",
                    &[
                        ("vendor", vendor_id.to_string()),
                        ("cap", MAX_KEY_POOL.to_string()),
                        ("dropped", resolved.truncated.to_string()),
                    ],
                )
            );
        }
        if resolved.keys.is_empty() && !names.is_empty() {
            tracing::warn!(
                vendor = %vendor_id,
                "{}",
                crate::i18n::t_args(
                    "vendor-key-pool-fallback-legacy",
                    &[("vendor", vendor_id.to_string())],
                )
            );
        }
    }

    /// 路由热路径用：只读当前指纹（不触 env、不写快照、不产生日志）；池空返回 `None`。
    #[cfg_attr(not(feature = "resilience"), allow(dead_code))] // 仅 resilience 组合的熔断聚合/选路读取
    pub(crate) fn peek_current_fingerprint(&self) -> Option<String> {
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            return (!inline.keys.is_empty()).then(|| {
                let index = self.cursor.load(Ordering::Relaxed) % inline.keys.len();
                fingerprint_of(&inline.keys[index])
            });
        }
        let guard = self.snapshot.lock().unwrap();
        let keys = &guard.keys;
        if keys.is_empty() {
            return None;
        }
        let index = self.cursor.load(Ordering::Relaxed) % keys.len();
        Some(fingerprint_of(&keys[index]))
    }

    /// 401/403 认证类失败推进游标——轮换生效于**下次尝试**，本请求剩余尝试仍用同一 key。
    /// 池未激活（keys 空）时 no-op；多 key 池时显性 warn。
    pub(crate) fn advance(&self, vendor_id: &str, status: u16) -> bool {
        let len = self.active_pool_len();
        if len == 0 {
            return false;
        }
        let next = self.cursor.fetch_add(1, Ordering::Relaxed) + 1;
        if len > 1 {
            tracing::warn!(
                vendor = %vendor_id,
                status = status,
                next_index = next % len,
                pool_size = len,
                "{}",
                crate::i18n::t_args(
                    "vendor-key-rotated",
                    &[
                        ("vendor", vendor_id.to_string()),
                        ("status", status.to_string()),
                    ],
                )
            );
        }
        true
    }

    /// 请求错误记账（三态）：永久错误（401/402/403 或账务类正文）→ 黑名单 + 游标推进；
    /// 瞬时错误（429/超时/5xx/网络）→ 无 key 动作（冷却/退避走既有路径）。resilience
    /// 关闭时退化为 legacy 语义（仅 401/403 推进、无黑名单）。
    pub(crate) fn note_request_error(
        &self,
        vendor_id: &str,
        fingerprint: Option<&str>,
        status: u16,
        message: &str,
    ) {
        #[cfg(feature = "resilience")]
        {
            use crate::vendor::resilience::error_map::{classify_key_error, KeyErrorClass};
            if classify_key_error(status, message) == KeyErrorClass::Permanent {
                if let Some(fp) = fingerprint {
                    self.mark_blacklisted(fp);
                    tracing::warn!(
                        vendor = %vendor_id,
                        fingerprint = %fp,
                        "{}",
                        crate::i18n::t("vendor-key-blacklisted")
                    );
                }
                self.advance(vendor_id, status);
            }
        }
        #[cfg(not(feature = "resilience"))]
        {
            let _ = (fingerprint, message);
            if matches!(status, 401 | 403) {
                self.advance(vendor_id, status);
            }
        }
    }

    /// 激活池长度（内联池优先，env 快照兜底）；游标取模与轮换判定的单一事实源。
    fn active_pool_len(&self) -> usize {
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            return inline.keys.len();
        }
        self.snapshot.lock().unwrap().keys.len()
    }

    /// admin 观测快照（仅 env 名 / 数量 / 下标 / 指纹，绝无 key 原文）；内联池 `envs` 为空列表。
    pub(crate) fn info(&self) -> KeyPoolInfo {
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            let len = inline.keys.len();
            return KeyPoolInfo {
                envs: Vec::new(),
                size: len,
                current_index: if len == 0 {
                    0
                } else {
                    self.cursor.load(Ordering::Relaxed) % len
                },
                fingerprints: inline.keys.iter().map(|k| fingerprint_of(k)).collect(),
            };
        }
        let guard = self.snapshot.lock().unwrap();
        let len = guard.keys.len();
        KeyPoolInfo {
            envs: guard.names.clone(),
            size: len,
            current_index: if len == 0 {
                0
            } else {
                self.cursor.load(Ordering::Relaxed) % len
            },
            fingerprints: guard.keys.iter().map(|k| fingerprint_of(k)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! key 池单元测试：env 经 lookup 注入，无真实环境变量竞态；同一用例内 lookup
    //! 恒定，对齐生产「env 值每次请求现读」语义。

    use super::*;

    pub(super) fn vendor_config(api_key_env: &str, api_key_envs: &[&str]) -> VendorConfig {
        VendorConfig {
            id: "v-test".into(),
            name: "Test".into(),
            type_: "openai".into(),
            api_key_env: api_key_env.into(),
            api_key_envs: api_key_envs.iter().map(|s| s.to_string()).collect(),
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: crate::config::RateLimitConfig::Token {
                max_tokens: 1000,
                window_hours: None,
                sliding: false,
            },
            concurrency: crate::config::ConcurrencyConfig::default(),
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            supports_batch: false,
            chat_path: None,
        }
    }

    pub(super) fn lookup_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    // ======== 合并去重定序（显式列表在前） ========

    #[test]
    fn merged_names_explicit_list_first_then_legacy_dedup() {
        let config = vendor_config("ENV_A,ENV_C", &["ENV_B", "ENV_A"]);
        assert_eq!(
            merged_key_env_names(&config),
            vec!["ENV_B", "ENV_A", "ENV_C"],
            "显式列表在前、legacy 兜底在后、跨来源去重保留首现位置"
        );
    }

    #[test]
    fn merged_names_comma_split_and_blank_filtered() {
        let config = vendor_config(" A , , B ,, ", &[]);
        assert_eq!(
            merged_key_env_names(&config),
            vec!["A", "B"],
            "逗号分隔展开、空白条目剔除、条目 trim"
        );
    }

    #[test]
    fn merged_names_legacy_only_is_backward_compatible() {
        let config = vendor_config("LEGACY_ONLY", &[]);
        assert_eq!(merged_key_env_names(&config), vec!["LEGACY_ONLY"]);
    }

    // ======== 池解析（缺失跳过 + 计数、上限截断） ========

    #[test]
    fn resolve_skips_missing_envs_and_counts_them() {
        let names: Vec<String> = ["SET", "MISSING", "EMPTY_VAL"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let resolved =
            resolve_key_pool(&names, lookup_map(&[("SET", "key-1"), ("EMPTY_VAL", "  ")]));
        assert_eq!(resolved.keys, vec!["key-1"], "命中名按序产出 key");
        assert_eq!(
            resolved.missing,
            vec!["MISSING", "EMPTY_VAL"],
            "未设置与空值 env 均计缺失（显性告警输入）"
        );
        assert_eq!(resolved.truncated, 0);
    }

    #[test]
    fn resolve_truncates_at_cap_and_reports_dropped_count() {
        let names: Vec<String> = (0..10).map(|i| format!("K{i}")).collect();
        let pairs: Vec<(String, String)> = (0..10).map(|i| (format!("K{i}"), "v".into())).collect();
        let map: std::collections::HashMap<String, String> = pairs.into_iter().collect();
        let resolved = resolve_key_pool(&names, move |name| map.get(name).cloned());
        assert_eq!(resolved.keys.len(), MAX_KEY_POOL);
        assert_eq!(resolved.truncated, 2, "10 名超上限 8，截断 2 条并显性化");
    }

    // ======== 指纹稳定性与区分度 ========

    /// 指纹哈希用进程内随机密钥：同 key 恒同指纹、不同 key 互异（共享计数与隔离前提）；
    /// 跨进程随机性无法在单进程内断言。
    #[test]
    fn fingerprint_is_stable_hex16_and_distinct_per_key() {
        let a1 = fingerprint_of("alpha");
        let a2 = fingerprint_of("alpha");
        let b = fingerprint_of("beta");
        assert_eq!(a1, a2, "同 key 恒同指纹（跨请求共享计数的前提）");
        assert_ne!(a1, b, "不同 key 指纹互异（隔离的前提）");
        assert_eq!(a1.len(), 16, "16 位十六进制（对齐 provider_chain 语义）");
        assert!(a1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ======== Debug 脱敏（纵深防御） ========

    #[test]
    fn request_credential_debug_redacts_key_plaintext() {
        // 测试假凭证（detect-secrets 已人工核阅误报豁免）
        let secret = "super-secret-credential-12345"; // pragma: allowlist secret
        let credential = RequestCredential::Pool {
            key: secret.to_string(),
            fingerprint: "0123456789abcdef".to_string(),
        };
        let dbg = format!("{credential:?}");
        assert!(!dbg.contains(secret), "Debug 不得泄漏 key 原文: {dbg}");
        assert!(
            dbg.contains("[REDACTED 29 chars]"),
            "脱敏形态须为 [REDACTED n chars]: {dbg}"
        );
        assert!(
            dbg.contains("0123456789abcdef"),
            "指纹非机密（进程内随机哈希），Debug 保留可观测: {dbg}"
        );
        assert_eq!(
            format!("{:?}", RequestCredential::LegacyEnv),
            "LegacyEnv",
            "LegacyEnv 臂无敏感字段"
        );
    }

    /// PoolSnapshot / ResolvedPool / ResolvedSnapshot 手工 Debug：keys 与 env 名只出
    /// 计数，防 key 原文经 Debug 回显。
    #[test]
    fn pool_structs_debug_redact_key_plaintext() {
        // 测试假凭证（detect-secrets 已人工核阅误报豁免）
        let secret = "inline-credential-value"; // pragma: allowlist secret
        let snapshot = PoolSnapshot::from_keys([secret, "k2"]);
        let dbg = format!("{snapshot:?}");
        assert!(
            !dbg.contains(secret),
            "PoolSnapshot Debug 不得泄漏 key 原文: {dbg}"
        );
        assert!(dbg.contains("[REDACTED 2 keys]"), "脱敏形态: {dbg}");

        let resolved = resolve_key_pool(
            &["ENV_SET".to_string(), "ENV_MISSING".to_string()],
            |name| (name == "ENV_SET").then(|| secret.to_string()),
        );
        let dbg = format!("{resolved:?}");
        assert!(
            !dbg.contains(secret),
            "ResolvedPool Debug 不得泄漏 key 原文: {dbg}"
        );
        assert!(dbg.contains("[REDACTED 1 keys]"), "脱敏形态: {dbg}");
        assert!(
            dbg.contains("missing: 1"),
            "env 名改计数口径（防误填原文回显）: {dbg}"
        );

        let snapshot_state = ResolvedSnapshot {
            names: vec!["ENV_X".to_string()],
            keys: vec![secret.to_string()],
            missing: 0,
            truncated: 0,
        };
        let dbg = format!("{snapshot_state:?}");
        assert!(
            !dbg.contains(secret),
            "ResolvedSnapshot Debug 不得泄漏 key 原文: {dbg}"
        );
        assert!(dbg.contains("[REDACTED 1 keys]"), "脱敏形态: {dbg}");
        assert!(dbg.contains("names: 1"), "env 名改计数口径: {dbg}");
    }

    // ======== 轮换顺序与耗尽回退 ========

    #[test]
    fn select_returns_pool_keys_in_order_then_wraps_after_advance() {
        let config = vendor_config("ENV_X", &["ENV_P", "ENV_Q"]);
        let lookup = lookup_map(&[
            ("ENV_P", "key-first"),
            ("ENV_Q", "key-second"),
            ("ENV_X", "key-third"),
        ]);
        let pool = KeyPool::default_for_test();

        let c0 = pool.select_with(&config, "v", &lookup);
        assert_eq!(c0.api_key(), Some("key-first"), "游标 0：显式列表首名先出");

        assert!(pool.advance("v", 401));
        let c1 = pool.select_with(&config, "v", &lookup);
        assert_eq!(c1.api_key(), Some("key-second"), "401 后游标推进到第 2 名");

        assert!(pool.advance("v", 403));
        let c2 = pool.select_with(&config, "v", &lookup);
        assert_eq!(
            c2.api_key(),
            Some("key-third"),
            "再次认证失败推进到 legacy 兜底名"
        );

        assert!(pool.advance("v", 401));
        let c3 = pool.select_with(&config, "v", &lookup);
        assert_eq!(
            c3.api_key(),
            Some("key-first"),
            "池耗尽（游标越过末位）回绕到首位，恒有 key 可用"
        );
    }

    #[test]
    fn select_falls_back_to_legacy_when_all_envs_missing() {
        let config = vendor_config("MISSING_LEGACY", &["MISSING_A"]);
        let pool = KeyPool::default_for_test();
        let credential = pool.select_with(&config, "v", &|_| None);
        assert_eq!(
            credential,
            RequestCredential::LegacyEnv,
            "池耗尽/全缺 env 回退既有单 key 行为"
        );
        assert!(!pool.advance("v", 401), "池未激活时推进是 no-op");
    }

    #[test]
    fn select_refreshes_when_env_value_changes_without_name_change() {
        let config = vendor_config("LIVE_ENV", &[]);
        let pool = KeyPool::default_for_test();
        let first = pool.select_with(&config, "v", &lookup_map(&[("LIVE_ENV", "old")]));
        assert_eq!(first.api_key(), Some("old"));
        let second = pool.select_with(&config, "v", &lookup_map(&[("LIVE_ENV", "new")]));
        assert_eq!(
            second.api_key(),
            Some("new"),
            "env 值变化立即生效（与既有 per-attempt 读 env 语义一致）"
        );
    }

    /// 请求期 env 收缩：部分消失收缩池（游标取模钳制），全部消失回退 legacy 且游标推进 no-op。
    #[test]
    fn select_reflects_env_shrink_and_full_vanish_fallback() {
        let config = vendor_config("ENV_X", &["ENV_P", "ENV_Q"]);
        let pool = KeyPool::default_for_test();
        let full = lookup_map(&[("ENV_P", "pk"), ("ENV_Q", "qk"), ("ENV_X", "xk")]);
        let only_q = lookup_map(&[("ENV_Q", "qk")]);
        let none = lookup_map(&[]);

        assert_eq!(pool.select_with(&config, "v", &full).api_key(), Some("pk"));
        // 部分收缩：2 把 → 1 把，游标 0 仍指向现存的唯一 key
        assert!(pool.advance("v", 401));
        assert_eq!(
            pool.select_with(&config, "v", &only_q).api_key(),
            Some("qk"),
            "收缩后游标取模钳制到现存 key"
        );
        assert_eq!(
            pool.select_with(&config, "v", &none),
            RequestCredential::LegacyEnv
        );
        assert!(!pool.advance("v", 401), "池耗尽后推进是 no-op");
    }

    #[test]
    fn peek_current_fingerprint_matches_select_without_env_access() {
        let config = vendor_config("ENV_X", &["ENV_P"]);
        let pool = KeyPool::default_for_test();
        let lookup = lookup_map(&[("ENV_P", "pk"), ("ENV_X", "lk")]);
        let credential = pool.select_with(&config, "v", &lookup);
        let fp = credential.fingerprint().unwrap();
        assert_eq!(
            pool.peek_current_fingerprint().as_deref(),
            Some(fp),
            "路由热路径 peek 与请求级 select 同源（快照 + 游标）"
        );
    }

    #[test]
    fn info_exposes_only_envs_size_index_fingerprints() {
        let config = vendor_config("ENV_X", &["ENV_P", "ENV_Q"]);
        let pool = KeyPool::default_for_test();
        let lookup = lookup_map(&[("ENV_P", "pk"), ("ENV_Q", "qk"), ("ENV_X", "xk")]);
        pool.select_with(&config, "v", &lookup);
        pool.advance("v", 401);
        let info = pool.info();
        assert_eq!(info.envs, vec!["ENV_P", "ENV_Q", "ENV_X"]);
        assert_eq!(info.size, 3);
        assert_eq!(info.current_index, 1);
        assert_eq!(info.fingerprints.len(), 3);
        let serialized = serde_json::to_string(&info).unwrap();
        assert!(
            !serialized.contains("\"pk\"")
                && !serialized.contains("\"qk\"")
                && !serialized.contains("\"xk\""),
            "admin 观测快照绝不含 key 原文（掩码约定）: {serialized}"
        );
    }

    #[test]
    fn info_size_zero_when_pool_exhausted() {
        let config = vendor_config("MISSING_LEGACY", &[]);
        let pool = KeyPool::default_for_test();
        pool.select_with(&config, "v", &|_| None);
        let info = pool.info();
        assert_eq!(info.size, 0);
        assert_eq!(info.current_index, 0);
        assert!(info.fingerprints.is_empty());
    }
}

#[cfg(test)]
mod inline_tests {
    //! 内联池注入面：指纹/游标与 env 池一致，env resolve 整体跳过，生命周期由调用方管理。

    use super::*;

    fn inline_vendor_config(api_key_env: &str) -> VendorConfig {
        VendorConfig {
            id: "gate-0".into(),
            name: "Inline".into(),
            type_: "openai".into(),
            api_key_env: api_key_env.into(),
            api_key_envs: vec![],
            extra_headers: Default::default(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: crate::config::RateLimitConfig::Token {
                max_tokens: 1000,
                window_hours: None,
                sliding: false,
            },
            concurrency: crate::config::ConcurrencyConfig::default(),
            timeout_ms: 30_000,
            stream_ttft_ms: 30_000,
            stream_idle_ms: 30_000,
            supports_batch: false,
            chat_path: None,
        }
    }

    #[test]
    fn from_keys_filters_blank_and_truncates() {
        let pool = PoolSnapshot::from_keys([" k1 ", "", "  ", "k2"]);
        assert_eq!(pool.len(), 2, "空白项剔除");
        let many: Vec<String> = (0..(MAX_KEY_POOL + 3)).map(|i| format!("k{i}")).collect();
        assert_eq!(
            PoolSnapshot::from_keys(many).len(),
            MAX_KEY_POOL,
            "上界截断"
        );
        assert!(PoolSnapshot::from_keys(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn inline_select_skips_env_resolve_and_rotates() {
        let pool = KeyPool::default_for_test();
        pool.set_inline(PoolSnapshot::from_keys(["k-bad", "k-good"]));
        let config = inline_vendor_config("OPENAI_API_KEY");
        // lookup 若被咨询会返回该值——内联池必须无视 env
        let credential = pool.select_with(&config, "gate-0", &|name| {
            name.ends_with("API_KEY").then(|| "FROM-ENV".to_string())
        });
        match credential {
            RequestCredential::Pool { key, fingerprint } => {
                assert_eq!(key, "k-bad", "首发取游标 0");
                assert_eq!(fingerprint, fingerprint_of("k-bad"), "指纹与 env 池同源");
            }
            other => panic!("内联池必须命中 Pool: {other:?}"),
        }
        // 401 推进游标 → 下一把
        assert!(pool.advance("gate-0", 401));
        match pool.select_with(&config, "gate-0", &|_| None) {
            RequestCredential::Pool { key, .. } => assert_eq!(key, "k-good", "游标轮换语义一致"),
            other => panic!("{other:?}"),
        }
        // admin 快照：envs 空（内联池无 env 名），指纹反映真实池
        let info = pool.info();
        assert!(info.envs.is_empty());
        assert_eq!(info.size, 2);
        assert_eq!(info.fingerprints.len(), 2);
    }

    #[test]
    fn inline_empty_pool_falls_back_to_legacy() {
        let pool = KeyPool::default_for_test();
        pool.set_inline(PoolSnapshot::from_keys(Vec::<String>::new()));
        let config = inline_vendor_config("OPENAI_API_KEY");
        let credential = pool.select_with(&config, "gate-0", &|_| None);
        assert!(
            matches!(credential, RequestCredential::LegacyEnv),
            "空内联池等价 env 池耗尽 → legacy 回退"
        );
    }

    // ======== 同名单热更新（名单不变现读 env，值变化才落快照） ========

    use super::tests::{lookup_map, vendor_config};

    #[test]
    fn select_same_names_rereads_env_and_selects_new_values() {
        let pool = KeyPool::default_for_test();
        let config = vendor_config("ENV_A,ENV_B", &[]);
        let first = pool.select_with(
            &config,
            "gate-0",
            &lookup_map(&[("ENV_A", "k-a1"), ("ENV_B", "k-b1")]),
        );
        assert!(
            matches!(&first, RequestCredential::Pool { key, .. } if key == "k-a1"),
            "首发取游标 0"
        );
        let second = pool.select_with(
            &config,
            "gate-0",
            &lookup_map(&[("ENV_A", "k-a2"), ("ENV_B", "k-b2")]),
        );
        assert!(
            matches!(&second, RequestCredential::Pool { key, .. } if key == "k-a2"),
            "同名单换值必须选中新值（热更新立即生效）"
        );
    }

    #[test]
    fn select_same_names_shrunk_pool_keeps_surviving_key() {
        let pool = KeyPool::default_for_test();
        let config = vendor_config("ENV_A,ENV_B,ENV_C", &[]);
        let full = pool.select_with(
            &config,
            "gate-0",
            &lookup_map(&[("ENV_A", "k-a"), ("ENV_B", "k-b"), ("ENV_C", "k-c")]),
        );
        assert!(matches!(&full, RequestCredential::Pool { key, .. } if key == "k-a"));
        let shrunk = pool.select_with(&config, "gate-0", &lookup_map(&[("ENV_A", "k-a")]));
        assert!(
            matches!(&shrunk, RequestCredential::Pool { key, .. } if key == "k-a"),
            "收缩后只剩存留 key 可选"
        );
        assert_eq!(pool.info().size, 1, "快照收缩为存留 key 数");
    }

    #[test]
    fn select_same_names_all_missing_falls_back_to_legacy() {
        let pool = KeyPool::default_for_test();
        let config = vendor_config("ENV_A,ENV_B", &[]);
        let initial = pool.select_with(
            &config,
            "gate-0",
            &lookup_map(&[("ENV_A", "k-a"), ("ENV_B", "k-b")]),
        );
        assert!(matches!(initial, RequestCredential::Pool { .. }));
        let drained = pool.select_with(&config, "gate-0", &lookup_map(&[]));
        assert!(
            matches!(drained, RequestCredential::LegacyEnv),
            "同名单全缺回退 legacy 凭证"
        );
    }

    // ======== 首次解析的告警输入（缺失 / 截断 / 全缺） ========

    #[test]
    fn select_first_resolution_with_missing_and_over_cap_names_records_snapshot() {
        let pool = KeyPool::default_for_test();
        // 10 名单：K1..K9（显式列表在前）+ K0（legacy 兜底）；K9 缺失，9 命中超上限 8
        let config = vendor_config(
            "K0",
            &["K1", "K2", "K3", "K4", "K5", "K6", "K7", "K8", "K9"],
        );
        let lookup = lookup_map(&[
            ("K0", "k0"),
            ("K1", "k1"),
            ("K2", "k2"),
            ("K3", "k3"),
            ("K4", "k4"),
            ("K5", "k5"),
            ("K6", "k6"),
            ("K7", "k7"),
            ("K8", "k8"),
        ]);
        let credential = pool.select_with(&config, "gate-0", &lookup);
        assert!(matches!(credential, RequestCredential::Pool { .. }));
        assert_eq!(
            pool.keys_snapshot().len(),
            MAX_KEY_POOL,
            "缺失显性化后按上限截断，快照如实记录"
        );
    }

    #[test]
    fn select_first_resolution_all_missing_falls_back_to_legacy() {
        let pool = KeyPool::default_for_test();
        let config = vendor_config("MISSING_A,MISSING_B", &[]);
        let credential = pool.select_with(&config, "gate-0", &lookup_map(&[]));
        assert!(
            matches!(credential, RequestCredential::LegacyEnv),
            "首次解析全缺 → legacy 回退"
        );
    }

    // ======== 探针/观测辅助面 ========

    #[test]
    fn pool_snapshot_keys_observer_lists_key_texts() {
        let snapshot = PoolSnapshot::from_keys(["k-1", "k-2"]);
        assert_eq!(snapshot.keys(), &["k-1".to_string(), "k-2".to_string()]);
    }

    #[test]
    fn all_blacklisted_tri_state_on_inline_pool() {
        let pool = KeyPool::default_for_test();
        assert!(!pool.all_blacklisted(), "空池（legacy 形态）恒 false");
        pool.set_inline(PoolSnapshot::from_keys(["k-a", "k-b"]));
        pool.mark_blacklisted(&fingerprint_of("k-a"));
        assert!(!pool.all_blacklisted(), "半黑不算全黑");
        pool.mark_blacklisted(&fingerprint_of("k-b"));
        assert!(pool.all_blacklisted(), "全黑 → true");
    }

    #[test]
    fn keys_snapshot_prefers_inline_pool_over_env_snapshot() {
        let pool = KeyPool::default_for_test();
        let config = vendor_config("ENV_A", &[]);
        pool.select_with(&config, "gate-0", &lookup_map(&[("ENV_A", "k-env")]));
        assert_eq!(pool.keys_snapshot(), vec!["k-env".to_string()]);
        pool.set_inline(PoolSnapshot::from_keys(["k-inline"]));
        assert_eq!(
            pool.keys_snapshot(),
            vec!["k-inline".to_string()],
            "内联池优先于 env 快照"
        );
    }

    #[test]
    fn current_key_empty_pool_none_and_inline_returns_fingerprinted_key() {
        let pool = KeyPool::default_for_test();
        assert!(pool.current_key().is_none(), "空池无当前 key");
        pool.set_inline(PoolSnapshot::from_keys(["k-only"]));
        let (key, fingerprint) = pool.current_key().expect("非空池必有当前 key");
        assert_eq!(key, "k-only");
        assert_eq!(fingerprint, fingerprint_of("k-only"), "指纹与明文同源");
    }

    #[test]
    fn peek_current_fingerprint_inline_branch_follows_cursor() {
        let pool = KeyPool::default_for_test();
        pool.set_inline(PoolSnapshot::from_keys(["k-a", "k-b"]));
        assert_eq!(
            pool.peek_current_fingerprint(),
            Some(fingerprint_of("k-a")),
            "内联池游标 0 起读"
        );
        assert!(pool.advance("gate-0", 429), "多 key 池推进返回 true");
        assert_eq!(
            pool.peek_current_fingerprint(),
            Some(fingerprint_of("k-b")),
            "推进后游标指向下一 key"
        );
    }

    #[test]
    fn note_request_error_permanent_blacklists_and_transient_does_not() {
        let pool = KeyPool::default_for_test();
        pool.set_inline(PoolSnapshot::from_keys(["k-a", "k-b"]));
        let fp_a = fingerprint_of("k-a");
        pool.note_request_error("gate-0", Some(&fp_a), 401, "invalid api key");
        assert!(
            pool.blacklisted_fingerprints().contains(&fp_a),
            "401 永久错误必须入黑名单"
        );
        let fp_b = fingerprint_of("k-b");
        pool.note_request_error("gate-0", Some(&fp_b), 429, "rate limited");
        assert!(
            !pool.blacklisted_fingerprints().contains(&fp_b),
            "429 瞬时错误不得入黑名单"
        );
        // 全黑内联池 → select 回退 legacy；撤黑后 k-b 恢复可选
        pool.mark_blacklisted(&fp_b);
        assert!(matches!(
            pool.select_with(&inline_vendor_config("X"), "gate-0", &|_| None),
            RequestCredential::LegacyEnv
        ));
    }
}

#[cfg(test)]
mod blacklist_tests {
    //! 黑名单三态单测：跳过 / 全黑回退 / 热更裁剪 / 重置恢复。

    use super::tests::{lookup_map, vendor_config};
    use super::*;

    fn pool_config() -> VendorConfig {
        vendor_config("LEGACY_UNUSED", &["ENV_A", "ENV_B"])
    }

    fn lookup() -> impl Fn(&str) -> Option<String> {
        lookup_map(&[
            ("ENV_A", "k-a"),
            ("ENV_B", "k-b"),
            ("LEGACY_UNUSED", "k-legacy"),
        ])
    }

    fn selected_key(pool: &KeyPool, config: &VendorConfig) -> Option<String> {
        match pool.select_with(config, "v-test", &lookup()) {
            RequestCredential::Pool { key, .. } => Some(key),
            RequestCredential::LegacyEnv => None,
        }
    }

    #[test]
    fn blacklisted_key_is_skipped_others_continue() {
        let pool = KeyPool::default_for_test();
        let config = pool_config();
        assert_eq!(selected_key(&pool, &config).as_deref(), Some("k-a"));
        pool.mark_blacklisted(&fingerprint_of("k-a"));
        assert_eq!(pool.blacklisted_len(), 1);
        // 游标仍指 0，但 k-a 被跳过 → k-b；多次选择恒为 k-b（不随时间恢复）。
        for _ in 0..3 {
            assert_eq!(selected_key(&pool, &config).as_deref(), Some("k-b"));
        }
        assert_eq!(pool.blacklisted_len(), 1, "黑名单不随选择衰减");
    }

    #[test]
    fn all_blacklisted_falls_back_to_legacy() {
        let pool = KeyPool::default_for_test();
        let config = pool_config();
        pool.mark_blacklisted(&fingerprint_of("k-a"));
        pool.mark_blacklisted(&fingerprint_of("k-b"));
        // legacy 名也在池内（合并名单含 LEGACY_UNUSED → k-legacy），同样拉黑
        pool.mark_blacklisted(&fingerprint_of("k-legacy"));
        assert!(
            selected_key(&pool, &config).is_none(),
            "全黑必须回退 legacy（与池耗尽现状一致）"
        );
    }

    #[test]
    fn hot_reload_prunes_blacklist_for_removed_keys() {
        let pool = KeyPool::default_for_test();
        let config = pool_config();
        assert_eq!(selected_key(&pool, &config).as_deref(), Some("k-a"));
        pool.mark_blacklisted(&fingerprint_of("k-a"));
        assert_eq!(selected_key(&pool, &config).as_deref(), Some("k-b"));

        // 热更：ENV_A 移除 → 名单变化 → k-a 条目被裁剪（状态重置）。
        let shrunk = vendor_config("LEGACY_UNUSED", &["ENV_B"]);
        assert_eq!(selected_key(&pool, &shrunk).as_deref(), Some("k-b"));
        assert_eq!(pool.blacklisted_len(), 0, "移除 key 的黑名单必须被裁剪");
    }

    #[test]
    fn value_change_prunes_stale_blacklist() {
        let pool = KeyPool::default_for_test();
        let config = pool_config();
        assert_eq!(selected_key(&pool, &config).as_deref(), Some("k-a"));
        pool.mark_blacklisted(&fingerprint_of("k-a"));

        // ENV_A 换值 k-a → k-a2：旧指纹不在新集合 → 裁剪。
        let rotated = lookup_map(&[
            ("ENV_A", "k-a2"),
            ("ENV_B", "k-b"),
            ("LEGACY_UNUSED", "k-legacy"),
        ]);
        match pool.select_with(&config, "v-test", &rotated) {
            RequestCredential::Pool { key, .. } => assert_eq!(key, "k-a2"),
            other => panic!("换值后必须选中新 key: {other:?}"),
        }
        assert_eq!(pool.blacklisted_len(), 0);
    }

    #[test]
    fn reset_clears_blacklist_and_rewinds_cursor() {
        let pool = KeyPool::default_for_test();
        let config = pool_config();
        pool.mark_blacklisted(&fingerprint_of("k-a"));
        pool.mark_blacklisted(&fingerprint_of("k-b"));
        pool.reset_key_state();
        assert_eq!(pool.blacklisted_len(), 0);
        assert_eq!(
            selected_key(&pool, &config).as_deref(),
            Some("k-a"),
            "重置后游标回绕、首 key 重新可选"
        );
    }
}
