// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! vendor 内 key 池（T005/N5）— env 名池解析、轮换游标与按 key 指纹的
//! 熔断状态隔离。
//!
//! 语义对齐 agentstem `provider_chain.rs:197-236`（实现不复用旧代码）：
//! - **池展开**：显式 env 名列表（`VendorConfig::api_key_envs`）优先于池
//!   env（`api_key_env` 向后兼容单名，可逗号分隔多个名），两来源合并去重
//!   定序（显式列表在前），池上限截断；
//! - **轮换**：vendor 实例持解析后的 key 池与游标，出站请求用当前 key；
//!   401/403 认证类失败推进游标到下一可用 key——轮换生效于**下次尝试**，
//!   不在单请求内换 key 重试（与既有 limiteron 重试/fallback 预算语义
//!   对齐，最小侵入）；
//! - **隔离**：key 经 16 位十六进制指纹参与熔断状态键（vendor_id + model + endpoint + 指纹，对齐 provider_chain breaker_state_key 维度）——同 key 同 model 跨请求共享计数，不同 key / 不同 model 独立。指纹哈希用进程内随机密钥（`RandomState`，进程启动时随机）：进程内稳定、跨进程随机——admin 观测的指纹不可被离线复算（消除低熵 key 的离线穷举 oracle），指纹仍不出网原文；
//! - **回退**：池耗尽/全缺 env 回退既有单 key 行为 + 显性告警，禁止静默
//!   （含请求期 env 收缩路径——池收缩与全缺同样显性）。
//!
//! **与 provider_chain 的已知偏差**（记 design.md D7，编排方回写）：
//! provider_chain 侧坏 key 被熔断短路后由候选链自动让位下一 key；本池
//! 侧无请求内候选链，坏 key 的让位经**熔断冷却窗后的探针自愈**间接实现
//! （每坏 key 每次到访约一个 cooling 窗自愈），游标仅由 401/403 认证类
//! 失败联动。
//!
//! 记账口径声明：GlobalMetrics（成败/延迟/失败率滑窗）与 CostTracker 维持
//! vendor 级——路由健康与成本报表是 vendor 维度的运营信号，不随 key 轮换
//! 碎片化；按 key 隔离的仅熔断计数（对齐 provider_chain 语义的迁移范围）。
//!
//! 日志脱敏约定：env 名列表缺失告警不回显配置原文（用户可能误将 key
//! 原文当作 env 名填入配置）——只打印序号 + 值长度 + 核对提示。

use serde::Serialize;
use std::collections::HashSet;
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::config::VendorConfig;

/// 指纹哈希的进程内随机密钥（进程启动首次使用时初始化）：进程内所有
/// 指纹同源（稳定），跨进程随机（不可离线复算）。
static FINGERPRINT_KEYS: OnceLock<RandomState> = OnceLock::new();

/// 单 vendor key 池上限（防配置爆炸；截断显性告警）。
///
/// agentstem 侧同名常量为 4（其链元素数 = 候选数 × 池大小，需防乘积爆炸）；
/// 本池为 per-vendor 单层展开（无候选数乘积），取 8 对齐 server PUT
/// 候选上限。截断时以 `tracing::warn` 显性化，禁止静默。
pub const MAX_KEY_POOL: usize = 8;

/// 合并去重定序的 key 池 env 名（纯函数）。
///
/// `api_key_envs` 显式列表在前，`api_key_env`（向后兼容，可逗号分隔多个名）
/// 兜底在后；条目 trim、去空、跨来源去重（保留首次出现位置）。
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

/// 池解析结果（纯函数 [`resolve_key_pool`] 的产物）。
///
/// Debug 手写脱敏：`keys` 只出 `[REDACTED n keys]` 数量（同
/// [`RequestCredential`] 口径）；`missing` 为 env 名（非机密，同 config
/// 暴露面）与截断计数保留可观测。
#[derive(Default, PartialEq)]
pub(crate) struct ResolvedPool {
    /// 实际 key 串（顺序 = env 名顺序；未设置或空值的 env 视同缺失跳过）
    pub keys: Vec<String>,
    /// 缺失（未设置或空值）的 env 名——调用方逐名显性告警
    pub missing: Vec<String>,
    /// 超出 [`MAX_KEY_POOL`] 被截断的 env 名条数
    pub truncated: usize,
}

impl std::fmt::Debug for ResolvedPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // missing 只出计数——防「误将 key 原文填进 env 名位置」经 Debug 回显
        // （与 log_resolution 只报序号+长度的脱敏约定同口径）。
        f.debug_struct("ResolvedPool")
            .field("keys", &format!("[REDACTED {} keys]", self.keys.len()))
            .field("missing", &self.missing.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// 池解析纯函数核（env 经 `lookup` 注入，可测）。
///
/// 名单先截断到 [`MAX_KEY_POOL`]（`truncated` 记超量条数），再逐名解析：
/// 缺失/空值跳过并计入 `missing`，不中断后续名解析。
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

/// key 指纹 — 16 位十六进制（进程内随机密钥哈希）。
///
/// 语义对齐 provider_chain.rs:197-214 的维度，密钥策略加固（安全 L1）：
/// `RandomState` 进程启动时随机播种——**进程内稳定**（同 key 恒同指纹，
/// 跨请求共享熔断计数的前提；admin 面同一进程内可对照），**跨进程随机**
/// （指纹不可被离线复算——消除「拿 admin 指纹对候选 key 离线穷举验证」
/// 的 oracle；低熵 key 亦不可穷举）。指纹仅进程内存活，禁止外泄。
pub(crate) fn fingerprint_of(key: &str) -> String {
    let state = FINGERPRINT_KEYS.get_or_init(RandomState::new);
    format!("{:016x}", state.hash_one(key))
}

/// 请求级凭证——出站路径的 key 选择结果。
///
/// 纵深防御：`Debug` 手写脱敏——`Pool` 臂不回显 key 原文，输出
/// `Pool { key: "[REDACTED n chars]", fingerprint: ... }` 形态（对齐
/// GateCandidate 的 `[REDACTED]` 脱敏口径）；指纹为进程内随机密钥哈希
/// （见 [`fingerprint_of`]），非机密可观测。`LegacyEnv` 臂无字段
/// （build 时读 env，不持凭证），无脱敏面。
#[derive(Clone, PartialEq)]
pub(crate) enum RequestCredential {
    /// 池命中：本次出站请求固定使用该 key（指纹参与熔断状态键）。
    /// 单请求内所有重试尝试复用同一 key（轮换生效于下次尝试）。
    Pool { key: String, fingerprint: String },
    /// 池空回退：既有 `api_key_env` 单 key 行为（build 时读 env，
    /// 缺失报 `Config` 错误——与历史逐位一致），不参与轮换与按 key 隔离。
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
    /// 无 resilience 组合下仅单测消费（按 key 隔离是 resilience 语义）。
    #[cfg_attr(not(feature = "resilience"), allow(dead_code))]
    pub(crate) fn fingerprint(&self) -> Option<&str> {
        match self {
            RequestCredential::Pool { fingerprint, .. } => Some(fingerprint),
            RequestCredential::LegacyEnv => None,
        }
    }
}

/// key 池观测快照（admin 面）——只暴露 env 名 / 数量 / 下标 / 指纹，
/// 绝不暴露 key 原文（掩码约定，与 `/admin/config` 仅存 env 名的既有
/// 暴露面一致）。
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

/// 解析快照——实例持有的「按 env 名单解析出的 key 池」（env 模式专属；
/// 内联注入模式见 [`PoolSnapshot`]）。
///
/// Debug 手写脱敏同 [`ResolvedPool`]：`keys` 只出数量，env 名与计数保留。
#[derive(Default, PartialEq)]
struct ResolvedSnapshot {
    /// 最近一次解析的合并 env 名（变更检测键：名单变 → 全量重解析）
    names: Vec<String>,
    /// 解析后的 key 池（空 = 池耗尽/全缺 env）
    keys: Vec<String>,
    /// 缺失 env 名计数（显性化用）
    missing: usize,
    /// 被截断的 env 名条数
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

/// 预解析 key 池快照（编程式注入面，A2 审查回写安全 HIGH 根治）。
///
/// 由调用方以 **key 原文列表**直接构造（[`PoolSnapshot::from_keys`]），
/// 经 [`crate::vendor::VendorManager::set_inline_key_pool`] 注入 vendor
/// 实例，`select` 全程跳过 env 名单解析与环境变量读取（config 面仍只存
/// env 名——注入是库 API 编程式能力，不改变 NexusFlow 的 config 暴露面
/// 约定）。
///
/// 语义边界：
/// - **指纹/游标与 env 池一致**：同一 `fingerprint_of`、同一轮换游标，
///   401/403 推进游标轮换下一把，per-key 指纹熔断隔离照常生效；
/// - **env 变更检测/重解析不适用**：内联池没有 env 名，名单比对与
///   「env 值现读」分支整体跳过——池内容由调用方一次性给定，生命周期
///   由调用方管理（vendor 实例存活期内有效；实例重建或换 vendor_id 后
///   需重新注入）；
/// - 空池（剔除空白项后无 key）等价 env 池耗尽：回退 legacy 单 key 行为
///   （config `api_key_env` 现读，缺失则请求不带 Authorization 头）；
/// - Debug 手写脱敏：`keys` 只出 `[REDACTED n keys]` 数量（指纹观测面在
///   [`KeyPoolInfo`]，指纹非机密保留）。
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
    /// 由 key 原文列表构造（顺序即轮换序；空白项剔除；上界
    /// [`MAX_KEY_POOL`] 截断——与 env 池同界）。
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

    /// 是否为空池。
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// vendor 实例持有的 key 池状态：解析快照 + 轮换游标。
pub(crate) struct KeyPool {
    snapshot: Mutex<ResolvedSnapshot>,
    /// 内联注入池（Some 时 select 全程跳过 env resolve，见 [`PoolSnapshot`]）。
    inline: Mutex<Option<PoolSnapshot>>,
    /// 轮换游标：401/403 认证类失败 `fetch_add(1)`；出站取
    /// `keys[cursor % len]`。并发下多次 401 连续推进是保守方向（跳过
    /// 失败 key），游标语义只要求「下次尝试换下一把」。
    cursor: AtomicUsize,
}

impl KeyPool {
    /// 实例构造期解析一次（admin 面在首个请求前即有快照可读）；
    /// 后续每次 [`Self::select`] 按需刷新（名单变更或 env 值变化）。
    pub(crate) fn new(config: &VendorConfig, vendor_id: &str) -> Self {
        let pool = Self {
            snapshot: Mutex::new(ResolvedSnapshot::default()),
            inline: Mutex::new(None),
            cursor: AtomicUsize::new(0),
        };
        pool.select(config, vendor_id);
        pool
    }

    /// 单测基座：空快照池（首个 `select_with` 按注入 lookup 完成解析，
    /// 不触真实环境变量）。
    #[cfg(test)]
    fn default_for_test() -> Self {
        Self {
            snapshot: Mutex::new(ResolvedSnapshot::default()),
            inline: Mutex::new(None),
            cursor: AtomicUsize::new(0),
        }
    }

    /// 注入内联池（编程式；见 [`PoolSnapshot`]）。注入后 `select` 全程
    /// 跳过 env resolve；重复注入以最后一次为准（调用方管理生命周期）。
    pub(crate) fn set_inline(&self, snapshot: PoolSnapshot) {
        *self.inline.lock().unwrap() = Some(snapshot);
    }

    /// 请求级凭证选择——执行路径统一入口。
    ///
    /// 内联池激活时直接按游标取 key（跳过 env resolve，见
    /// [`PoolSnapshot`]）。env 模式：名单变（配置热更新）→ 全量重解析并
    /// 显性告警；名单不变 → env 值仍现读（env 值变化立即生效，与既有
    /// per-attempt 读 env 语义一致），仅在结果变化时更新快照（避免日志
    /// 刷屏）。池空回退 [`RequestCredential::LegacyEnv`]。
    pub(crate) fn select(&self, config: &VendorConfig, vendor_id: &str) -> RequestCredential {
        self.select_with(config, vendor_id, &|name| std::env::var(name).ok())
    }

    /// [`Self::select`] 的可注入核（env 经 `lookup` 注入，可测）。
    /// `lookup` 按引用传入——同一用例内可复用（对齐生产「每请求现读」语义）。
    pub(crate) fn select_with(
        &self,
        config: &VendorConfig,
        vendor_id: &str,
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> RequestCredential {
        // 内联池快路径：跳过 env 名单解析与变更检测（见 [`PoolSnapshot`]）
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            if inline.keys.is_empty() {
                return RequestCredential::LegacyEnv;
            }
            let index = self.cursor.load(Ordering::Relaxed) % inline.keys.len();
            let key = inline.keys[index].clone();
            return RequestCredential::Pool {
                key,
                fingerprint: fingerprint_of(&inline.keys[index]),
            };
        }

        let names = merged_key_env_names(config);
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
        } else {
            // 名单未变：env 值现读，仅结果变化时落快照（热更新立即生效）。
            // 收缩与全缺同样显性（禁止静默），明细只报
            // 数量不回显 env 名（脱敏约定见模块 doc）。
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
            }
        }
        let keys = guard.keys.clone();
        drop(guard);

        if keys.is_empty() {
            return RequestCredential::LegacyEnv;
        }
        let index = self.cursor.load(Ordering::Relaxed) % keys.len();
        let key = &keys[index];
        RequestCredential::Pool {
            key: key.clone(),
            fingerprint: fingerprint_of(key),
        }
    }

    /// 解析显性化（仅名单变更时调用一次，禁止每请求刷屏）：
    /// 逐缺失条目 warn、截断 warn、全缺回退 warn。
    /// 安全 L4 脱敏：缺失条目不回显配置原文（防误填的 key 原文进日志），
    /// 只报序号 + 值长度 + 核对提示。
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

    /// 路由热路径用：只读当前指纹（不触 env、不写快照、不产生日志）。
    /// 池空（legacy 回退）返回 `None`。无 resilience 组合下仅单测消费。
    #[cfg_attr(not(feature = "resilience"), allow(dead_code))]
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

    /// 401/403 认证类失败推进游标——轮换生效于**下次尝试**，当前请求的
    /// 剩余尝试仍用启动时解析的同一 key。池未激活（keys 空）时 no-op。
    /// 多 key 池时显性 warn（size≤1 无轮换语义，401 上游告警已覆盖，不刷屏）。
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

    /// 激活池长度（内联池优先，env 快照兜底）——游标取模与轮换判定的
    /// 单一事实源。
    fn active_pool_len(&self) -> usize {
        if let Some(inline) = self.inline.lock().unwrap().clone() {
            return inline.keys.len();
        }
        self.snapshot.lock().unwrap().keys.len()
    }

    /// admin 观测快照（仅 env 名 / 数量 / 下标 / 指纹，绝无 key 原文）。
    /// 内联池无 env 名，`envs` 为空列表、`size`/`fingerprints` 反映真实池。
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
    //! key 池单元测试（T005 验收：轮换顺序与耗尽行为 / 指纹稳定性）。
    //! env 经 lookup 注入，无真实环境变量竞态；同一用例内 lookup 恒定，
    //! 对齐生产语义「env 值每次请求现读」。

    use super::*;

    fn vendor_config(api_key_env: &str, api_key_envs: &[&str]) -> VendorConfig {
        VendorConfig {
            id: "v-test".into(),
            name: "Test".into(),
            type_: "openai".into(),
            api_key_env: api_key_env.into(),
            api_key_envs: api_key_envs.iter().map(|s| s.to_string()).collect(),
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
        }
    }

    fn lookup_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
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

    /// 安全 L1：指纹哈希用进程内随机密钥——**进程内**稳定（同 key 恒同
    /// 指纹，共享计数前提）、不同 key 互异（隔离前提）；跨进程随机性
    /// （不可离线复算）由 RandomState 播种保证，无法在单进程内断言。
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

    /// `RequestCredential` 手工 Debug：Pool 臂不回显 key 原文（
    /// `[REDACTED n chars]` 形态，对齐 GateCandidate 脱敏口径），指纹
    /// 非机密保留可观测；LegacyEnv 臂无字段、无脱敏面。
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

    /// PoolSnapshot / ResolvedPool / ResolvedSnapshot 手工 Debug（批复 2
    /// 顺手修）：keys 列表只出 `[REDACTED n keys]` 数量，不回显 key 原文；
    /// env 名同样只出计数——防「误将 key 原文填进 env 名位置」经 Debug 回显
    /// （与 log_resolution 只报序号+长度的约定同口径）。
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

    /// 审查 M3：请求期 env 收缩——部分消失收缩池（游标取模钳制），
    /// 全部消失回退 legacy 且游标推进 no-op（显性 warn 在日志层，
    /// 此处断言状态机语义）。
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
        // 全部消失：回退 legacy，游标推进 no-op
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
    //! 内联池注入面（A2 审查回写）：指纹/游标与 env 池一致，env resolve
    //! 整体跳过，生命周期由调用方管理。

    use super::*;

    fn inline_vendor_config(api_key_env: &str) -> VendorConfig {
        VendorConfig {
            id: "gate-0".into(),
            name: "Inline".into(),
            type_: "openai".into(),
            api_key_env: api_key_env.into(),
            api_key_envs: vec![],
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
}
