// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 占位符注册表：同一 pepper 下同一 (kind, value) 任意请求产同一占位符
//! `{{FLUX_<KIND>_<8HEX>}}`（SHA256(pepper ‖ kind_tag ‖ 0x00 ‖ value) 前
//! 8 hex——0x00 分隔符防 tag/value 边界歧义）。
//!
//! pepper 三态来源（[`OnceLock`] 缓存，进程内恒定）：
//! 1. env `FLUXGATE_MASK_PEPPER`（64 hex = 32 字节）；
//! 2. `~/.fluxgate/mask_pepper.key`（0600；不存在则随机创建，仿
//!    `vault/master_key.rs` 先例；创建失败降级下一级并 warn）；
//! 3. 进程内随机（OS 熵种子派生；前两级都不可用时的降级面）。
//!
//! **跨进程/多副本确定性成立的条件**：各进程解析到同一 pepper——同机同
//! pepper 文件、多副本共享持久卷、或统一注入同一 env 值。pepper 各异时
//! 占位符漂移：上游前缀缓存全量失效（经济面损失）、跨轮对话占位符字面
//! 变化（值仍不出网关）——不出正确性问题（还原凭据是本请求登记集，与
//! pepper 无关）。降级为进程内随机时确定性不成立，warn 显性披露。
//!
//! 方向性：**value → placeholder 是进程内纯函数**；存储的反向映射
//!（placeholder → value）仅存内存，LRU 逐出（缺省 10k 条，测试可注入
//! 容量）+ TTL 24h，进程退出即弃，任何落盘路径不得包含原值。逐出后同值
//! 重掩码可再生同占位符并重建映射——进程内确定性使逐出不产生正确性风险。
//!
//! 还原放行判据：[`MaskTicket`]。回程还原只认**本次出站掩码登记过的**
//! 占位符——上游编造或他请求回显的占位符不还原（跨请求还原预言机防线），
//! 同时让零命中请求的还原零遍历。

use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::hash_map::RandomState;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 占位符前缀（`{{FLUX_`）
pub const PLACEHOLDER_PREFIX: &str = "{{FLUX_";
/// 占位符后缀（`}}`）
pub const PLACEHOLDER_SUFFIX: &str = "}}";
/// 最长占位符字节数：`{{FLUX_` + 最长 kind 标签（PRIVATEKEY）+ `_` + 8 hex +
/// `}}` = 28。流式 hold 缓冲上界判据消费（stream_restore 单测断言）。
#[cfg(test)]
pub const PLACEHOLDER_MAX_LEN: usize = 7 + 10 + 1 + 8 + 2;
/// 反向映射容量缺省（LRU 逐出上界）。
pub const DEFAULT_CAPACITY: usize = 10_000;
/// 反向映射条目 TTL（到期映射失效；同值重掩码重建）。
pub const ENTRY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// pepper 环境变量名（64 hex = 32 字节；最高优先级，覆盖 pepper 文件）。
pub const MASK_PEPPER_ENV: &str = "FLUXGATE_MASK_PEPPER";

/// 进程级 pepper：三态来源解析（env → pepper 文件 → 进程内随机），OnceLock
/// 缓存进程内恒定。解析核心 [`resolve_mask_pepper`] 绝不失败——pepper 面
/// 任何异常都降级随机并 warn，绝不因 pepper 问题拒绝请求。
fn pepper() -> &'static [u8; 32] {
    static PEPPER: OnceLock<[u8; 32]> = OnceLock::new();
    PEPPER.get_or_init(|| {
        let env = std::env::var(MASK_PEPPER_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty());
        resolve_mask_pepper(env.as_deref(), default_pepper_path().as_deref())
    })
}

/// 缺省 pepper 文件：`~/.fluxgate/mask_pepper.key`（home 不可得返回 None）。
fn default_pepper_path() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join(".fluxgate").join("mask_pepper.key"))
}

/// pepper 三态解析核心（env 值与文件路径注入，便于测试隔离）：
/// 1. env 非空 → 64 hex 解析；非法 → warn 降级下一级；
/// 2. 文件存在 → 64 hex 解析；非法 → warn 后降级进程内随机（不覆写坏
///    文件，留运维处置）；
/// 3. 文件不存在 → 随机生成并写盘（0600，仿 `vault/master_key.rs` 先例）；
///    创建失败 → warn 降级进程内随机。home 不可得 → warn 降级进程内随机。
pub(crate) fn resolve_mask_pepper(
    env_value: Option<&str>,
    file_path: Option<&std::path::Path>,
) -> [u8; 32] {
    if let Some(value) = env_value {
        if let Some(key) = parse_hex_pepper(value) {
            return key;
        }
        tracing::warn!(
            "{}",
            crate::i18n::t_args(
                "masking-pepper-degraded",
                &[
                    ("stage", "env".to_string()),
                    ("detail", "not 64 hex chars".to_string()),
                ],
            ),
        );
    }
    let Some(path) = file_path else {
        tracing::warn!(
            "{}",
            crate::i18n::t_args(
                "masking-pepper-degraded",
                &[
                    ("stage", "home".to_string()),
                    ("detail", "home dir unavailable".to_string()),
                ],
            ),
        );
        return process_random_pepper();
    };
    if let Ok(encoded) = std::fs::read_to_string(path) {
        if let Some(key) = parse_hex_pepper(encoded.trim()) {
            return key;
        }
        tracing::warn!(
            "{}",
            crate::i18n::t_args(
                "masking-pepper-degraded",
                &[
                    ("stage", "file".to_string()),
                    ("detail", "content not 64 hex chars".to_string()),
                ],
            ),
        );
        return process_random_pepper();
    }
    let key = process_random_pepper();
    match write_pepper_file(path, &key) {
        Ok(()) => key,
        Err(e) => {
            tracing::warn!(
                "{}",
                crate::i18n::t_args(
                    "masking-pepper-degraded",
                    &[("stage", "create".to_string()), ("detail", e.to_string()),],
                ),
            );
            process_random_pepper()
        }
    }
}

/// 64 hex（32 字节）解析；其余形态（含空串）一律 None。
fn parse_hex_pepper(value: &str) -> Option<[u8; 32]> {
    let trimmed = value.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = [0u8; 32];
    hex::decode_to_slice(trimmed, &mut key).ok()?;
    Some(key)
}

/// 进程内随机 32 字节（`RandomState` 的 OS 熵种子经 SipHash 派生 4×64bit
/// ——std 自带跨平台熵源，不为单点引入新依赖）。
fn process_random_pepper() -> [u8; 32] {
    let builder = RandomState::new();
    let mut seed = [0u8; 32];
    for (round, chunk) in seed.chunks_mut(8).enumerate() {
        let mut hasher = builder.build_hasher();
        hasher.write_u64(round as u64);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    seed
}

/// pepper 文件写盘（hex 文本；0600 直建防全局可读窗口 + set_permissions
/// 兜底收紧——与 `vault/master_key.rs` 同款先例）。
fn write_pepper_file(path: &std::path::Path, key: &[u8; 32]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let encoded = hex::encode(key);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(encoded.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, encoded.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// 进程内纯函数：value → placeholder（SHA256(pepper ‖ tag ‖ 0x00 ‖ value)
/// 前 8 hex；pepper 三态解析见模块文档——同 pepper 下跨进程/重启确定）。
pub fn placeholder_for(kind: super::MaskKind, value: &str) -> String {
    placeholder_with_pepper(pepper(), kind, value)
}

/// 显式 pepper 的占位符计算（测试与多态初始化面；生产走 [`placeholder_for`]）。
pub(crate) fn placeholder_with_pepper(
    pepper: &[u8; 32],
    kind: super::MaskKind,
    value: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pepper);
    hasher.update(kind.as_tag().as_bytes());
    hasher.update([0u8]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{PLACEHOLDER_PREFIX}{}_{}{}",
        kind.as_tag(),
        hex::encode_upper(&digest[..4]),
        PLACEHOLDER_SUFFIX
    )
}

/// 请求域还原凭据：一次出站掩码登记的占位符集合。回程还原只认本集
/// 内的占位符——未登记形态（上游编造/他请求回显）原样透传不还原。
#[derive(Default)]
pub struct MaskTicket {
    seen: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for MaskTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只披露登记条数——占位符集合虽非原值，Debug 面保持最小
        let count = self.seen.lock().unwrap_or_else(|p| p.into_inner()).len();
        f.debug_struct("MaskTicket")
            .field("recorded", &count)
            .finish()
    }
}

impl MaskTicket {
    /// 空凭据（未走掩码的请求形态；还原端整体跳过）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记占位符（掩码路径写入；`pub(crate)`——生产只经 masker 落账）。
    pub(crate) fn record(&self, placeholder: &str) {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(placeholder.to_string());
    }

    /// 占位符是否属于本请求登记集（还原放行判据）。
    pub fn contains(&self, placeholder: &str) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(placeholder)
    }

    /// 本请求是否登记过任何占位符（false → 还原可整体跳过）。
    pub fn is_empty(&self) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }
}

/// 反向映射条目：原值 + 写入时刻 + 近用序号（seq 全局递增，越大越近用）。
struct Entry {
    value: String,
    at: Instant,
    seq: u64,
}

struct Inner {
    /// 反向映射：placeholder → 条目
    reverse: HashMap<String, Entry>,
    /// 近用序最小堆（`Reverse(seq, key)`，堆顶 = 最旧）。触碰/重掩码产生
    /// 新 seq 后，堆内旧 `(seq, key)` 项**惰性失效**——弹出时与 map 当前
    /// seq 比对，不符即弃（一次近用一条堆项，摊还 O(log n)/操作）。
    lru: BinaryHeap<Reverse<(u64, String)>>,
    capacity: usize,
    /// 近用序号发生器（每次近用 +1）
    seq: u64,
}

/// 占位符注册表（线程安全；内部互斥锁保护反向映射）。
pub struct PlaceholderRegistry {
    inner: Mutex<Inner>,
}

impl Default for PlaceholderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PlaceholderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaceholderRegistry")
            .field("capacity", &DEFAULT_CAPACITY)
            .finish_non_exhaustive()
    }
}

impl PlaceholderRegistry {
    /// 缺省容量（[`DEFAULT_CAPACITY`]）的注册表。
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// 指定容量的注册表（测试注入小容量；生产走 [`Self::new`]）。
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                reverse: HashMap::new(),
                lru: BinaryHeap::new(),
                capacity: capacity.max(1),
                seq: 0,
            }),
        }
    }

    /// 登记映射并返回占位符（幂等：重复掩码同值刷新近用度，不增条目）。
    /// 命中路径 O(log n)（堆序更新）；容量压力下逐出最旧一条（见 `evict_one`）。
    pub fn mask(&self, kind: super::MaskKind, value: &str) -> String {
        let placeholder = placeholder_for(kind, value);
        let mut inner = self.lock();
        let now = Instant::now();
        inner.seq += 1;
        let seq = inner.seq;
        if let Some(entry) = inner.reverse.get_mut(&placeholder) {
            entry.at = now;
            entry.seq = seq;
            inner.lru.push(Reverse((seq, placeholder.clone())));
            return placeholder;
        }
        inner.reverse.insert(
            placeholder.clone(),
            Entry {
                value: value.to_string(),
                at: now,
                seq,
            },
        );
        inner.lru.push(Reverse((seq, placeholder.clone())));
        if inner.reverse.len() > inner.capacity {
            Self::evict_one(&mut inner);
        }
        placeholder
    }

    /// 反查占位符原值；未登记返回 None；TTL 过期返回 None 并清条目。
    /// 命中刷新近用度。
    pub fn restore(&self, placeholder: &str) -> Option<String> {
        let mut inner = self.lock();
        let entry = inner.reverse.get(placeholder)?;
        if entry.at.elapsed() >= ENTRY_TTL {
            inner.reverse.remove(placeholder);
            return None;
        }
        let now = Instant::now();
        inner.seq += 1;
        let seq = inner.seq;
        let value = {
            let entry = inner.reverse.get_mut(placeholder)?;
            entry.at = now;
            entry.seq = seq;
            entry.value.clone()
        };
        inner.lru.push(Reverse((seq, placeholder.to_string())));
        Some(value)
    }

    /// 容量溢出逐出**恰一条**最旧存活条目（精确 LRU 语义——在途请求引用
    /// 的近用映射不受批次误伤）：弹堆至堆顶 seq 与 map 一致（陈旧堆项惰性
    /// 弃置）；堆积超 4×capacity 时整体重建去陈旧（防纯触碰场景无界堆积）。
    fn evict_one(inner: &mut Inner) {
        while let Some(Reverse((seq, key))) = inner.lru.pop() {
            if inner.reverse.get(&key).is_some_and(|e| e.seq == seq) {
                inner.reverse.remove(&key);
                break;
            }
        }
        if inner.lru.len() > inner.capacity.saturating_mul(4) {
            inner.lru = inner
                .reverse
                .iter()
                .map(|(key, entry)| Reverse((entry.seq, key.clone())))
                .collect();
        }
    }

    /// 锁获取：中毒互斥锁恢复（`into_inner`）继续服务；掩码/还原全程在
    /// 门层 `catch_unwind` 内，锁面异常折叠为 [`super::MaskError::Unavailable`]。
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masking::MaskKind;

    #[test]
    fn placeholder_shape_and_determinism() {
        let p = placeholder_for(MaskKind::PrivateKey, "x");
        assert!(p.starts_with("{{FLUX_PRIVATEKEY_"));
        assert!(p.ends_with("}}"));
        assert_eq!(p.len(), PLACEHOLDER_MAX_LEN);
        assert_eq!(p, placeholder_for(MaskKind::PrivateKey, "x"));
        assert_ne!(
            placeholder_for(MaskKind::ApiKey, "x"),
            placeholder_for(MaskKind::Token, "x")
        );
    }

    /// 容量淘汰保近用：新条目存活、最旧条目（含刷新过的近用序被抬高的
    /// 条目按 seq 比较淘汰）——同值重掩码可再生同占位符并重建映射。
    #[test]
    fn capacity_eviction_keeps_recent_entries() {
        let registry = PlaceholderRegistry::with_capacity(4);
        let keys = ["a", "b", "c", "d", "e"];
        for key in keys {
            registry.mask(MaskKind::ApiKey, key);
        }
        // 溢出恰逐出最旧一条（精确 LRU，不误伤在途映射）
        assert_eq!(
            registry.restore(&placeholder_for(MaskKind::ApiKey, "a")),
            None
        );
        // 其余存活且映射正确
        for key in ["b", "c", "d", "e"] {
            assert_eq!(
                registry.restore(&placeholder_for(MaskKind::ApiKey, key)),
                Some(key.to_string())
            );
        }
        // 逐出后同值重掩码：占位符不变（进程内确定性）、映射重建
        let ph = registry.mask(MaskKind::ApiKey, "a");
        assert_eq!(ph, placeholder_for(MaskKind::ApiKey, "a"));
        assert_eq!(
            registry.restore(&ph),
            Some("a".to_string()),
            "逐出后重掩码必须重建反向映射"
        );
    }

    /// 近用刷新免淘汰：写入满容量后再触碰最旧条目，新插入淘汰的是
    /// 未触碰半边而非被刷新的条目。
    #[test]
    fn touch_protects_entry_from_eviction() {
        let registry = PlaceholderRegistry::with_capacity(2);
        registry.mask(MaskKind::ApiKey, "old");
        registry.mask(MaskKind::ApiKey, "mid");
        // 触碰 old：seq 抬到最大
        assert_eq!(
            registry.restore(&placeholder_for(MaskKind::ApiKey, "old")),
            Some("old".to_string())
        );
        registry.mask(MaskKind::ApiKey, "new");
        // 淘汰 seq 最小者 = mid
        assert_eq!(
            registry.restore(&placeholder_for(MaskKind::ApiKey, "mid")),
            None
        );
        assert_eq!(
            registry.restore(&placeholder_for(MaskKind::ApiKey, "old")),
            Some("old".to_string()),
            "近用触碰的条目必须存活"
        );
    }

    /// MaskTicket 契约：record/contains/is_empty；空凭据即跳过还原。
    #[test]
    fn mask_ticket_scopes_restoration() {
        let ticket = MaskTicket::new();
        assert!(ticket.is_empty());
        assert!(!ticket.contains("{{FLUX_APIKEY_00000000}}"));
        ticket.record("{{FLUX_APIKEY_00000000}}");
        assert!(!ticket.is_empty());
        assert!(ticket.contains("{{FLUX_APIKEY_00000000}}"));
        assert!(!ticket.contains("{{FLUX_APIKEY_11111111}}"));
    }

    /// 同 pepper 文件两次初始化：读回同值，占位符跨初始化一致
    ///（R-masking-001 验收：跨重启/多副本稳定性的核心语义）。
    #[test]
    fn same_pepper_file_two_inits_same_placeholder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mask_pepper.key");
        let first = resolve_mask_pepper(None, Some(&path));
        let second = resolve_mask_pepper(None, Some(&path));
        assert_eq!(first, second, "同文件两次初始化必须读回同一 pepper");
        assert_eq!(
            placeholder_with_pepper(&first, MaskKind::ApiKey, "secret"),
            placeholder_with_pepper(&second, MaskKind::ApiKey, "secret"),
            "同 pepper 下占位符必须一致"
        );
    }

    /// env 覆盖生效：env 值优先于文件，且不创建/不改写文件。
    #[test]
    fn env_overrides_file_without_touching_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mask_pepper.key");
        let env_hex = "ab".repeat(32);
        let from_env = resolve_mask_pepper(Some(env_hex.as_str()), Some(&path));
        assert_eq!(from_env, [0xab; 32]);
        assert!(!path.exists(), "env 生效时不得创建 pepper 文件");
        std::fs::write(&path, "cd".repeat(32)).expect("seed file");
        let again = resolve_mask_pepper(Some(env_hex.as_str()), Some(&path));
        assert_eq!(again, [0xab; 32], "env 必须压过已有文件");
        assert_eq!(
            std::fs::read_to_string(&path).expect("file intact"),
            "cd".repeat(32),
            "文件内容不得被 env 覆盖"
        );
    }

    /// 随机创建的 pepper 文件权限 0600（R-masking-001 验收）。
    #[test]
    fn generated_pepper_file_is_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("mask_pepper.key");
        let key = resolve_mask_pepper(None, Some(&path));
        assert_eq!(hex::encode(key).len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "pepper 文件必须 0600");
        }
    }

    /// env 非法（非 64 hex）→ warn 降级到文件链路（文件有效则用文件值）。
    #[test]
    fn invalid_env_degrades_to_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mask_pepper.key");
        std::fs::write(&path, "cd".repeat(32)).expect("seed file");
        let key = resolve_mask_pepper(Some("zz-not-hex"), Some(&path));
        assert_eq!(key, [0xcd; 32]);
    }

    /// 文件内容非法 → 降级进程内随机，且不覆写坏文件（留运维处置）。
    #[test]
    fn invalid_file_content_degrades_to_random_without_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mask_pepper.key");
        std::fs::write(&path, "definitely-not-hex").expect("seed bad file");
        let first = resolve_mask_pepper(None, Some(&path));
        let second = resolve_mask_pepper(None, Some(&path));
        assert_ne!(
            first, second,
            "降级随机必须逐进程独立（不可缓存面由 OnceLock 承担）"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("file intact"),
            "definitely-not-hex",
            "坏文件不得被静默覆写"
        );
    }

    /// 文件创建失败（父路径是普通文件）→ warn 降级进程内随机，绝不 panic。
    #[test]
    fn create_failure_degrades_to_random() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").expect("blocker");
        let path = blocker.join("mask_pepper.key");
        let key = resolve_mask_pepper(None, Some(&path));
        assert_eq!(key.len(), 32, "降级随机仍须产出可用 pepper");
    }
}
