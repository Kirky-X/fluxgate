// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use super::{validate_config, Config, ModelConfig, VendorConfig};
use crate::error::GateError;
use crate::i18n::{t, t_args};
use confers::dynamic::DynamicField;
use confers::snapshot::{SnapshotConfig, SnapshotFormat, SnapshotManager};
use confers::types::SourceId;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
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

/// 磁盘配置内容指纹（本进程视角：加载 / 热重载提交 / 成功写回后刷新）。
/// 写回前与磁盘现值比对，用于识别进程外编辑。
fn file_fingerprint(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

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
    let saved = confers::loader::parse_toml(&content, SourceId::from("nexusflow-config"), None)
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
    /// 本进程上次观测到的磁盘配置指纹（见 file_fingerprint）
    disk_fingerprint: Arc<RwLock<Option<u64>>>,
    /// 写路径互斥（std Mutex：无 .await 穿越临界区，跨线程可用）
    write_lock: Arc<Mutex<()>>,
}

impl ConfigManager {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(DynamicField::new(config)),
            path: Arc::new(RwLock::new(None)),
            disk_fingerprint: Arc::new(RwLock::new(None)),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 记录配置文件加载路径（由 ConfigLoader::load 注入），并以当时磁盘
    /// 内容作为指纹基线
    pub fn set_path(&self, path: PathBuf) {
        *self
            .disk_fingerprint
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = file_fingerprint(&path);
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
    /// 桥接的候选配置来自磁盘当前内容，故提交后刷新指纹基线——后续写回不算冲突
    pub fn swap_memory(&self, new_config: Config) -> Result<(), GateError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        validate_config(&new_config)?;
        self.config.update(new_config);
        if let Some(path) = self.path() {
            if let Some(fingerprint) = file_fingerprint(&path) {
                *self
                    .disk_fingerprint
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fingerprint);
            }
        }
        Ok(())
    }

    /// 采纳磁盘现值作为指纹基线（热重载桥接对「语义等价候选」调用）：
    /// 外部编辑已被解析且无需切换内存态时，基线须跟进——否则后续管理面
    /// 写回一律被误判为进程外编辑，冻结到重启。持写锁，防止与写回的
    /// 读基线-比对-刷基线序列交错产生陈旧基线；无路径（纯内存构造）no-op。
    pub fn adopt_disk_baseline(&self) {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(path) = self.path() {
            if let Some(fingerprint) = file_fingerprint(&path) {
                *self
                    .disk_fingerprint
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fingerprint);
            }
        }
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
        let observed = *self
            .disk_fingerprint
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if file_fingerprint(&path) != observed {
            return Err(GateError::Config {
                message: t_args(
                    "config-disk-modified-refusing-write",
                    &[("path", path.display().to_string())],
                ),
            });
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

        std::fs::write(&tmp_path, content).map_err(|e| GateError::Config {
            message: t_args(
                "config-tmp-write-failed",
                &[
                    ("path", tmp_path.display().to_string()),
                    ("error", e.to_string()),
                ],
            ),
        })?;
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
        if let Some(fingerprint) = file_fingerprint(&path) {
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
    /// `X-Vendor` 头）；命中返回配置原文。配置中存在仅大小写不同的多
    /// vendor 时取声明序首个（配置校验按精确 id 去重，未禁止该写法）。
    /// 管理/内部按精确 id 的调用方仍用 [`Self::get_vendor`]。
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
        manager.adopt_disk_baseline();
        manager
            .update_config(sample_config("v2"))
            .expect("采纳基线后写回必须恢复");
        assert!(manager.get_vendor("v2").is_some(), "写回已生效");

        // no-op 分支：纯内存构造（无路径）不 panic
        ConfigManager::new(sample_config("v3")).adopt_disk_baseline();
    }

    /// 并发 update 一致性（评审 major-1 场景）：多线程并发 POST 等价的
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

        // 缺段 → None（不监听 gRPC 端口，行为与历史逐位一致）
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
