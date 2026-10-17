// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{parse_config_content, validate_config, Config, ModelConfig, VendorConfig};
use crate::error::GateError;
use crate::i18n::{t, t_args};
use confers::dynamic::DynamicField;
use confers::snapshot::{SnapshotConfig, SnapshotFormat, SnapshotManager};
use confers::types::SourceId;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// tmp 文件序号 — 唯一临时文件名的进程内单调分量
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 快照目录名与保留份数 — 更新留痕的固定策略（不进配置面，热更新路径零新增必填项）
const SNAPSHOT_DIR_NAME: &str = "config-backups";
const SNAPSHOT_KEEP: usize = 10;

/// confers Redacted 序列化时打码的路径（api_keys 存的已是 SHA256 摘要，双保险）
const SNAPSHOT_REDACT_PATHS: &[&str] = &["auth.api_keys"];

/// 落盘前的旧配置快照（confers SnapshotManager：时间戳命名 + 超量自动清理）。
/// 解析/写盘任一步失败都只记 warn，绝不阻塞更新主路径。
fn snapshot_current_config(manager: &ConfigManager, config_path: &Path) {
    if !config_path.exists() {
        return; // 首次落盘没有旧值可留
    }
    let old = manager.get_config();
    let Ok(content) = toml::to_string_pretty(&old) else {
        tracing::warn!("{}", t("config-snapshot-serialize-failed"));
        return;
    };
    let dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(SNAPSHOT_DIR_NAME);
    let snapshots = SnapshotManager::new(SnapshotConfig {
        dir,
        max_snapshots: SNAPSHOT_KEEP,
        format: SnapshotFormat::Toml,
        include_provenance: false,
    });
    let saved = confers::loader::parse_toml(&content, SourceId::from("fluxgate-config"), None)
        .and_then(|annotated| snapshots.save_blocking(&annotated, SNAPSHOT_REDACT_PATHS));
    match saved {
        Ok(p) => tracing::debug!(
            "{}",
            t_args(
                "config-snapshot-saved",
                &[("path", p.display().to_string())]
            )
        ),
        Err(e) => tracing::warn!(
            "{}",
            t_args("config-snapshot-failed", &[("error", e.to_string())])
        ),
    }
}

/// 配置管理器：内存热读态由 confers DynamicField 承载（无锁读），
/// 文件写入为唯一临时文件 + rename 原子替换。写路径互斥（write_lock）：
/// 校验→序列化→落盘→内存切换全程串行，防并发落盘交错；RMW 场景用
/// update_config_with 在同一锁内完成，杜绝丢失更新。
/// 另外以磁盘内容指纹做乐观并发控制：写回前发现文件被进程外改动即拒绝，
/// 全量序列化不得静默抹掉尚未生效的外部编辑（如需重启的拓扑变更）。
#[derive(Clone)]
pub struct ConfigManager {
    config: Arc<DynamicField<Config>>,
    /// 启动加载路径（ConfigLoader::load 时注入）；None = 纯内存构造，update_config 拒绝落盘
    path: Arc<RwLock<Option<PathBuf>>>,
    /// 本进程上次观测到的磁盘配置指纹（见 [`Self::file_fingerprint`]）
    disk_fingerprint: Arc<RwLock<Option<u64>>>,
    /// 指纹哈希器：进程级随机键（SipHash）——指纹仅进程内比对、不持久化，
    /// 随机键使离线构造碰撞不可行（防伪造指纹绕过外部编辑保护）
    fingerprint_hash: RandomState,
    /// 写路径互斥（std Mutex：无 .await 穿越临界区，跨线程可用）
    write_lock: Arc<Mutex<()>>,
}

impl ConfigManager {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(DynamicField::new(config)),
            path: Arc::new(RwLock::new(None)),
            disk_fingerprint: Arc::new(RwLock::new(None)),
            fingerprint_hash: RandomState::new(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 字节内容指纹（进程随机键；file_fingerprint 与验证式基线采纳共用）
    fn bytes_fingerprint(&self, bytes: &[u8]) -> u64 {
        self.fingerprint_hash.hash_one(bytes)
    }

    /// 磁盘配置内容指纹（本进程视角：加载 / 热重载提交 / 成功写回后刷新）。
    /// 写回前与磁盘现值比对，用于识别进程外编辑。
    fn file_fingerprint(&self, path: &Path) -> Option<u64> {
        let bytes = std::fs::read(path).ok()?;
        Some(self.bytes_fingerprint(&bytes))
    }

    /// 记录配置文件加载路径（由 ConfigLoader::load 注入），并以当时磁盘
    /// 内容作为指纹基线
    pub fn set_path(&self, path: PathBuf) {
        *self
            .disk_fingerprint
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.file_fingerprint(&path);
        *self.path.write().unwrap() = Some(path);
    }

    /// 当前配置文件路径
    pub fn path(&self) -> Option<PathBuf> {
        self.path.read().unwrap().clone()
    }

    /// 热读当前配置 — DynamicField::get_ref 返回不可变 Arc 快照
    pub fn read(&self) -> Arc<Config> {
        self.config.get_ref()
    }

    pub fn get_config(&self) -> Config {
        (*self.read()).clone()
    }

    /// 更新配置：校验 → 原子落盘 → 内存切换（写锁全程持有）；
    /// 校验/落盘任一失败时内存态与磁盘均不变
    pub fn update_config(&self, new_config: Config) -> Result<(), GateError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.persist_and_swap(new_config)
    }

    /// 内存态热交换（热重载桥接用）：校验后只切内存不落盘，落盘只走 update_config。
    /// 桥接的候选配置来自磁盘当前内容，故提交后刷新指纹基线——刷新为验证式
    ///（见 `Self::refresh_baseline_if_matches`）：候选与磁盘现值不等价
    ///（两次外部写交错）时不刷新，防基线领先于未验证内容。
    pub fn swap_memory(&self, new_config: Config) -> Result<(), GateError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        validate_config(&new_config)?;
        self.refresh_baseline_if_matches(&new_config);
        self.config.update(new_config);
        Ok(())
    }

    /// 验证式采纳磁盘现值作为指纹基线（热重载桥接对「语义等价候选」调用）：
    /// 写锁内重读一次内容，仅当该份字节重解析后与当前内存态等价才采纳
    /// （指纹绑定被解析验证的字节），返回是否采纳。防两次外部写交错把
    /// 未经解析验证的中间态采纳为基线——基线领先会让后续管理面写回静默
    /// 覆盖该编辑。无路径 / 读取 / 解析 / 等价性任一失败均不采纳（基线
    /// 保持，写回继续受保护，由后续事件或重启兜底）。
    pub fn adopt_disk_baseline(&self) -> bool {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expected = self.read();
        self.refresh_baseline_if_matches(&expected)
    }

    /// 验证式刷新基线（调用方须持 write_lock）：读一次字节 → 重解析（格式按
    /// 路径扩展名检测）→ 仅当解析结果与 `expected` 等价时以该同一份字节的
    /// 指纹刷新基线（字节绑定）；不等价 / 不可读不刷新。返回是否刷新。
    fn refresh_baseline_if_matches(&self, expected: &Config) -> bool {
        let Some(path) = self.path() else {
            return false;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        let Ok(content) = std::str::from_utf8(&bytes) else {
            return false;
        };
        let Ok(parsed) = parse_config_content(&path, content) else {
            return false;
        };
        if &parsed != expected {
            return false;
        }
        *self
            .disk_fingerprint
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(self.bytes_fingerprint(&bytes));
        true
    }

    /// 原子 read-modify-write：写锁内读当前配置 → 应用闭包 → 校验落盘切换；
    /// 闭包返回 Err 时整个更新中止
    pub fn update_config_with<F>(&self, mutate: F) -> Result<(), GateError>
    where
        F: FnOnce(&mut Config) -> Result<(), GateError>,
    {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = self.get_config();
        mutate(&mut next)?;
        self.persist_and_swap(next)
    }

    /// 持锁前提下的公共尾部：校验 → 序列化 → 唯一 tmp → rename → 内存切换
    fn persist_and_swap(&self, new_config: Config) -> Result<(), GateError> {
        // 共享校验（与加载路径同一条 validate_config）
        validate_config(&new_config)?;

        // 序列化先行 — 失败不触碰磁盘与内存
        let content = toml::to_string_pretty(&new_config).map_err(|e| GateError::Config {
            message: t_args("config-serialize-failed", &[("error", e.to_string())]),
        })?;

        // 原子写：同目录唯一临时文件 + rename；唯一名（pid+序号）防并发/多进程互踩
        let path = self.path().ok_or(GateError::Config {
            message: t("config-path-unknown"),
        })?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        // 外部编辑检测：磁盘内容 ≠ 本进程上次观测（加载/热重载提交/写回）时
        // 拒绝写回——全量序列化会静默抹掉进程外编辑（如需重启才生效的
        // vendor 拓扑变更、注释与自定义段）。显式失败，磁盘保持原样。
        // 先做一次前置比对快速失败（避免无谓的快照/tmp 写），rename 前再做
        // 末次比对（见下）把无保护窗口压到系统调用间隙。
        let refuse_external_edit = || GateError::Config {
            message: t_args(
                "config-disk-modified-refusing-write",
                &[("path", path.display().to_string())],
            ),
        };
        let observed = *self
            .disk_fingerprint
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.file_fingerprint(&path) != observed {
            return Err(refuse_external_edit());
        }

        let mut tmp_name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "config.toml".into());
        tmp_name.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp_path = parent.join(tmp_name);

        // 3.5 更新留痕：落盘前把当前配置快照到 config-backups/。尽力而为——
        //     快照失败只记 warn，不阻塞合法更新（主回滚手段仍是 tmp+rename 原子性）。
        snapshot_current_config(self, &path);

        if let Err(e) = std::fs::write(&tmp_path, content) {
            // 半程写入（如 ENOSPC/ENOTDIR）清理临时文件，避免残留
            let _ = std::fs::remove_file(&tmp_path);
            return Err(GateError::Config {
                message: t_args(
                    "config-tmp-write-failed",
                    &[
                        ("path", tmp_path.display().to_string()),
                        ("error", e.to_string()),
                    ],
                ),
            });
        }

        // 末次比对（TOCTOU 窗口收窄）：快照与 tmp 写期间可能发生进程外编辑，
        // rename 前再比对一次——无保护窗口从「快照+tmp 写」压到比对与 rename
        // 的系统调用间隙；不匹配时清理 tmp 并显式拒绝
        if self.file_fingerprint(&path) != observed {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(refuse_external_edit());
        }

        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            // rename 失败时清理临时文件，避免残留
            let _ = std::fs::remove_file(&tmp_path);
            return Err(GateError::Config {
                message: t_args(
                    "config-atomic-replace-failed",
                    &[
                        ("path", path.display().to_string()),
                        ("error", e.to_string()),
                    ],
                ),
            });
        }

        // 4. 磁盘落定后才切换内存态（触发 on_change 回调），并把磁盘现值
        //    记为新的指纹基线（下次写回以此判外部编辑）
        if let Some(fingerprint) = self.file_fingerprint(&path) {
            *self
                .disk_fingerprint
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fingerprint);
        }
        self.config.update(new_config);
        Ok(())
    }

    pub fn get_vendor(&self, vendor_id: &str) -> Option<VendorConfig> {
        self.read()
            .vendors
            .iter()
            .find(|v| v.id == vendor_id)
            .cloned()
    }

    /// 大小写不敏感厂商查找（请求边界语义：`vendor.model` 直连与
    /// `X-Vendor` 头）；命中返回配置原文。配置校验（`validate_vendor_entries`）
    /// 已按 ASCII 大小写不敏感拒绝仅差大小写的重复条目，故经校验的配置
    /// 至多一个命中；未经校验的进程内直构（如测试）若持有该形态，取
    /// 声明序首个。管理/内部按精确 id 的调用方仍用 [`Self::get_vendor`]。
    pub fn get_vendor_ci(&self, vendor_id: &str) -> Option<VendorConfig> {
        self.read()
            .vendors
            .iter()
            .find(|v| v.id.eq_ignore_ascii_case(vendor_id))
            .cloned()
    }

    pub fn resolve_model(&self, abstract_id: &str) -> Vec<(VendorConfig, ModelConfig)> {
        let normalized = abstract_id.to_lowercase();
        let config = self.read();

        let mut results = Vec::new();

        for vendor in &config.vendors {
            if !vendor.enabled {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                let model_abstract = model
                    .abstract_id
                    .as_ref()
                    .unwrap_or(&model.model_id)
                    .to_lowercase();

                if model_abstract == normalized {
                    results.push((vendor.clone(), model.clone()));
                }
            }
        }

        results
    }

    pub fn get_enabled_vendors(&self) -> Vec<VendorConfig> {
        self.read()
            .vendors
            .iter()
            .filter(|v| v.enabled)
            .cloned()
            .collect()
    }

    /// `[global_model]` 段路由开关（auto 档是否可用）。
    /// 段缺省 = 启用：与字段级 `default_global_enabled` 同源，保持无该段
    /// 配置的历史兜底语义（auto 走全局池），只有显式 `enabled=false` 才关闭。
    pub fn is_global_model_enabled(&self) -> bool {
        let config = self.read();
        config
            .global_model
            .as_ref()
            .map(|g| g.enabled)
            .unwrap_or(true)
    }

    pub fn get_all_model_mappings(&self) -> HashMap<String, Vec<(String, String)>> {
        let config = self.read();
        let mut mappings: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for vendor in &config.vendors {
            if !vendor.enabled {
                continue;
            }

            for model in &vendor.models {
                if !model.enabled {
                    continue;
                }

                let abstract_id = model
                    .abstract_id
                    .as_ref()
                    .unwrap_or(&model.model_id)
                    .to_lowercase();

                mappings
                    .entry(abstract_id)
                    .or_default()
                    .push((vendor.id.clone(), model.model_id.clone()));
            }
        }

        mappings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ConcurrencyConfig, DatabaseConfig, GlobalModelConfig, RateLimitConfig, ServerConfig,
    };

    fn sample_config(vendor_id: &str) -> Config {
        Config {
            rate_limit: Default::default(),
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 8080,
            },
            database: DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![VendorConfig {
                id: vendor_id.to_string(),
                name: "Test".into(),
                type_: "openai".into(),
                api_key_env: "TEST_KEY".into(),
                api_key_envs: Vec::new(),
                extra_headers: Default::default(),
                api_base: String::new(),
                enabled: true,
                priority: 1,
                models: vec![],
                rate_limit: RateLimitConfig::Interval {
                    hours: 1,
                    sliding: false,
                },
                concurrency: ConcurrencyConfig {
                    requests_per_second: 10,
                    max_concurrent_requests: None,
                    token_weighted: false,
                    token_ticket_min: 1,
                    token_ticket_max: 16384,
                    token_ticket_default: 4096,
                },
                timeout_ms: 60_000,
                stream_ttft_ms: 60_000,
                stream_idle_ms: 30_000,
                supports_batch: false,
                chat_path: None,
            }],
            global_model: Some(GlobalModelConfig {
                enabled: true,
                strategy: "weighted".into(),
            }),
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: crate::config::RetrySection::default(),
            routing: crate::config::RoutingSection::default(),
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
            docs: Default::default(),
            lexcompress: Default::default(),
        }
    }

    /// `[global_model]` 开关语义：显式 enabled 权威；段缺省 = 启用
    ///（保持无该段配置的历史 auto 兜底行为）。
    #[test]
    fn test_is_global_model_enabled_defaults() {
        let mut config = sample_config("v1");
        config.global_model = Some(GlobalModelConfig {
            enabled: false,
            strategy: "weighted".into(),
        });
        assert!(!ConfigManager::new(config).is_global_model_enabled());

        let mut config = sample_config("v1");
        config.global_model = None;
        assert!(
            ConfigManager::new(config).is_global_model_enabled(),
            "缺 [global_model] 段必须沿用历史兜底（启用）"
        );
    }

    #[test]
    fn test_path_roundtrip() {
        let manager = ConfigManager::new(sample_config("v1"));
        assert!(manager.path().is_none(), "未注入路径时为 None");

        let dir = tempfile::tempdir().expect("tempdir creatable");
        manager.set_path(dir.path().join("config.toml"));
        assert_eq!(manager.path(), Some(dir.path().join("config.toml")));
    }

    #[test]
    fn test_update_config_without_path_is_rejected() {
        let manager = ConfigManager::new(sample_config("v1"));
        let result = manager.update_config(sample_config("v2"));
        assert!(result.is_err(), "无路径记忆时 update 必须拒绝落盘");
        // 内存态未变
        assert_eq!(manager.get_vendor("v1").unwrap().id, "v1");
        assert!(manager.get_vendor("v2").is_none());
    }

    /// 请求边界 CI 查找：大写配置 id 可被任意书写大小写命中且返回规范值；
    /// `get_vendor` 精确语义不回归（管理/内部调用方依赖）。
    #[test]
    fn test_get_vendor_ci_case_insensitive() {
        let manager = ConfigManager::new(sample_config("OpenAI-Upper"));
        assert_eq!(
            manager.get_vendor_ci("openai-upper").unwrap().id,
            "OpenAI-Upper",
            "配置大写 id 必须被小写请求命中且返回规范值"
        );
        assert_eq!(
            manager.get_vendor_ci("OPENAI-UPPER").unwrap().id,
            "OpenAI-Upper"
        );
        assert!(manager.get_vendor_ci("ghost").is_none(), "未命中返回 None");
        assert!(
            manager.get_vendor("openai-upper").is_none(),
            "get_vendor 精确语义不回归"
        );
    }

    #[test]
    fn test_update_config_rejects_duplicate_vendor_ids() {
        let manager = ConfigManager::new(sample_config("v1"));
        let mut bad = sample_config("v2");
        bad.vendors.push({
            let mut v = bad.vendors[0].clone();
            v.id = "v2".to_string();
            v
        });
        let result = manager.update_config(bad);
        assert!(result.is_err(), "重复 vendor id 必须被共享校验拦截");
        // 内存态未变
        assert!(manager.get_vendor("v2").is_none());
    }

    #[test]
    fn test_update_config_switches_memory_after_success() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "placeholder").unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        manager
            .update_config(sample_config("v2"))
            .expect("update ok");
        // ③ update 后 read() 立即可见（DynamicField 语义）
        assert!(manager.get_vendor("v2").is_some());
        assert!(manager.get_vendor("v1").is_none());
        // 磁盘已写回同一路径
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("v2"), "磁盘文件应含新 vendor id");
        // 无临时文件残留（唯一名 .tmp-{pid}-{seq} 均被 rename 消费）
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp 文件应被 rename 消费: {:?}",
            leftovers
        );
    }

    /// RMW 原子性：update_config_with 在写锁内基于最新内存态改单字段，
    /// 不回退其他字段（回归 admin update_vendor/update_model 裸三段式
    /// get_config→改→update_config 的丢失更新）
    #[test]
    fn test_update_config_with_preserves_other_fields() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 先把 api_key_env 改为 B
        let mut with_b = sample_config("v1");
        with_b.vendors[0].api_key_env = "KEY_B".to_string();
        manager.update_config(with_b).expect("seed update ok");

        // RMW 只改 name —— api_key_env 必须保持 B（裸三段式并发下会被
        // 旧快照覆盖回 A）
        manager
            .update_config_with(|config| {
                config.vendors[0].name = "renamed".to_string();
                Ok(())
            })
            .expect("rmw ok");

        let fresh = manager.get_vendor("v1").unwrap();
        assert_eq!(fresh.name, "renamed");
        assert_eq!(fresh.api_key_env, "KEY_B", "RMW 不得回退其他字段");
    }

    /// RMW 闭包返回 Err → 整体中止，内存与磁盘均不变
    #[test]
    fn test_update_config_with_closure_error_aborts() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());
        let before = std::fs::read(&path).unwrap();

        let result = manager.update_config_with(|_config| {
            Err(GateError::Config {
                message: "mutate rejected".to_string(),
            })
        });
        assert!(result.is_err(), "闭包 Err 必须传播");
        assert_eq!(manager.get_vendor("v1").unwrap().id, "v1", "内存态不变");
        assert_eq!(std::fs::read(&path).unwrap(), before, "磁盘文件字节不变");
    }

    /// 外部编辑保护：磁盘被进程外修改后写回必须显式拒绝，且磁盘逐字节
    /// 保持原样——全量序列化不得静默抹掉待生效的外部编辑（如按约定
    /// 「改 config.toml 后重启」新增、但热重载以 topology changed 拒绝的
    /// vendor 段）。
    #[test]
    fn test_update_config_refuses_when_disk_modified_externally() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 进程外编辑：在既有内容后追加待生效的 [[vendors]] 段
        let edited = format!(
            "{}\n[[vendors]]\nid = \"pending-vendor\"\n",
            std::fs::read_to_string(&path).unwrap()
        );
        std::fs::write(&path, &edited).unwrap();

        let result = manager.update_config(sample_config("v2"));
        assert!(
            matches!(result.as_ref(), Err(GateError::Config { .. })),
            "外部编辑后写回必须以 Config 错误显式失败: {result:?}"
        );
        if let Err(GateError::Config { message }) = result.as_ref() {
            let expected = t_args(
                "config-disk-modified-refusing-write",
                &[("path", path.display().to_string())],
            );
            assert_eq!(message, &expected, "拒绝必须为外部编辑保护键原文且指名路径");
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            edited,
            "拒绝写回时磁盘保持原样（外部编辑不丢）"
        );
        assert!(manager.get_vendor("v1").is_some(), "内存态不变");
        assert!(manager.get_vendor("v2").is_none(), "未切换到新配置");
    }

    /// 热重载提交刷新指纹基线：监听桥接接受磁盘内容后，后续写回不得
    /// 被误判为外部冲突（否则成功热重载一次即永久禁用管理面写回）。
    #[test]
    fn test_write_back_succeeds_after_hot_reload_commit() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 外部编辑 → 热字段变更被监听接受（swap_memory 即桥接落点）
        let mut hot = sample_config("v1");
        hot.retry.max_retries += 1;
        std::fs::write(&path, toml::to_string_pretty(&hot).unwrap()).unwrap();
        manager.swap_memory(hot).expect("热重载提交");

        manager
            .update_config(sample_config("v2"))
            .expect("热重载提交后的写回不得被判为外部冲突");
        assert!(manager.get_vendor("v2").is_some(), "写回已生效");
    }

    /// swap_memory 的基线刷新同为验证式（评审复核发现的同害窗口）：
    /// 候选与磁盘现值不等价（两次外部写交错）时不刷新基线——磁盘上的
    /// 待生效编辑继续受写回保护。
    #[test]
    fn test_swap_memory_baseline_refresh_is_verified() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(&sample_config("v1")).unwrap()).unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 模拟交错：热重载候选（v1 + retry 变化）已解析，但磁盘已被第二次
        // 外部编辑（结构性 v9）覆盖
        let mut hot = sample_config("v1");
        hot.retry.max_retries += 1;
        let external = toml::to_string_pretty(&sample_config("v9")).unwrap();
        std::fs::write(&path, &external).unwrap();
        manager
            .swap_memory(hot)
            .expect("热重载提交（内存态照常切换）");

        // 基线不得被刷成 v9 的指纹：写回必须继续拒绝并保住 v9 磁盘内容
        assert!(
            manager.update_config(sample_config("v2")).is_err(),
            "候选与磁盘不等价时基线不得刷新，写回必须被拒"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            external,
            "受保护的磁盘编辑不得被覆盖"
        );
    }

    /// tmp 半程写失败清理：tmp 写失败（父路径非目录 → ENOTDIR）时显式拒绝
    /// 且不留 tmp 残留（跨平台成立，不依赖文件权限）。
    #[test]
    fn test_tmp_write_failure_leaves_no_tmp_residue() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let not_a_dir = dir.path().join("not-a-dir");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let path = not_a_dir.join("config.toml");

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        match manager.update_config(sample_config("v2")) {
            Err(GateError::Config { message }) => {
                assert!(message.contains(".tmp-"), "必须报 tmp 写失败: {message}");
            }
            other => panic!("父路径非目录时写回必须失败，实际 {other:?}"),
        }
        assert!(not_a_dir.is_file(), "不得在文件路径下建目录或留残留");
        assert!(manager.get_vendor("v2").is_none(), "内存态不得切换");
    }

    /// 语义等价外部编辑（仅注释/格式化）经桥接采纳基线后写回必须恢复：
    /// 采纳前拒绝（既有保护不变），adopt_disk_baseline 后同一写回成功。
    #[test]
    fn test_adopt_disk_baseline_unfreezes_write_back() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        let initial = toml::to_string_pretty(&sample_config("v1")).unwrap();
        std::fs::write(&path, &initial).unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 进程外注释级编辑：字节变、语义等价 → 未采纳前写回必须拒绝
        let commented = format!("# 外部注释（语义等价）\n{initial}");
        std::fs::write(&path, &commented).unwrap();
        assert!(
            manager.update_config(sample_config("v2")).is_err(),
            "字节级外部编辑未采纳前必须拒绝写回"
        );

        // 桥接对等价候选采纳基线 → 同一写回恢复
        assert!(manager.adopt_disk_baseline(), "等价磁盘必须采纳成功");
        manager
            .update_config(sample_config("v2"))
            .expect("采纳基线后写回必须恢复");
        assert!(manager.get_vendor("v2").is_some(), "写回已生效");

        // no-op 分支：纯内存构造（无路径）不采纳、不 panic
        assert!(
            !ConfigManager::new(sample_config("v3")).adopt_disk_baseline(),
            "无路径必须不采纳"
        );
    }

    /// 验证式采纳的拒绝面：磁盘重解析与内存态不等价（如待重启的拓扑编辑）
    /// 或文件缺失时不得采纳——该编辑必须继续受写回保护。
    #[test]
    fn test_adopt_disk_baseline_refuses_non_equivalent_or_missing_disk() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(&sample_config("v1")).unwrap()).unwrap();

        let manager = ConfigManager::new(sample_config("v1"));
        manager.set_path(path.clone());

        // 进程外结构性编辑（语义不同）：拒绝采纳，写回继续被保护
        std::fs::write(&path, toml::to_string_pretty(&sample_config("v9")).unwrap()).unwrap();
        assert!(
            !manager.adopt_disk_baseline(),
            "不等价磁盘必须拒绝采纳（防基线领先于未验证内容）"
        );
        assert!(
            manager.update_config(sample_config("v2")).is_err(),
            "拒绝采纳后写回仍必须受外部编辑保护"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            toml::to_string_pretty(&sample_config("v9")).unwrap(),
            "被保护编辑不得被覆盖"
        );

        // 文件缺失：不采纳、不 panic
        std::fs::remove_file(&path).unwrap();
        assert!(!manager.adopt_disk_baseline(), "文件缺失必须拒绝采纳");
    }

    /// 并发 update 一致性：多线程并发 POST 等价的
    /// update_config，写锁串行化后最终磁盘 == 最终内存态，且无 tmp 残留。
    /// 无锁交错会导致磁盘与内存分裂（磁盘=后者内容、内存=前者值）。
    #[test]
    fn test_concurrent_updates_leave_disk_and_memory_consistent() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = std::sync::Arc::new(ConfigManager::new(sample_config("v0")));
        manager.set_path(path.clone());

        const THREADS: usize = 8;
        const ITERS: usize = 25;
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let m = manager.clone();
                std::thread::spawn(move || {
                    for i in 0..ITERS {
                        m.update_config(sample_config(&format!("c{t}-{i}")))
                            .expect("concurrent update ok under write lock");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread should not panic");
        }

        // 最终内存态
        let mem_id = manager.read().vendors[0].id.clone();
        assert!(mem_id != "v0", "并发更新应已生效");
        // 磁盘内容与最终内存态一致（解析磁盘 TOML 比对 vendor id）
        let disk = std::fs::read_to_string(&path).unwrap();
        let disk_config: Config = toml::from_str(&disk).expect("disk file must parse");
        assert_eq!(
            disk_config.vendors[0].id, mem_id,
            "磁盘必须等于最后一次内存切换的值（无分裂）"
        );
        // 无 tmp 残留
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "无 tmp 残留: {:?}", leftovers);
    }

    /// 并发 RMW 不丢字段：两个线程分别经 update_config_with 改 name 与
    /// api_key_env，最终两者都保留各自最后一次写入（裸三段式会互相覆盖）
    #[test]
    fn test_concurrent_rmw_does_not_lose_fields() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "seed").unwrap();

        let manager = std::sync::Arc::new(ConfigManager::new(sample_config("v1")));
        manager.set_path(path.clone());

        const ITERS: usize = 50;
        let name_worker = {
            let m = manager.clone();
            std::thread::spawn(move || {
                for i in 0..ITERS {
                    m.update_config_with(|c| {
                        c.vendors[0].name = format!("name-{i}");
                        Ok(())
                    })
                    .expect("rmw ok");
                }
            })
        };
        let env_worker = {
            let m = manager.clone();
            std::thread::spawn(move || {
                for i in 0..ITERS {
                    m.update_config_with(|c| {
                        c.vendors[0].api_key_env = format!("env-{i}");
                        Ok(())
                    })
                    .expect("rmw ok");
                }
            })
        };
        name_worker.join().unwrap();
        env_worker.join().unwrap();

        let mem = manager.get_vendor("v1").unwrap();
        let disk_config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let disk = &disk_config.vendors[0];

        // 各字段的最终值来自各自的最后一次写入，且磁盘 == 内存
        assert_eq!(
            mem.name,
            format!("name-{}", ITERS - 1),
            "name 最后一次写入保留"
        );
        assert_eq!(
            mem.api_key_env,
            format!("env-{}", ITERS - 1),
            "api_key_env 最后一次写入保留"
        );
        assert_eq!(mem.name, disk.name, "磁盘与内存一致（name）");
        assert_eq!(mem.api_key_env, disk.api_key_env, "磁盘与内存一致（env）");
    }

    /// [grpc] 段 serde 行为：缺段 = None（禁用）、全键缺省值、显式值透传
    #[test]
    fn test_grpc_section_serde() {
        use crate::config::GrpcConfig;

        // 缺段 → None（不监听 gRPC 端口）
        let config: Config = toml::from_str(
            r#"
            [server]
            port = 8080
            [database]
            path = ":memory:"
            [[vendors]]
            id = "v"
            name = "V"
            type_ = "openai"
            api_key_env = "K"
            rate_limit = { type = "token", max_tokens = 1 }
            concurrency = { requests_per_second = 1 }
            "#,
        )
        .unwrap();
        assert!(config.grpc.is_none(), "缺 [grpc] 段必须为 None");

        // 空段（enabled: false 隐含）→ 全默认值
        let config: Config = toml::from_str(
            r#"
            [server]
            port = 8080
            [database]
            path = ":memory:"
            [[vendors]]
            id = "v"
            name = "V"
            type_ = "openai"
            api_key_env = "K"
            rate_limit = { type = "token", max_tokens = 1 }
            concurrency = { requests_per_second = 1 }
            [grpc]
            "#,
        )
        .unwrap();
        let grpc = config.grpc.expect("空 [grpc] 段应为 Some");
        assert!(!grpc.enabled);
        assert_eq!(grpc.host, "0.0.0.0");
        assert_eq!(grpc.port, 50051);
        assert_eq!(grpc.timeout_seconds, 30);
        assert_eq!(grpc.max_connections, 1000);
        assert_eq!(grpc.http2_keepalive_interval_secs, None);
        assert_eq!(grpc.http2_keepalive_timeout_secs, None);

        // 显式值透传
        let grpc = GrpcConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 60000,
            timeout_seconds: 0,
            max_connections: 0,
            http2_keepalive_interval_secs: Some(15),
            http2_keepalive_timeout_secs: Some(5),
        };
        let text = toml::to_string(&grpc).unwrap();
        let parsed: GrpcConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed, grpc, "GrpcConfig 序列化往返必须保真");
    }
}
