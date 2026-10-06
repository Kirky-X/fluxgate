// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! G7 就绪探针（absorb-sibling-libs T008）：真实组件检查经 sdforge
//! `AsyncReadinessCheck` 注册，`/readyz` 聚合（200 ready / 503 unavailable）。
//! 检查只读既有句柄，不发起真实上游请求。现有 `#[forge]` /api/v1/health
//! 保留（存活探针），readyz 为就绪探针，两者语义分离。

use crate::api::state::ApiState;
use dbnexus::DbPool;

/// DB 池探针：取 admin 会话 + `SELECT 1` 往返。任一环节失败即 unhealthy。
/// details 只给固定码（G3 复核 MEDIUM：探针免认证，错误原文不外泄；
/// 原文进 tracing 日志供排障）。
pub(crate) async fn db_pool_check(pool: &DbPool) -> sdforge::health::CheckOutcome {
    match pool.get_session("admin").await {
        Ok(session) => match session.execute_raw("SELECT 1").await {
            Ok(_) => sdforge::health::CheckOutcome {
                name: "db-pool".to_string(),
                healthy: true,
                details: None,
            },
            Err(e) => {
                tracing::warn!("readiness db probe failed: {e}");
                sdforge::health::CheckOutcome {
                    name: "db-pool".to_string(),
                    healthy: false,
                    details: Some(serde_json::json!({"code": "probe-failed"})),
                }
            }
        },
        Err(e) => {
            tracing::warn!("readiness db session failed: {e}");
            sdforge::health::CheckOutcome {
                name: "db-pool".to_string(),
                healthy: false,
                details: Some(serde_json::json!({"code": "pool-unreachable"})),
            }
        }
    }
}

/// oxcache 后端探针：`health_check`（内存后端恒通过；远端后端失败即
/// unhealthy）。缓存未接线（None）视为 healthy（可选组件，不阻塞就绪）。
pub(crate) async fn cache_backend_check(
    cache: Option<std::sync::Arc<crate::api::response_cache::ResponseCache>>,
) -> sdforge::health::CheckOutcome {
    match cache {
        None => sdforge::health::CheckOutcome {
            name: "cache-backend".to_string(),
            healthy: true,
            details: Some(serde_json::json!({"wired": false})),
        },
        Some(c) => match c.probe_backend().await {
            Ok(()) => sdforge::health::CheckOutcome {
                name: "cache-backend".to_string(),
                healthy: true,
                details: Some(serde_json::json!({"wired": true})),
            },
            Err(e) => {
                tracing::warn!("readiness cache probe failed: {e}");
                sdforge::health::CheckOutcome {
                    name: "cache-backend".to_string(),
                    healthy: false,
                    details: Some(serde_json::json!({"code": "backend-unavailable"})),
                }
            }
        },
    }
}

/// vendor 管理器探针：读既有句柄（fleet 并发占用），不发起上游请求。
/// 读锁可得即 healthy（details carrying 容量信号供排障）。
/// 性质说明（G3 复核 MEDIUM A-M6）：这是 wiring 存活探针，不是健康门——
/// 空 vendors 系合法状态（测试/闲置网关），不判 degraded。
pub(crate) async fn vendor_manager_check(
    manager: &std::sync::Arc<crate::vendor::VendorManager>,
) -> sdforge::health::CheckOutcome {
    let (in_flight, capacity) = manager.fleet_concurrency();
    sdforge::health::CheckOutcome {
        name: "vendor-manager".to_string(),
        healthy: true,
        details: Some(serde_json::json!({"in_flight": in_flight, "capacity": capacity})),
    }
}

/// 注册全部就绪检查（bootstrap 装配期一次性调用；sdforge 注册表进程全局，
/// 重复注册仅追加同名条目——生产单进程单次装配）。
/// 进程级 Once 守卫：同进程二次装配（测试多 build_app）不再重复注册，
/// 避免 /readyz 重复执行同名检查（G3 复核 MEDIUM）。
pub fn register_readiness_checks(api_state: &ApiState, db_pool: &DbPool) {
    static REGISTERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if REGISTERED.set(()).is_err() {
        return;
    }
    let pool = db_pool.clone();
    sdforge::health::register_async_readiness_check_fn("db-pool", move || {
        let pool = pool.clone();
        async move { db_pool_check(&pool).await }
    });
    let cache = api_state.response_cache.clone();
    sdforge::health::register_async_readiness_check_fn("cache-backend", move || {
        let cache = cache.clone();
        async move { cache_backend_check(cache).await }
    });
    let manager = api_state.vendor_manager.clone();
    sdforge::health::register_async_readiness_check_fn("vendor-manager", move || {
        let manager = manager.clone();
        async move { vendor_manager_check(&manager).await }
    });
}
