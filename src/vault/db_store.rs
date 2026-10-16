// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! db 柜后端（默认）：逐条 cocoon 密文存 `vault_keys.payload`（V3 迁移建表），
//! 与模型目录同库同事务语义；掩码/指纹为明文衍生列，列表页零解密。
//!
//! 语句口径仿 `repository.rs`：`pool.get_session("admin")` +
//! `Statement::from_sql_and_values`（占位符统一 `?`，按 backend 翻译）。

use sea_orm::{ConnectionTrait, Statement, Value};
use secrecy::{ExposeSecret, SecretString};

use crate::i18n::t_args;
use crate::vault::crypto;
use crate::vault::{KeyStore, SecretKeyRecord, VaultError};

/// db 柜：master key 仅驻进程内存，密文落 `vault_keys` 表。
pub struct DbKeyStore {
    master_key: Vec<u8>,
    pool: dbnexus::DbPool,
}

impl DbKeyStore {
    pub fn new(master_key: Vec<u8>, pool: dbnexus::DbPool) -> Self {
        Self { master_key, pool }
    }

    fn record(row: &sea_orm::QueryResult) -> Result<SecretKeyRecord, VaultError> {
        Ok(SecretKeyRecord {
            id: row.try_get::<String>("", "id").map_err(db_err)?,
            vendor_id: row.try_get::<String>("", "vendor_id").map_err(db_err)?,
            label: row.try_get::<Option<String>>("", "label").map_err(db_err)?,
            mask: row.try_get::<String>("", "mask").map_err(db_err)?,
            fingerprint: row.try_get::<String>("", "fingerprint").map_err(db_err)?,
            created_at_ms: row.try_get::<i64>("", "created_at").map_err(db_err)?,
            updated_at_ms: row.try_get::<i64>("", "updated_at").map_err(db_err)?,
        })
    }
}

fn db_err(e: impl std::fmt::Display) -> VaultError {
    VaultError::Storage {
        message: t_args("vault-db-access-failed", &[("error", e.to_string())]),
    }
}

const SELECT_COLS: &str = "id, vendor_id, label, mask, fingerprint, created_at, updated_at";

#[async_trait::async_trait]
impl KeyStore for DbKeyStore {
    async fn put(
        &self,
        vendor_id: &str,
        label: Option<&str>,
        plaintext: &SecretString,
    ) -> Result<SecretKeyRecord, VaultError> {
        let session = self.pool.get_session("admin").await.map_err(db_err)?;
        let conn = session.connection().map_err(db_err)?;
        let backend = conn.get_database_backend();
        let fingerprint = crypto::fingerprint(plaintext.expose_secret());
        let mask = crypto::mask_key(plaintext.expose_secret());
        let now = crypto::now_ms();

        let existing = Statement::from_sql_and_values(
            backend,
            "SELECT id FROM vault_keys WHERE vendor_id = ? AND fingerprint = ?",
            [vendor_id.into(), fingerprint.as_str().into()],
        );
        let found = conn.query_all_raw(existing).await.map_err(db_err)?;
        let blob = crypto::seal(&self.master_key, plaintext)?;
        let id = if let Some(row) = found.first() {
            let id: String = row.try_get("", "id").map_err(db_err)?;
            let update = Statement::from_sql_and_values(
                backend,
                "UPDATE vault_keys SET label = ?, payload = ?, mask = ?, updated_at = ? WHERE id = ?",
                [
                    label.map(str::to_string).into(),
                    Value::Bytes(Some(blob)),
                    mask.into(),
                    now.into(),
                    id.clone().into(),
                ],
            );
            conn.execute_raw(update).await.map_err(db_err)?;
            id
        } else {
            let id = crypto::new_id();
            let insert = Statement::from_sql_and_values(
                backend,
                "INSERT INTO vault_keys (id, vendor_id, label, payload, mask, fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                [
                    id.clone().into(),
                    vendor_id.into(),
                    label.map(str::to_string).into(),
                    Value::Bytes(Some(blob)),
                    mask.into(),
                    fingerprint.into(),
                    now.into(),
                    now.into(),
                ],
            );
            conn.execute_raw(insert).await.map_err(db_err)?;
            id
        };

        let row = Statement::from_sql_and_values(
            backend,
            format!("SELECT {SELECT_COLS} FROM vault_keys WHERE id = ?"),
            [id.clone().into()],
        );
        conn.query_all_raw(row)
            .await
            .map_err(db_err)?
            .first()
            .map(Self::record)
            .transpose()?
            .ok_or(VaultError::NotFound(id))
    }

    async fn list(&self, vendor_id: Option<&str>) -> Result<Vec<SecretKeyRecord>, VaultError> {
        let session = self.pool.get_session("admin").await.map_err(db_err)?;
        let conn = session.connection().map_err(db_err)?;
        let (sql, values) = match vendor_id {
            Some(v) => (
                format!("SELECT {SELECT_COLS} FROM vault_keys WHERE vendor_id = ? ORDER BY created_at ASC"),
                vec![v.into()],
            ),
            None => (
                format!("SELECT {SELECT_COLS} FROM vault_keys ORDER BY created_at ASC"),
                Vec::new(),
            ),
        };
        let stmt = Statement::from_sql_and_values(conn.get_database_backend(), &sql, values);
        let rows = conn.query_all_raw(stmt).await.map_err(db_err)?;
        rows.iter().map(Self::record).collect()
    }

    async fn remove(&self, id: &str) -> Result<bool, VaultError> {
        let session = self.pool.get_session("admin").await.map_err(db_err)?;
        let conn = session.connection().map_err(db_err)?;
        let stmt = Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM vault_keys WHERE id = ?",
            [id.into()],
        );
        let res = conn.execute_raw(stmt).await.map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn reveal(&self, id: &str) -> Result<SecretString, VaultError> {
        let session = self.pool.get_session("admin").await.map_err(db_err)?;
        let conn = session.connection().map_err(db_err)?;
        let stmt = Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT payload FROM vault_keys WHERE id = ?",
            [id.into()],
        );
        let rows = conn.query_all_raw(stmt).await.map_err(db_err)?;
        let row = rows
            .first()
            .ok_or_else(|| VaultError::NotFound(id.to_string()))?;
        let blob: Vec<u8> = row.try_get("", "payload").map_err(db_err)?;
        crypto::open(&self.master_key, &blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_store() -> DbKeyStore {
        // 与 migration live_tests::memory_pool 同口径：sqlite::memory: 直连 DbPool，
        // V3 schema 经 init_schema 建齐（vault_keys 表断言见迁移测试）
        let pool = memory_pool().await;
        crate::database::repository::init_schema(&pool)
            .await
            .expect("init V3 schema");
        DbKeyStore::new(vec![7u8; 32], pool)
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

    #[tokio::test]
    async fn put_list_remove_reveal_roundtrip() {
        let store = open_store().await;
        let record = store
            .put(
                "openai",
                Some("prod"),
                &SecretString::new("sk-live-abcdef123".into()),
            )
            .await
            .expect("put");
        assert_eq!(record.mask, "sk-***f123");
        assert_eq!(record.vendor_id, "openai");
        let listed = store.list(Some("openai")).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, record.id);
        assert_eq!(listed[0].mask, "sk-***f123");
        assert!(store
            .list(Some("anthropic"))
            .await
            .expect("list")
            .is_empty());
        assert_eq!(
            store
                .reveal(&record.id)
                .await
                .expect("reveal")
                .expose_secret(),
            "sk-live-abcdef123"
        );
        assert!(store.remove(&record.id).await.expect("remove"));
        assert!(matches!(
            store.reveal(&record.id).await,
            Err(VaultError::NotFound(_))
        ));
        assert!(!store.remove(&record.id).await.expect("remove again"));
    }

    #[tokio::test]
    async fn same_fingerprint_upserts_instead_of_duplicating() {
        let store = open_store().await;
        let first = store
            .put(
                "openai",
                None,
                &SecretString::new("sk-same-key-123456".into()),
            )
            .await
            .expect("put 1");
        let second = store
            .put(
                "openai",
                Some("renamed"),
                &SecretString::new("sk-same-key-123456".into()),
            )
            .await
            .expect("put 2");
        assert_eq!(first.id, second.id);
        assert_eq!(second.label.as_deref(), Some("renamed"));
        assert_eq!(store.list(Some("openai")).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn payload_column_never_stores_plaintext() {
        let store = open_store().await;
        let record = store
            .put(
                "anthropic",
                None,
                &SecretString::new("sk-ant-plaintext-marker-42".into()),
            )
            .await
            .expect("put");
        let session = store.pool.get_session("admin").await.expect("session");
        let conn = session.connection().expect("conn");
        let stmt = Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT payload FROM vault_keys WHERE id = ?",
            [record.id.as_str().into()],
        );
        let rows = conn.query_all_raw(stmt).await.expect("raw payload");
        let blob: Vec<u8> = rows[0].try_get("", "payload").expect("blob");
        let needle = b"sk-ant-plaintext-marker-42";
        assert!(
            blob.windows(needle.len()).all(|w| w != needle),
            "payload 列不得含明文子串"
        );
    }
}
