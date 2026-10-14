// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 保密柜启动装配：env 吸收（幂等不覆盖）与 inline key pool 注入。
//!
//! 吸收语义：env 有值且柜内无同 (vendor_id, fingerprint) 条目才入库——
//! API 录入的新 key 永不被重启时的旧 env 值覆盖；env 只是首装引导源。

use secrecy::{ExposeSecret, SecretString};

use crate::config::VendorConfig;
use crate::vault::crypto;
use crate::vault::KeyStore;
use crate::vault::VaultError;
use crate::vendor::VendorManager;

/// env 值探测函数型（生产走 `std::env::var`，测试注入避免并行 set_var 竞争）。
pub(crate) type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + Send + Sync + 'a;

/// 对每个 vendor 的合并 env 名逐个探测吸收，返回新入库条数。
pub(crate) async fn absorb_env_keys(
    store: &dyn KeyStore,
    vendors: &[VendorConfig],
    lookup: &EnvLookup<'_>,
) -> Result<usize, VaultError> {
    let mut absorbed = 0;
    for vendor in vendors {
        for name in crate::vendor::key_pool::merged_key_env_names(vendor) {
            let Some(value) = lookup(&name) else {
                continue;
            };
            if value.trim().is_empty() {
                continue;
            }
            let plaintext = SecretString::new(value.into());
            let fingerprint = crypto::fingerprint(plaintext.expose_secret());
            let present = store
                .list(Some(&vendor.id))
                .await?
                .iter()
                .any(|r| r.fingerprint == fingerprint);
            if present {
                continue;
            }
            // label 记录来源 env 名（人工核对"这条 key 从哪来"）
            store.put(&vendor.id, Some(&name), &plaintext).await?;
            absorbed += 1;
        }
    }
    Ok(absorbed)
}

/// 生产侧 env 探测。
pub(crate) fn std_env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// 把柜内某 vendor 的全部条目解密注入 inline key pool（热生效）；
/// 柜内无条目时返回 false（不触碰既有 pool，env 兜底语义保留）。
pub async fn inject_vendor_keys(
    store: &dyn KeyStore,
    manager: &VendorManager,
    vendor_id: &str,
) -> Result<bool, VaultError> {
    let records = store.list(Some(vendor_id)).await?;
    if records.is_empty() {
        return Ok(false);
    }
    let mut keys = Vec::with_capacity(records.len());
    for record in &records {
        keys.push(store.reveal(&record.id).await?.expose_secret().to_string());
    }
    manager.set_inline_key_pool(vendor_id, crate::vendor::PoolSnapshot::from_keys(keys));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::FileKeyStore;

    fn vendor(id: &str, envs: &[&str]) -> VendorConfig {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "type_": "openai",
            "api_key_env": "",
            "api_key_envs": envs,
            "rate_limit": { "type": "interval", "hours": 1 },
            "concurrency": {},
        }))
        .expect("vendor config fragment")
    }

    fn open_store(dir: &std::path::Path) -> FileKeyStore {
        FileKeyStore::open(vec![7u8; 32], dir.join("vault.bin")).expect("open store")
    }

    fn lookup_of<'a>(map: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        move |name: &str| {
            map.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[tokio::test]
    async fn absorb_is_idempotent_across_restarts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(dir.path());
        let vendors = [vendor("openai", &["TEST_VAULT_KEY_A"])];
        let lookup = lookup_of(&[("TEST_VAULT_KEY_A", "sk-absorb-once-123456")]);
        let first = absorb_env_keys(&store, &vendors, &lookup)
            .await
            .expect("absorb 1");
        assert_eq!(first, 1);
        // 二次启动（env 不变）不新增
        let second = absorb_env_keys(&store, &vendors, &lookup)
            .await
            .expect("absorb 2");
        assert_eq!(second, 0);
        assert_eq!(store.list(Some("openai")).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn api_stored_key_wins_over_stale_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(dir.path());
        // API 先录入（label 不同于 env 名）
        let api_record = store
            .put(
                "openai",
                Some("admin-api"),
                &SecretString::new("sk-same-value-9876".into()),
            )
            .await
            .expect("api put");
        let vendors = [vendor("openai", &["TEST_VAULT_KEY_B"])];
        let lookup = lookup_of(&[("TEST_VAULT_KEY_B", "sk-same-value-9876")]);
        let absorbed = absorb_env_keys(&store, &vendors, &lookup)
            .await
            .expect("absorb");
        assert_eq!(absorbed, 0, "同指纹 key 已在柜内，env 不得覆盖");
        let listed = store.list(Some("openai")).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label.as_deref(), Some("admin-api"));
        assert_eq!(listed[0].id, api_record.id);
    }

    #[tokio::test]
    async fn empty_or_missing_env_values_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(dir.path());
        let vendors = [
            vendor("openai", &["TEST_VAULT_MISSING", "TEST_VAULT_EMPTY"]),
            vendor("anthropic", &["TEST_VAULT_OK"]),
        ];
        let lookup = lookup_of(&[
            ("TEST_VAULT_EMPTY", "  "),
            ("TEST_VAULT_OK", "sk-anthropic-real-01"),
        ]);
        let absorbed = absorb_env_keys(&store, &vendors, &lookup)
            .await
            .expect("absorb");
        assert_eq!(absorbed, 1);
        assert!(store.list(Some("openai")).await.expect("list").is_empty());
        assert_eq!(store.list(Some("anthropic")).await.expect("list").len(), 1);
    }
}
