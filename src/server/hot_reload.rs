// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 配置热重载：监听文件变更，桥接进 `ConfigManager` 内存态（不落盘）。
//! 内容一致时跳过，不形成写回环；解析或结构校验失败保旧快照。
//! 热生效：retry/routing/cache/resilience/admission/auth/pricing/model_catalog。
//! 拓扑与构造期段（vendors/server/database/grpc/cors/rate_limit/docs/
//! global_model）变更被拒绝，需重启进程。

use std::sync::Arc;

use confers::interface::ConfigProvider;
use confers::types::AnnotatedValue;
use confers::watcher::{
    FsWatcher, HotReloadLoader, HotReloader, PreCommitCheck, ProgressiveReloader, ReloadStrategy,
};

use crate::config::{Config, ConfigManager};
use crate::i18n::t_args;

/// 去抖（毫秒）：原子 rename 穿透一次事件，多次保存合并为一次重载。
const HOT_RELOAD_DEBOUNCE_MS: u64 = 500;

/// 结构门：构造期绑定/拓扑段变化一律拒绝，旧快照继续服务。
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

/// 变更段名（日志只记段名不记值，值可能是密钥）。
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
    if old.quota != new.quota {
        sections.push("quota");
    }
    if old.health != new.health {
        sections.push("health");
    }
    // 以下段变更走拒绝路径，正常提交不出现；列出仅为日志完备
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
    if old.global_model != new.global_model {
        sections.push("global_model");
    }
    sections
}

/// 启动热重载（装配期一次性）；无配置文件路径或监听建立失败时返回 `None`。
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
    // 文件目标监听：库内部自动改挂父目录 + 精确路径过滤 + 出口去抖，
    // 换 inode 的原子替换与 delete+recreate 都不会失明，同时只转发该文件
    // 自身的事件（同目录 data/logs/config-backups 等旁路写盘不再触发重载）。
    let watcher = FsWatcher::new(&path, HOT_RELOAD_DEBOUNCE_MS).await.ok()?;
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

    // 桥接：已提交配置 → ConfigManager 内存态；内容一致时跳过。
    let mut rx = reloader_handle.subscribe();
    let bridge_manager = manager.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let next = rx.borrow().clone();
            if *next == *bridge_manager.read() {
                // 语义等价（如仅注释/格式化改动）：候选已解析且无需切换内存态
                //（不触发回调、不形成写回环）。基线采纳为验证式：重读并重解析
                // 同一份字节、再确认等价后才采纳——防两次外部写交错把未经
                // 解析验证的中间态采纳为基线（基线领先会让后续管理面写回
                // 静默覆盖该编辑）；未采纳则维持冻结，由后续事件或重启兜底。
                if !bridge_manager.adopt_disk_baseline() {
                    tracing::debug!("{}", t_args("main-hot-reload-baseline-not-adopted", &[]));
                }
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

    // 过滤控制字符再入日志，防伪造日志行
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
        // 最小合法配置：空 vendors + 默认段。
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
            lexcompress: Default::default(),
            admission: Default::default(),
            health: Default::default(),
            quota: Default::default(),
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
            supports_batch: false,
            chat_path: None,
            api_key_envs: Vec::new(),
            extra_headers: Default::default(),
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

    /// watcher 探针：文件改写必须产生事件（依赖 inotify 可用）。
    #[tokio::test]
    async fn fs_watcher_delivers_modify_events() {
        use confers::watcher::FsWatcher;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("w.toml");
        std::fs::write(&path, "a = 1\n").expect("seed");
        let mut watcher = FsWatcher::new(&path, 100).await.expect("watch");
        assert!(watcher.is_running());
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
        // 等待监听线程就绪，否则首写事件可能早于 watch 生效。
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

    /// 文件目标监听的两条契约（本次改动本体）：
    /// 1) 同目录兄弟文件与子目录（data/logs/config-backups 形态）的写盘
    ///    不再触发重载——事件源收敛为配置文件本身；
    /// 2) admin 落盘走 tmp+rename 原子替换，换 inode 后仍必须被捕获。
    #[tokio::test]
    async fn file_target_watch_ignores_sibling_noise_and_survives_atomic_replace() {
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

        let reloader = maybe_start_hot_reload(&manager)
            .await
            .expect("watcher must start");
        let reload_events = reloader.subscribe();
        // 等待监听线程就绪，否则首写事件可能早于 watch 生效。
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // 旁路噪声：同目录兄弟文件 + 子目录写盘（等 ≥3 个去抖窗口）
        std::fs::write(dir.path().join("sibling.toml"), "noise = 1\n").expect("sibling noise");
        std::fs::create_dir_all(dir.path().join("data")).expect("mkdir data");
        std::fs::write(dir.path().join("data/app.db"), b"noise").expect("subdir noise");
        tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
        assert!(
            !reload_events.has_changed().expect("reload channel alive"),
            "旁路写盘不得触发重载（事件源必须收敛为配置文件本身）"
        );
        assert_eq!(
            reloader.reload_failures(),
            0,
            "旁路写盘不得计失败（否则说明仍在对该盘事件做全量解析）"
        );
        assert_eq!(manager.read().retry.max_retries, 3);

        // 原子替换：同目录 tmp + rename 覆盖配置文件（admin 落盘路径）
        let tmp = dir.path().join("config.toml.tmp");
        std::fs::write(&tmp, base.replace("max_retries = 3", "max_retries = 9"))
            .expect("tmp write");
        std::fs::rename(&tmp, &path).expect("atomic replace");

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
            "rename 原子替换必须仍被捕获（failures={}，current_max_retries={}）",
            reloader.reload_failures(),
            manager.read().retry.max_retries,
        );
    }

    /// 注释级外部编辑（语义等价）不得冻结管理面写回：桥接必须采纳基线
    ///（真实文件监听 + 桥接全链路；修复前写回持续被拒到重启）。
    #[tokio::test]
    async fn comment_only_external_edit_keeps_write_back_available() {
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

        let reloader = maybe_start_hot_reload(&manager)
            .await
            .expect("watcher must start");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // 注释级外部编辑：字节变、解析结果 == 内存态（桥接走等价分支）
        std::fs::write(&path, format!("# 外部注释\n{base}")).expect("comment-only edit");

        // 桥接收纳后（去抖 500ms）管理面写回必须恢复，不得冻结至重启
        let mut next = crate::config::loader::load_config_from_path(&path).expect("re-parse");
        assert_eq!(next.retry.max_retries, 3, "注释编辑不改语义");
        next.retry.max_retries = 20;
        // 前置保护断言：基线采纳前写回必须仍被外部编辑保护拒绝
        assert!(
            manager.update_config(next.clone()).is_err(),
            "采纳前写回必须被外部编辑保护拒绝"
        );
        let mut recovered = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if manager.update_config(next.clone()).is_ok() {
                recovered = true;
                break;
            }
        }
        assert!(
            recovered,
            "注释级外部编辑后写回必须在窗口内恢复（failures={}）",
            reloader.reload_failures()
        );
        assert_eq!(manager.read().retry.max_retries, 20, "写回已生效");
    }

    // ---- changed_sections 全段名覆盖（retry 臂由既有热更用例覆盖）----

    #[test]
    fn changed_sections_lists_each_changed_segment() {
        let base = minimal_config();
        assert!(
            changed_sections(&base, &minimal_config()).is_empty(),
            "全同 → 空列表"
        );

        type SectionMutation = (&'static str, Box<dyn Fn(&mut Config)>);
        let mutations: Vec<SectionMutation> = vec![
            (
                "routing",
                Box::new(|c: &mut Config| c.routing.difficulty.enabled = true),
            ),
            ("cache", Box::new(|c: &mut Config| c.cache.ttl_secs += 1)),
            (
                "resilience",
                Box::new(|c: &mut Config| c.resilience.cover_all_paths = true),
            ),
            (
                "admission",
                Box::new(|c: &mut Config| c.admission.enabled = true),
            ),
            (
                "auth",
                Box::new(|c: &mut Config| {
                    c.auth = Some(crate::config::AuthSection {
                        enabled: false,
                        api_keys: vec![],
                        header: Default::default(),
                        keys: vec![],
                    });
                }),
            ),
            (
                "pricing",
                Box::new(|c: &mut Config| {
                    c.pricing.push(crate::config::PricingConfig {
                        vendor_id: "v".into(),
                        model_id: "m".into(),
                        input_per_1k_tokens: 1.0,
                        output_per_1k_tokens: 2.0,
                    });
                }),
            ),
            (
                "model_catalog",
                Box::new(|c: &mut Config| {
                    c.model_catalog.push(crate::config::ModelCatalogEntry {
                        model_id: "m".into(),
                        ..Default::default()
                    });
                }),
            ),
            ("quota", Box::new(|c: &mut Config| c.quota.enabled = true)),
            (
                "health",
                Box::new(|c: &mut Config| c.health.mode = "all".into()),
            ),
            ("server", Box::new(|c: &mut Config| c.server.port = 9999)),
            (
                "database",
                Box::new(|c: &mut Config| c.database.pool_size = 2),
            ),
            (
                "grpc",
                Box::new(|c: &mut Config| c.grpc = Some(Default::default())),
            ),
            (
                "cors",
                Box::new(|c: &mut Config| {
                    c.cors = Some(crate::config::CorsSection {
                        allowed_origins: vec!["https://x".into()],
                        ..Default::default()
                    });
                }),
            ),
            (
                "rate_limit",
                Box::new(|c: &mut Config| {
                    c.rate_limit.rules.push(crate::config::RateLimitRule {
                        id: "r".into(),
                        path: None,
                        per_tenant: true,
                        window_secs: 60,
                        max_requests: 1,
                    });
                }),
            ),
            ("docs", Box::new(|c: &mut Config| c.docs.enabled = true)),
            (
                "global_model",
                Box::new(|c: &mut Config| {
                    c.global_model = Some(crate::config::GlobalModelConfig {
                        enabled: true,
                        strategy: "other".into(),
                    });
                }),
            ),
        ];
        for (name, mutate) in mutations {
            let mut changed = minimal_config();
            mutate(&mut changed);
            assert_eq!(
                changed_sections(&base, &changed),
                vec![name],
                "单段差异必须只报该段名"
            );
        }
    }

    #[test]
    fn empty_provider_serves_no_entries() {
        let provider = EmptyProvider;
        assert!(ConfigProvider::get_raw(&provider, "any").is_none());
        assert!(ConfigProvider::keys(&provider).is_empty());
    }

    // ---- maybe_start_hot_reload 启动门控与失败重载 ----

    fn reload_toml(max_retries: u32) -> String {
        format!(
            "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n\
             [database]\npath = \":memory:\"\n\n[retry]\nmax_retries = {max_retries}\n"
        )
    }

    #[tokio::test]
    async fn maybe_start_hot_reload_needs_existing_file() {
        let manager = Arc::new(ConfigManager::new(minimal_config()));
        assert!(
            maybe_start_hot_reload(&manager).await.is_none(),
            "无路径不启动"
        );
        manager.set_path(std::path::PathBuf::from("/nonexistent/cfg/fluxgate.toml"));
        assert!(
            maybe_start_hot_reload(&manager).await.is_none(),
            "文件不存在不启动"
        );
    }

    #[tokio::test]
    async fn invalid_rewrite_records_reload_failure_and_keeps_memory() {
        let dir = tempfile::tempdir().expect("tempdir creatable");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, reload_toml(2)).expect("write initial config");
        let content = std::fs::read_to_string(&path).expect("read initial config");
        let config =
            crate::config::loader::parse_config_content(&path, &content).expect("初始配置必须合法");
        let manager = Arc::new(ConfigManager::new(config));
        manager.set_path(path.clone());
        let reloader = maybe_start_hot_reload(&manager)
            .await
            .expect("有文件必须启动");
        // 等待监听线程就绪，否则首写事件可能早于 watch 生效
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        std::fs::write(&path, "not [ valid toml").expect("rewrite invalid config");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while reloader.reload_failures() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "5s 内非法改写未被记为重载失败"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            manager.read().retry.max_retries,
            2,
            "重载失败时内存态保持旧值"
        );
    }
}
