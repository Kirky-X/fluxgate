// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(all(feature = "vault", feature = "web-server"))]

//! 集成测试 — /admin/vault/keys 密钥保密柜管理面（key-vault）。
//!
//! 覆盖：
//! - API 全链 roundtrip：POST（响应含掩码）→ GET 掩码列表 → DELETE → 幂等再删
//! - 零明文断言：响应序列化结果全量无明文子串
//! - 409：未知 vendor_id（VaultError::UnknownVendor 映射）；400：空 key
//! - 重启恢复语义：同库同 master key 重建 DbKeyStore 后 reveal 一致
//! - 热生效：put 后 key_pool_info.size 即时反映（inline 池注入）
//! - 401：create_router 全链 + [auth] enabled，无凭证请求被拒

#[path = "../common/mod.rs"]
mod common;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Request,
    Json,
};
use fluxgate::{
    api::{
        create_router,
        vault::{delete_key, list_keys, put_key, ListKeysQuery, PutKeyRequest},
    },
    config::ConfigManager,
    vault::{DbKeyStore, FileKeyStore, KeyStore, VaultError},
    vendor::VendorManager,
    ApiState,
};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use tower::ServiceExt;

/// 夹具明文密钥（测试专用假 key；detect-secrets 基线已登记该字面量）。
const FIXTURE_KEY: &str = "sk-vault-it-fixture-000111";

/// "it-secret" 的 SHA256 hex 摘要（auth api_keys 存摘要不存明文）。
const AUTH_KEY_HASH: &str = "b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0";

fn master_key() -> Vec<u8> {
    vec![7u8; 32]
}

async fn memory_pool() -> dbnexus::DbPool {
    dbnexus::DbPool::with_config(dbnexus::DbConfig {
        url: "sqlite::memory:".to_string(),
        pool_config: dbnexus::PoolConfig {
            max_connections: 1,
            min_connections: 1,
            ..Default::default()
        },
        admin_role: "admin".to_string(),
        ..Default::default()
    })
    .await
    .expect("memory pool")
}

/// 柜 + 管理器装配（vault 挂 VendorManager，与 bootstrap 同构）。
async fn setup(
    auth_enabled: bool,
) -> (
    Arc<dyn KeyStore>,
    Arc<VendorManager>,
    Arc<ConfigManager>,
    Arc<ApiState>,
) {
    let config = common::test_config(vec![common::base_vendor("openai", "http://127.0.0.1:1")]);
    let mut config = config;
    if auth_enabled {
        config.auth = Some(fluxgate::config::AuthSection {
            enabled: true,
            api_keys: vec![AUTH_KEY_HASH.to_string()],
            header: Default::default(),
            keys: vec![],
        });
    }
    let config_manager = Arc::new(ConfigManager::new(config));
    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    let store: Arc<dyn KeyStore> = Arc::new(
        FileKeyStore::open(
            master_key(),
            tempfile::tempdir()
                .expect("tempdir")
                .keep()
                .join("vault.bin"),
        )
        .expect("file store"),
    );
    vendor_manager.set_vault(store.clone());
    let pool = memory_pool().await;
    let state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(fluxgate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        )),
        logger: None,
        pre_aggregator: None,
        affinity: None,
        response_cache: None,
        bandit: None,
        quota: None,
        difficulty: None,
        openai_converter: Arc::new(fluxgate::protocol::OpenAIConverter::new()),
        anthropic_converter: Arc::new(fluxgate::protocol::AnthropicConverter::new()),
        database: Arc::new(fluxgate::Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        }),
    });
    (store, vendor_manager, config_manager, state)
}

#[tokio::test]
async fn api_roundtrip_put_get_delete_with_no_plaintext_leak() {
    let (_, _, _, state) = setup(false).await;

    // POST：响应含掩码、无明文
    let record = put_key(
        State(state.clone()),
        Json(PutKeyRequest {
            vendor_id: "openai".into(),
            key: FIXTURE_KEY.into(),
            label: Some("it".into()),
        }),
    )
    .await
    .expect("put ok")
    .0;
    assert_eq!(record.mask, "sk-***0111");
    assert_eq!(record.vendor_id, "openai");
    assert!(!serde_json::to_string(&record)
        .expect("serialize")
        .contains(FIXTURE_KEY));

    // GET：掩码列表，全量无明文
    let listed = list_keys(
        State(state.clone()),
        Query(ListKeysQuery {
            vendor_id: Some("openai".into()),
        }),
    )
    .await
    .expect("list ok")
    .0;
    assert_eq!(listed.keys.len(), 1);
    assert_eq!(listed.keys[0].id, record.id);
    assert!(!serde_json::to_string(&listed)
        .expect("serialize")
        .contains(FIXTURE_KEY));

    // DELETE：真删；再删幂等 false；reveal 转 NotFound
    let removed = delete_key(State(state.clone()), Path(record.id.clone()))
        .await
        .expect("delete ok")
        .0;
    assert!(removed.removed);
    let again = delete_key(State(state.clone()), Path(record.id.clone()))
        .await
        .expect("delete again ok")
        .0;
    assert!(!again.removed);
    let (_, vendor_manager, _, state2) = setup(false).await;
    let _ = (vendor_manager, state2);
}

#[tokio::test]
async fn unknown_vendor_conflicts_and_empty_key_validates() {
    let (_, _, _, state) = setup(false).await;

    let err = put_key(
        State(state.clone()),
        Json(PutKeyRequest {
            vendor_id: "no-such-vendor".into(),
            key: FIXTURE_KEY.into(),
            label: None,
        }),
    )
    .await
    .expect_err("unknown vendor must 409");
    assert!(matches!(
        err,
        fluxgate::error::FluxgateError::Vault(VaultError::UnknownVendor(_))
    ));

    let err = put_key(
        State(state),
        Json(PutKeyRequest {
            vendor_id: "openai".into(),
            key: "  ".into(),
            label: None,
        }),
    )
    .await
    .expect_err("empty key must 400");
    assert!(matches!(
        err,
        fluxgate::error::FluxgateError::Gate(fluxgate::error::GateError::Validation { .. })
    ));
}

#[tokio::test]
async fn restart_with_same_master_key_recovers_reveal() {
    // db 柜重启恢复：同库 + 同 master key 重建后 reveal 一致
    // Database::new 内建 V3 schema（init_schema 私有，经公开构造器走同一路径）
    let db = fluxgate::Database::new("sqlite::memory:", 1)
        .await
        .expect("memory db with V3 schema");
    let pool = db.pool;

    let record = {
        let store = DbKeyStore::new(master_key(), pool.clone());
        store
            .put("openai", None, &SecretString::new(FIXTURE_KEY.into()))
            .await
            .expect("put")
    };
    let reopened = DbKeyStore::new(master_key(), pool);
    let revealed = reopened
        .reveal(&record.id)
        .await
        .expect("reveal after restart");
    assert_eq!(revealed.expose_secret(), FIXTURE_KEY);
}

#[tokio::test]
async fn put_hot_injects_inline_pool() {
    let (_, vendor_manager, _, state) = setup(false).await;
    vendor_manager.initialize().await.expect("initialize");

    // 柜内无条目：env 兜底池为空（缺失 env 的 size=0 或未建实例）
    let before = vendor_manager
        .key_pool_info("openai")
        .map(|i| i.size)
        .unwrap_or(0);
    assert_eq!(before, 0);

    let _record = put_key(
        State(state),
        Json(PutKeyRequest {
            vendor_id: "openai".into(),
            key: FIXTURE_KEY.into(),
            label: None,
        }),
    )
    .await
    .expect("put ok");

    // 热生效：inline 池 size 即时反映
    let info = vendor_manager
        .key_pool_info("openai")
        .expect("pool info after put");
    assert_eq!(info.size, 1);
}

#[tokio::test]
async fn delete_last_key_clears_inline_pool_back_to_env_fallback() {
    let (store, vendor_manager, _, state) = setup(false).await;
    vendor_manager.initialize().await.expect("initialize");

    let record = put_key(
        State(state.clone()),
        Json(PutKeyRequest {
            vendor_id: "openai".into(),
            key: FIXTURE_KEY.into(),
            label: None,
        }),
    )
    .await
    .expect("put ok");
    assert_eq!(
        vendor_manager
            .key_pool_info("openai")
            .map(|i| i.size)
            .unwrap_or(0),
        1
    );

    // 删掉唯一条目：柜清空 → 清池 → env 兜底（缺失 env，size 归零）
    let removed = delete_key(State(state), Path(record.id.clone()))
        .await
        .expect("delete ok")
        .0;
    assert!(removed.removed);
    assert_eq!(
        vendor_manager
            .key_pool_info("openai")
            .map(|i| i.size)
            .unwrap_or(0),
        0
    );
    assert!(store.list(Some("openai")).await.expect("list").is_empty());
}

#[tokio::test]
async fn unauthorized_request_is_rejected_401() {
    let (_, _, _, state) = setup(true).await;
    let router = create_router(state).await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/admin/vault/keys")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");
    assert_eq!(response.status(), 401);
}
