// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 配置热重载：`ConfigManager` 旁挂 confers
//! `HotReloader`（watch + progressive-reload），监听配置文件变更。
//!
//! 生效语义（保守白名单）：
//!
//! - 热生效：每请求现读的段——retry/routing/cache/resilience/admission/
//!   auth/pricing/model_catalog（读路径见各段 `config_manager.read()` 调用处）；
//! - 拒绝（`ReloadRejected`，保旧快照继续服务）：构造期绑定、拓扑段或
//!   非热读段——vendors（实例拓扑）/server（监听绑定）/database（连接池）/
//!   grpc/cors（建站装配）/rate_limit（Router 构造期建表）/docs（文档路由
//!   建站挂载）/global_model（无每请求热读点，接受即静默空转），改动需
//!   重启进程生效；
//! - 解析失败同样保旧（loader Err → 失败计数 + 日志，当前配置不动）。
//!
//! 回路安全：admin 通道写文件（`update_config` 落盘）同样触发 watcher 事件，
//! 但内容与内存一致时桥接跳过交换（`PartialEq` 比对），不形成写回环；
//! 不启用 confers env feature（无 .env 自动加载）。

use std::sync::Arc;

use confers::interface::ConfigProvider;
use confers::types::AnnotatedValue;
use confers::watcher::{
    FsWatcher, HotReloadLoader, HotReloader, PreCommitCheck, ProgressiveReloader, ReloadStrategy,
};

use crate::config::{Config, ConfigManager};
use crate::i18n::t_args;

/// 热重载监听去抖（毫秒）：原子 rename 写穿透一次事件；编辑器多次保存
/// 合并为一次重载。
const HOT_RELOAD_DEBOUNCE_MS: u64 = 500;

/// 结构门：构造期绑定/拓扑段变化一律拒绝（旧快照继续服务）。
struct StructuralGate {
    manager: Arc<ConfigManager>,
}

#[async_trait::async_trait]
impl PreCommitCheck<Config> for StructuralGate {
    async fn check(&self, candidate: &Config) -> Result<(), String> {
        let current = self.manager.read();
        if current.vendors != candidate.vendors {
            return Err("vendors topology changed (restart required)".to_string());
        }
        if current.server != candidate.server {
            return Err("server section changed (restart required)".to_string());
        }
        if current.database != candidate.database {
            return Err("database section changed (restart required)".to_string());
        }
        if current.grpc != candidate.grpc {
            return Err("grpc section changed (restart required)".to_string());
        }
        if current.cors != candidate.cors {
            return Err("cors section changed (restart required)".to_string());
        }
        if current.rate_limit != candidate.rate_limit {
            return Err("rate_limit rules changed (restart required)".to_string());
        }
        if current.docs != candidate.docs {
            return Err("docs switch changed (router rebuild required)".to_string());
        }
        if current.global_model != candidate.global_model {
            return Err("global_model changed (no hot-read point; restart required)".to_string());
        }
        Ok(())
    }
}

/// 空 provider（Immediate 策略无 canary 健康检查，provider 仅占位）。
#[derive(Debug, Default)]
struct EmptyProvider;

impl ConfigProvider for EmptyProvider {
    fn get_raw(&self, _key: &str) -> Option<&AnnotatedValue> {
        None
    }

    fn keys(&self) -> Vec<String> {
        Vec::new()
    }
}

/// 变更段名（日志留痕用，只记段名不记值——值可能是密钥）。
fn changed_sections(old: &Config, new: &Config) -> Vec<&'static str> {
    let mut sections = Vec::new();
    if old.retry != new.retry {
        sections.push("retry");
    }
    if old.routing != new.routing {
        sections.push("routing");
    }
    if old.cache != new.cache {
        sections.push("cache");
    }
    if old.resilience != new.resilience {
        sections.push("resilience");
    }
    if old.admission != new.admission {
        sections.push("admission");
    }
    if old.auth != new.auth {
        sections.push("auth");
    }
    if old.pricing != new.pricing {
        sections.push("pricing");
    }
    if old.model_catalog != new.model_catalog {
        sections.push("model_catalog");
    }
    // 以下段变更走拒绝路径（保旧），正常提交中不应出现；列出以便日志完备
    if old.vendors != new.vendors {
        sections.push("vendors");
    }
    if old.server != new.server {
        sections.push("server");
    }
    if old.database != new.database {
        sections.push("database");
    }
    if old.grpc != new.grpc {
        sections.push("grpc");
    }
    if old.cors != new.cors {
        sections.push("cors");
    }
    if old.rate_limit != new.rate_limit {
        sections.push("rate_limit");
    }
    if old.docs != new.docs {
        sections.push("docs");
    }
    sections
}

/// 启动热重载（装配期一次性）：无配置文件路径（纯内存构造，如测试）或
/// 监听建立失败时返回 `None`（热重载静默缺席，现有行为不变）。
pub(crate) async fn maybe_start_hot_reload(
    manager: &Arc<ConfigManager>,
) -> Option<HotReloader<Config>> {
    let path = manager.path()?;
    if !path.exists() {
        return None;
    }

    let reloader = ProgressiveReloader::new(manager.read(), ReloadStrategy::Immediate)
        .with_pre_commit_check(Arc::new(StructuralGate {
            manager: manager.clone(),
        }));
    // 监听父目录而非单文件——admin 通道 tmp+rename
    // 原子写盘会换 inode，单文件监听在首次 rename 后永久失明；目录监听
    // 下目录内杂项事件（tmp 文件、config-backups 快照）触发的冗余重载
    // 由“内容一致跳过”终结，无副作用。
    let watch_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let watcher = FsWatcher::new(&watch_dir, HOT_RELOAD_DEBOUNCE_MS)
        .await
        .ok()?;
    if !watcher.is_running() {
        tracing::warn!("{}", t_args("main-hot-reload-watcher-unavailable", &[]));
        return None;
    }

    let load_path = path.clone();
    let load: HotReloadLoader<Config> =
        Arc::new(
            move || match crate::config::loader::load_config_from_path(&load_path) {
                Ok(config) => Ok((
                    Arc::new(config),
                    Arc::new(EmptyProvider) as Arc<dyn ConfigProvider>,
                )),
                Err(e) => Err(confers::error::ConfigError::ParseError {
                    format: "toml".to_string(),
                    message: e.to_string(),
                    location: None,
                    source: None,
                }),
            },
        );

    let reloader_handle = HotReloader::spawn(reloader, watcher, load);

    // 桥接：已提交配置 → ConfigManager 内存态（只切内存，不落盘）。
    // 内容一致时跳过（admin 写盘触发的自有事件在此处终结，不成环）。
    let mut rx = reloader_handle.subscribe();
    let bridge_manager = manager.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let next = rx.borrow().clone();
            if *next == *bridge_manager.read() {
                continue;
            }
            let sections = changed_sections(&bridge_manager.read(), &next);
            match bridge_manager.swap_memory((*next).clone()) {
                Ok(()) => {
                    tracing::info!(
                        "{}",
                        t_args(
                            "main-hot-reload-committed",
                            &[("sections", format!("{sections:?}"))],
                        )
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "{}",
                        t_args("main-hot-reload-swap-failed", &[("error", e.to_string())],)
                    );
                }
            }
        }
    });

    // 路径来自启动参数，过滤控制字符再入日志，防伪造日志行
    let watching_path: String = path
        .display()
        .to_string()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    tracing::info!(
        "{}",
        t_args("main-hot-reload-watching", &[("path", watching_path)],)
    );
    Some(reloader_handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config() -> Config {
        // 复用现有单测的最小合法形态：空 vendors + 默认段（validate 通过，
        // 无 auth 段故无弱密钥校验）。
        Config {
            server: crate::config::ServerConfig {
                host: "127.0.0.1".into(),
                port: 8080,
            },
            database: crate::config::DatabaseConfig {
                path: ":memory:".into(),
                pool_size: 1,
                url: String::new(),
            },
            vendors: vec![],
            global_model: None,
            pricing: vec![],
            model_catalog: Vec::new(),
            auth: None,
            cors: None,
            retry: Default::default(),
            routing: Default::default(),
            cache: Default::default(),
            resilience: Default::default(),
            docs: Default::default(),
            admission: Default::default(),
            grpc: None,
            rate_limit: Default::default(),
        }
    }

    /// 结构门：热段放行，拓扑/构造段拒绝。
    #[tokio::test]
    async fn structural_gate_allows_hot_fields_rejects_topology() {
        let manager = Arc::new(ConfigManager::new(minimal_config()));
        let gate = StructuralGate {
            manager: manager.clone(),
        };

        // 热字段变更放行
        let mut hot = minimal_config();
        hot.retry.max_retries = minimal_config().retry.max_retries + 1;
        hot.cache.ttl_secs = 600;
        gate.check(&hot).await.expect("热字段必须放行");

        // 拓扑/构造段变更拒绝
        let mut topo = minimal_config();
        topo.vendors.push(crate::config::VendorConfig {
            id: "new-vendor".to_string(),
            name: "V".to_string(),
            type_: "openai".to_string(),
            api_key_env: "K".to_string(),
            api_key_envs: Vec::new(),
            api_base: String::new(),
            enabled: true,
            priority: 1,
            models: Vec::new(),
            rate_limit: crate::config::RateLimitConfig::Token {
                max_tokens: 1,
                window_hours: Some(24),
                sliding: false,
            },
            concurrency: Default::default(),
            timeout_ms: 0,
            stream_ttft_ms: 0,
            stream_idle_ms: 0,
        });
        assert!(gate.check(&topo).await.is_err(), "vendor 增删必须拒绝");

        let mut server = minimal_config();
        server.server.port = 9999;
        assert!(gate.check(&server).await.is_err(), "server 变更必须拒绝");

        let mut limits = minimal_config();
        limits.rate_limit.rules.push(crate::config::RateLimitRule {
            id: "r".into(),
            path: None,
            per_tenant: true,
            window_secs: 60,
            max_requests: 1,
        });
        assert!(
            gate.check(&limits).await.is_err(),
            "rate_limit 变更必须拒绝"
        );

        let mut gm = minimal_config();
        gm.global_model = Some(crate::config::GlobalModelConfig {
            enabled: true,
            strategy: "other".to_string(),
        });
        assert!(
            gate.check(&gm).await.is_err(),
            "global_model 无热读点，必须拒绝（converge 缺口）"
        );
    }

    /// watcher 本体探针：文件改写必须产生事件（环境 inotify 可用性门禁）。
    #[tokio::test]
    async fn fs_watcher_delivers_modify_events() {
        use confers::watcher::FsWatcher;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("w.toml");
        std::fs::write(&path, "a = 1\n").expect("seed");
        let mut watcher = FsWatcher::new(&path, 100).await.expect("watch");
        assert!(watcher.is_running());
        // 让监听线程就绪
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        std::fs::write(&path, "a = 2\n").expect("rewrite");
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), watcher.recv())
            .await
            .expect("5s 内必须收到事件");
        assert!(event.is_some(), "改写必须产生事件");
    }

    /// 热字段修改运行时生效 + 结构变更拒绝且服务不中断（真实文件监听）。
    #[tokio::test]
    async fn hot_fields_apply_structural_changes_rejected() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("config.toml");
        let base = r#"
[server]
host = "127.0.0.1"
port = 8080

[database]
path = ":memory:"
pool_size = 1

[retry]
max_retries = 3

[[vendors]]
id = "v"
name = "V"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "token", max_tokens = 1 }
concurrency = { requests_per_second = 1 }
"#;
        std::fs::write(&path, base).expect("seed config");
        let manager = Arc::new(ConfigManager::new(
            crate::config::loader::load_config_from_path(&path).expect("parse"),
        ));
        manager.set_path(path.clone());
        assert_eq!(manager.read().retry.max_retries, 3);

        let reloader = maybe_start_hot_reload(&manager)
            .await
            .expect("watcher must start");
        assert_eq!(reloader.reload_failures(), 0);
        // 监听线程建连是异步的：首写前等待就绪，否则事件可能落在
        // watch 生效之前（生产常驻监听不受影响，仅测试时序）。
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // 热字段：max_retries 3 → 9，运行时生效
        std::fs::write(&path, base.replace("max_retries = 3", "max_retries = 9")).expect("rewrite");
        let mut applied = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if manager.read().retry.max_retries == 9 {
                applied = true;
                break;
            }
        }
        assert!(
            applied,
            "热字段修改必须在运行时生效（failures={}，current_max_retries={}）",
            reloader.reload_failures(),
            manager.read().retry.max_retries,
        );

        // 结构变更：新增 vendor → 拒绝，旧快照继续服务
        let structural = format!(
            "{}\n[[vendors]]\nid = \"v2\"\nname = \"V2\"\ntype_ = \"openai\"\napi_key_env = \"T011_TEST_KEY\"\nrate_limit = {{ type = \"token\", max_tokens = 1 }}\nconcurrency = {{ requests_per_second = 1 }}\n",
            base.replace("max_retries = 3", "max_retries = 9")
        );
        std::fs::write(&path, structural).expect("rewrite vendors");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert_eq!(
            manager.read().vendors.len(),
            1,
            "结构变更必须保旧快照（vendors 仍为初始 1 条）"
        );
        assert!(reloader.reload_failures() >= 1, "拒绝必须计入失败数");
        assert_eq!(
            manager.read().retry.max_retries,
            9,
            "拒绝后热字段旧值不受影响"
        );
    }
}
