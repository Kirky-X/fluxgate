// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! DI 容器（infrastructure::di）集成测试——覆盖 build_kit_with_config 的
//! 成功装配（含 minimal_flow_config 与 verify_kit 全能力解析）与 build
//! 失败的错误映射路径。

#![cfg(all(
    feature = "infrastructure",
    feature = "database",
    feature = "vendor-management",
    feature = "logging"
))]

#[cfg(test)]
mod di_container {
    use std::sync::Arc;

    use dbnexus::foundation::config::DbConfig;
    use dbnexus::integrations::kit::DbNexusModule;
    use fluxgate::di::{build_kit_with_config, verify_kit};
    use fluxgate::error::{FluxgateError, GateError};
    use inklog::integrations::kit::InklogModule;
    use limiteron::integrations::kit::LimiteronModule;
    use oxcache::integrations::kit::{OxcacheConfig, OxcacheModule};
    use trait_kit::prelude::*;

    /// 生产同源的最小合法 DbConfig（sqlite 内存库，测试无落盘）。
    fn memory_db_config() -> DbConfig {
        DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::foundation::config::PoolConfig {
                max_connections: 5,
                min_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// 完整装配：四模块注册 + 拓扑构建 + 四能力全部可解析（oxcache →
    /// dbnexus → inklog 链与 limiteron 独立链），verify_kit 收敛 Ok。
    #[tokio::test]
    async fn build_kit_with_config_resolves_all_capabilities() {
        let kit = build_kit_with_config(memory_db_config())
            .await
            .expect("DI kit 应成功构建");

        // 模块注册齐全
        assert!(kit.contains::<OxcacheModule>());
        assert!(kit.contains::<LimiteronModule>());
        assert!(kit.contains::<DbNexusModule>());
        assert!(kit.contains::<InklogModule>());

        // 配置注入齐全
        assert!(kit.contains_config::<OxcacheConfig>());

        // 四能力逐个解析（类型擦除能力面）
        let cache: Arc<dyn oxcache::backend::CacheBackend + Send + Sync> =
            kit.require::<OxcacheModule>().expect("OxcacheModule 能力");
        let governor: Arc<limiteron::Governor> = kit
            .require::<LimiteronModule>()
            .expect("LimiteronModule 能力");
        let pool: Arc<dyn dbnexus::database::pool::ConnectionPool + Send + Sync> =
            kit.require::<DbNexusModule>().expect("DbNexusModule 能力");
        let provider: Arc<dyn inklog::Database + Send + Sync> =
            kit.require::<InklogModule>().expect("InklogModule 能力");

        // 能力可用：池可取状态、限流器可出统计、缓存后端可读写、
        // inklog Database 适配器健康（证明 oxcache → dbnexus → inklog
        // 注入链完整可用）
        let _status = pool.status();
        let _governor_stats = governor.stats().await;
        assert!(
            provider.is_healthy().await,
            "inklog Database 经 DI 注入后应健康"
        );
        cache
            .set("di-probe".into(), Arc::new(b"v".to_vec()), None)
            .await
            .expect("缓存写入");
        let cached = cache.get("di-probe").await.expect("缓存读取");
        assert_eq!(
            cached.as_deref(),
            Some(b"v".as_slice()),
            "经 DI 构造的缓存后端可读写"
        );

        // verify_kit：全部能力解析收敛 Ok
        verify_kit(&kit).expect("verify_kit 应对完整 kit 收敛 Ok");
    }

    /// build 失败（数据库 URL 非法 → 池构建回调 Err）→ 映射为
    /// FluxgateError::Config 且消息带 "DI kit build failed" 前缀。
    #[tokio::test]
    async fn build_failure_maps_to_config_error() {
        let bad_config = DbConfig {
            url: "sqlite:///nonexistent-di-test-dir/db.sqlite?mode=rw".to_string(),
            pool_config: dbnexus::foundation::config::PoolConfig {
                max_connections: 2,
                min_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let err = build_kit_with_config(bad_config)
            .await
            .expect_err("非法 db url 必须让 build 失败");
        match err {
            FluxgateError::Gate(GateError::Config { message }) => {
                assert!(
                    message.starts_with("DI kit build failed"),
                    "错误消息必须带 build 失败前缀: {message}"
                );
            }
            other => panic!("必须映射为 FluxgateError::Gate(GateError::Config): {other:?}"),
        }
    }

    /// verify_kit 对空 kit（无模块）必须收敛 Err（能力缺失面）。
    #[tokio::test]
    async fn verify_kit_returns_err_for_empty_kit() {
        let kit = AsyncKit::new().build().await.expect("空 kit 可构建");
        let result = verify_kit(&kit);
        match result {
            Err(FluxgateError::Gate(GateError::Config { message })) => {
                assert!(
                    message.contains("DI require"),
                    "错误消息必须指明 require 失败: {message}"
                );
            }
            Ok(()) => panic!("空 kit 不得通过 verify_kit"),
            other => panic!("必须映射为 FluxgateError::Gate(GateError::Config): {other:?}"),
        }
    }
}
