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
    let saved = confers::loader::parse_toml(&content, SourceId::from("llm-gate-config"), None)
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

/// 配置管理器 — 内存热读态由 confers::dynamic::DynamicField 承载
/// （arc-swap 无锁读，O(1) get_ref；update 触发 on_change 回调），
/// 文件写入为同目录唯一临时文件 + rename 的原子替换。
///
/// 写路径互斥（write_lock）：validate→序列化→tmp→rename→内存切换全程
/// 串行，消除 axum handler 并发 POST /admin/config 的交错——无锁时
/// 「A 写 tmp → B 覆盖 tmp → A rename 落盘 B 的内容 → B rename ENOENT」
/// 会导致磁盘与内存态分裂。read-modify-write 场景使用 update_config_with
/// 在同一锁内完成，杜绝丢失更新。
#[derive(Clone)]
pub struct ConfigManager {
    config: Arc<DynamicField<Config>>,
    /// 启动加载路径（ConfigLoader::load 时注入）；update_config 写回同一路径。
    /// None = 纯内存构造（测试场景），update_config 拒绝落盘。
    path: Arc<RwLock<Option<PathBuf>>>,
    /// 写路径互斥（std Mutex：无 .await 穿越临界区，跨线程可用）
    write_lock: Arc<Mutex<()>>,
}

impl ConfigManager {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(DynamicField::new(config)),
            path: Arc::new(RwLock::new(None)),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 记录配置文件加载路径（由 ConfigLoader::load 注入）
    pub fn set_path(&self, path: PathBuf) {
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

    /// 更新配置：校验 → 原子落盘 → 内存态切换（写锁全程持有）。
    ///
    /// 顺序即事务语义：校验/落盘任一失败时内存态不变、磁盘文件不变；
    /// 全部成功才切换内存态（DynamicField::update，触发 on_change 回调）。
    /// 并发调用在本管理器内串行化，最终磁盘与内存一致（都为最后完成者）。
    pub fn update_config(&self, new_config: Config) -> Result<(), GateError> {
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.persist_and_swap(new_config)
    }

    /// 原子 read-modify-write：在写锁内读当前配置 → 应用闭包 → 校验落盘切换。
    ///
    /// 供 admin 的 update_vendor/update_model 等 RMW 端点使用，替代
    /// 「get_config → 修改 → update_config」的裸三段式（并发下丢失更新）。
    /// 闭包返回 Err 时整个更新中止，内存与磁盘均不变。
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

    /// 持锁前提下的公共尾部：校验 → 序列化 → 唯一 tmp → rename → 内存切换。
    /// 调用方必须已持有 write_lock。
    fn persist_and_swap(&self, new_config: Config) -> Result<(), GateError> {
        // 1. 共享校验（与加载路径同一条 validate_config）
        validate_config(&new_config)?;

        // 2. 序列化先行 — 序列化失败不触碰磁盘与内存
        let content = toml::to_string_pretty(&new_config).map_err(|e| GateError::Config {
            message: t_args("config-serialize-failed", &[("error", e.to_string())]),
        })?;

        // 3. 原子写：同目录唯一临时文件 + rename（POSIX 原子替换）。
        //    唯一名（pid+序号）避免固定 "config.toml.tmp" 被并发/多进程实例
        //    互踩：一方 rename 走后另一方拿到 ENOENT 或覆盖内容错乱。
        let path = self.path().ok_or(GateError::Config {
            message: t("config-path-unknown"),
        })?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
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

        // 4. 磁盘落定后才切换内存态（触发 on_change 回调）
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

    pub fn is_global_model_enabled(&self) -> bool {
        let config = self.read();
        config
            .global_model
            .as_ref()
            .map(|g| g.enabled)
            .unwrap_or(false)
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
            },
            vendors: vec![VendorConfig {
                id: vendor_id.to_string(),
                name: "Test".into(),
                type_: "openai".into(),
                api_key_env: "TEST_KEY".into(),
                api_key_envs: Vec::new(),
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
            auth: None,
            cors: None,
            retry: crate::config::RetrySection::default(),
            routing: crate::config::RoutingSection::default(),
            cache: crate::config::CacheSection::default(),
            admission: Default::default(),
            grpc: None,
            resilience: crate::config::ResilienceSection::default(),
        }
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
