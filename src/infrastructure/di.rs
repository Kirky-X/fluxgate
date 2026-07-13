// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Dependency Injection container built on trait-kit's `AsyncKit`.
//!
//! Assembles the four infrastructure modules into a single `AsyncKit<AsyncReady>`
//! at startup. The kit resolves modules in topological order and provides
//! type-erased capabilities (`CacheBackend`, `ConnectionPool`, `Governor`,
//! `LogDbProvider`) that downstream code can retrieve via `kit.require::<M>()`.
//!
//! # DI Chains
//!
//! 1. `oxcache` → `dbnexus` → `inklog` (cache backend → pool → log storage)
//! 2. `limiteron` (standalone governor)

use std::sync::Arc;

use dbnexus::integrations::kit::DbNexusModule;
use inklog::integrations::kit::InklogModule;
use limiteron::integrations::kit::LimiteronModule;
use oxcache::integrations::kit::{OxcacheConfig, OxcacheModule};
use trait_kit::prelude::*;

use crate::error::NexusFlowError;

/// Build a minimal valid `FlowControlConfig` (1 rule matching all IPs with a
/// permissive token bucket). `FlowControlConfig::default()` has an empty
/// rules vec which fails `validate()`.
fn minimal_flow_config() -> limiteron::config::FlowControlConfig {
    use limiteron::config::{
        Action, ActionConfig, FlowControlConfig, LimiterConfig, Matcher, Rule,
    };
    let mut config = FlowControlConfig::default();
    config.rules.push(Rule {
        id: "default".to_string(),
        name: "default rule".to_string(),
        priority: 0,
        matchers: vec![Matcher::Ip {
            ip_ranges: vec!["0.0.0.0/0".to_string()],
        }],
        limiters: vec![LimiterConfig::TokenBucket {
            capacity: 100,
            refill_rate: 10,
        }],
        action: ActionConfig {
            on_exceed: Action::Reject,
            ban: None,
        },
    });
    config
}

/// Construct a minimal in-memory `DbConfig` for the DI kit.
fn memory_db_config() -> dbnexus::foundation::config::DbConfig {
    dbnexus::foundation::config::DbConfig {
        url: "sqlite::memory:".to_string(),
        max_connections: 5,
        min_connections: 1,
        ..Default::default()
    }
}

/// Build the `AsyncKit` DI container with all four infrastructure modules.
///
/// Registers `OxcacheModule`, `LimiteronModule`, `DbNexusModule`, `InklogModule`
/// and resolves them in topological order. The returned `AsyncKit<AsyncReady>`
/// provides capabilities via `require::<M>()`.
///
/// # Errors
///
/// Returns `NexusFlowError::Config` if module registration or the topological
/// build fails (missing dependency, cycle, or a module `build()` callback
/// returning `Err`).
pub async fn build_kit() -> Result<AsyncKit<AsyncReady>, NexusFlowError> {
    let mut kit = AsyncKit::new();

    kit.set_config(OxcacheConfig::default());
    kit.set_config(minimal_flow_config());
    kit.set_config(memory_db_config());

    kit.register::<OxcacheModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI register OxcacheModule failed: {}", e),
        })?;
    kit.register::<LimiteronModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI register LimiteronModule failed: {}", e),
        })?;
    kit.register::<DbNexusModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI register DbNexusModule failed: {}", e),
        })?;
    kit.register::<InklogModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI register InklogModule failed: {}", e),
        })?;

    kit.build()
        .await
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI kit build failed: {}", e),
        })
}

/// Verify that all four module capabilities resolve successfully.
///
/// Call after `build_kit()` to confirm the topological sort produced every
/// expected capability. Returns `Ok(())` if all resolve, or the first
/// failing module's error.
///
/// # Errors
///
/// Returns `NexusFlowError::Config` if any capability is missing.
pub fn verify_kit(kit: &AsyncKit<AsyncReady>) -> Result<(), NexusFlowError> {
    let _cache: Arc<dyn oxcache::backend::CacheBackend + Send + Sync> =
        kit.require::<OxcacheModule>().map_err(|e| NexusFlowError::Config {
            message: format!("DI require OxcacheModule failed: {}", e),
        })?;
    let _governor: Arc<limiteron::Governor> = kit
        .require::<LimiteronModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI require LimiteronModule failed: {}", e),
        })?;
    let _pool: Arc<dyn dbnexus::database::pool::ConnectionPool + Send + Sync> =
        kit.require::<DbNexusModule>().map_err(|e| NexusFlowError::Config {
            message: format!("DI require DbNexusModule failed: {}", e),
        })?;
    let _provider: Arc<dyn inklog::LogDbProvider + Send + Sync> = kit
        .require::<InklogModule>()
        .map_err(|e| NexusFlowError::Config {
            message: format!("DI require InklogModule failed: {}", e),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_build_kit_resolves_all_four_modules() {
        let kit = build_kit().await.expect("DI kit should build");

        // DI chain 1: oxcache → dbnexus → inklog
        let cache: Arc<dyn oxcache::backend::CacheBackend + Send + Sync> = kit
            .require::<OxcacheModule>()
            .expect("OxcacheModule capability");
        assert!(!Arc::as_ptr(&cache).is_null());

        let pool: Arc<dyn dbnexus::database::pool::ConnectionPool + Send + Sync> = kit
            .require::<DbNexusModule>()
            .expect("DbNexusModule capability");
        assert!(!Arc::as_ptr(&pool).is_null());

        let provider: Arc<dyn inklog::LogDbProvider + Send + Sync> = kit
            .require::<InklogModule>()
            .expect("InklogModule capability");
        assert!(!Arc::as_ptr(&provider).is_null());

        // DI chain 2: limiteron (standalone)
        let governor: Arc<limiteron::Governor> = kit
            .require::<LimiteronModule>()
            .expect("LimiteronModule capability");
        assert!(!Arc::as_ptr(&governor).is_null());
    }

    #[tokio::test]
    async fn test_kit_contains_all_modules() {
        let kit = build_kit().await.expect("DI kit should build");

        assert!(kit.contains::<OxcacheModule>(), "contains OxcacheModule");
        assert!(
            kit.contains::<LimiteronModule>(),
            "contains LimiteronModule"
        );
        assert!(kit.contains::<DbNexusModule>(), "contains DbNexusModule");
        assert!(kit.contains::<InklogModule>(), "contains InklogModule");
    }

    #[tokio::test]
    async fn test_kit_contains_oxcache_config() {
        let kit = build_kit().await.expect("DI kit should build");

        assert!(
            kit.contains_config::<OxcacheConfig>(),
            "contains_config OxcacheConfig"
        );
    }

    #[tokio::test]
    async fn test_di_chain_inklog_executes_ddl() {
        // Proves the full oxcache → dbnexus → inklog chain: the ConnectionPool
        // from DbNexusModule (which itself depended on CacheBackend from
        // OxcacheModule) is successfully injected into InklogModule's
        // LogDbProvider.
        let kit = build_kit().await.expect("DI kit should build");

        let provider: Arc<dyn inklog::LogDbProvider + Send + Sync> = kit
            .require::<InklogModule>()
            .expect("InklogModule capability");

        provider
            .execute_log("CREATE TABLE IF NOT EXISTS logs (id INTEGER PRIMARY KEY)")
            .await
            .expect("execute_log should succeed on injected ConnectionPool");
    }

    #[tokio::test]
    async fn test_optional_returns_some_for_registered() {
        let kit = build_kit().await.expect("DI kit should build");

        assert!(kit.optional::<OxcacheModule>().is_some());
        assert!(kit.optional::<DbNexusModule>().is_some());
        assert!(kit.optional::<InklogModule>().is_some());
        assert!(kit.optional::<LimiteronModule>().is_some());
    }
}
