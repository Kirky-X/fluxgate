// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 文件柜后端：整柜条目序列化后经 cocoon AEAD 落盘（SDK 无库形态保底）。
//!
//! `Cocoon` 的构造均强制 PBKDF2，而 master key 已是随机 32 字节，故沿用
//! `MiniCocoon::from_key` 的 `wrap`/`unwrap` 产 self-contained container，
//! 文件头自带 magic+版本行补自描述；写盘走临时文件 + rename 原子替换。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::i18n::{t, t_args};
use crate::vault::crypto;
use crate::vault::{KeyStore, SecretKeyRecord, VaultError};

/// 文件头：magic + 格式版本，将来 cipher/结构演进由此行承载。
const VAULT_FILE_MAGIC: &[u8] = b"FLUXGATE-VAULT1\n";

/// 默认柜文件：`~/.fluxgate/vault.bin`。
pub fn home_vault_path() -> Result<PathBuf, VaultError> {
    let home = std::env::home_dir().ok_or_else(|| VaultError::Storage {
        message: t("vault-home-not-found"),
    })?;
    Ok(home.join(".fluxgate").join("vault.bin"))
}

/// 柜内条目：blob 为 `crypto::seal` 产出的 self-contained 密文。
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    vendor_id: String,
    label: Option<String>,
    blob: Vec<u8>,
    mask: String,
    fingerprint: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

/// 文件柜：整柜常驻内存，每次变更全量落盘（条目量级：个位数到几十条）。
pub struct FileKeyStore {
    master_key: Vec<u8>,
    path: PathBuf,
    entries: Mutex<BTreeMap<String, StoredEntry>>,
}

impl FileKeyStore {
    /// 打开文件柜：文件存在则解密载入，不存在则以空柜启动（首次 put 时落盘）。
    pub fn open(master_key: Vec<u8>, path: impl Into<PathBuf>) -> Result<Self, VaultError> {
        let path = path.into();
        let entries = if path.exists() {
            let raw = std::fs::read(&path).map_err(|e| VaultError::Storage {
                message: t_args("vault-read-file-failed", &[("error", e.to_string())]),
            })?;
            let payload =
                raw.strip_prefix(VAULT_FILE_MAGIC)
                    .ok_or_else(|| VaultError::Storage {
                        message: t("vault-file-magic-mismatch"),
                    })?;
            let bytes = crypto::open(&master_key, payload)?;
            serde_json::from_str::<BTreeMap<String, StoredEntry>>(bytes.expose_secret()).map_err(
                |e| VaultError::Storage {
                    message: t_args("vault-decode-entries-failed", &[("error", e.to_string())]),
                },
            )?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            master_key,
            path,
            entries: Mutex::new(entries),
        })
    }

    fn persist(&self, entries: &BTreeMap<String, StoredEntry>) -> Result<(), VaultError> {
        let json = serde_json::to_vec(entries).map_err(|e| VaultError::Storage {
            message: t_args("vault-encode-entries-failed", &[("error", e.to_string())]),
        })?;
        let container =
            crypto::cipher(&self.master_key)
                .wrap(&json)
                .map_err(|e| VaultError::Storage {
                    message: t_args("vault-seal-file-failed", &[("error", e.to_string())]),
                })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| VaultError::Storage {
                message: t_args("vault-create-dir-failed", &[("error", e.to_string())]),
            })?;
        }
        let tmp = self.path.with_extension("bin.tmp");
        {
            let mut file = std::fs::File::create(&tmp).map_err(|e| VaultError::Storage {
                message: t_args("vault-create-tmp-failed", &[("error", e.to_string())]),
            })?;
            file.write_all(VAULT_FILE_MAGIC)
                .and_then(|_| file.write_all(&container))
                .map_err(|e| VaultError::Storage {
                    message: t_args("vault-write-file-failed", &[("error", e.to_string())]),
                })?;
            file.sync_all().map_err(|e| VaultError::Storage {
                message: t_args("vault-sync-file-failed", &[("error", e.to_string())]),
            })?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| VaultError::Storage {
            message: t_args("vault-replace-file-failed", &[("error", e.to_string())]),
        })?;
        Ok(())
    }

    fn record(id: &str, entry: &StoredEntry) -> SecretKeyRecord {
        SecretKeyRecord {
            id: id.to_string(),
            vendor_id: entry.vendor_id.clone(),
            label: entry.label.clone(),
            mask: entry.mask.clone(),
            fingerprint: entry.fingerprint.clone(),
            created_at_ms: entry.created_at_ms,
            updated_at_ms: entry.updated_at_ms,
        }
    }
}

#[async_trait::async_trait]
impl KeyStore for FileKeyStore {
    async fn put(
        &self,
        vendor_id: &str,
        label: Option<&str>,
        plaintext: &SecretString,
    ) -> Result<SecretKeyRecord, VaultError> {
        let mut entries = self.entries.lock().expect("vault lock poisoned");
        let fingerprint = crypto::fingerprint(plaintext.expose_secret());
        let now = crypto::now_ms();
        let existing = entries
            .iter_mut()
            .find(|(_, e)| e.vendor_id == vendor_id && e.fingerprint == fingerprint);
        let id = match existing {
            Some((id, entry)) => {
                entry.label = label.map(str::to_string);
                entry.updated_at_ms = now;
                id.clone()
            }
            None => {
                let id = crypto::new_id();
                let blob = crypto::seal(&self.master_key, plaintext)?;
                let mask = crypto::mask_key(plaintext.expose_secret());
                entries.insert(
                    id.clone(),
                    StoredEntry {
                        vendor_id: vendor_id.to_string(),
                        label: label.map(str::to_string),
                        blob,
                        mask,
                        fingerprint: fingerprint.clone(),
                        created_at_ms: now,
                        updated_at_ms: now,
                    },
                );
                id
            }
        };
        self.persist(&entries)?;
        let entry = &entries[&id];
        Ok(Self::record(&id, entry))
    }

    async fn list(&self, vendor_id: Option<&str>) -> Result<Vec<SecretKeyRecord>, VaultError> {
        let entries = self.entries.lock().expect("vault lock poisoned");
        let mut rows: Vec<(&String, &StoredEntry)> = entries
            .iter()
            .filter(|(_, e)| vendor_id.is_none_or(|v| e.vendor_id == v))
            .collect();
        rows.sort_by_key(|(_, e)| e.created_at_ms);
        Ok(rows
            .into_iter()
            .map(|(id, e)| Self::record(id, e))
            .collect())
    }

    async fn remove(&self, id: &str) -> Result<bool, VaultError> {
        let mut entries = self.entries.lock().expect("vault lock poisoned");
        let removed = entries.remove(id).is_some();
        if removed {
            self.persist(&entries)?;
        }
        Ok(removed)
    }

    async fn reveal(&self, id: &str) -> Result<SecretString, VaultError> {
        let entries = self.entries.lock().expect("vault lock poisoned");
        let entry = entries
            .get(id)
            .ok_or_else(|| VaultError::NotFound(id.to_string()))?;
        crypto::open(&self.master_key, &entry.blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn open_store(dir: &Path) -> FileKeyStore {
        FileKeyStore::open(vec![7u8; 32], dir.join("vault.bin")).expect("open store")
    }

    async fn put_key(
        store: &FileKeyStore,
        vendor: &str,
        label: Option<&str>,
        key: &str,
    ) -> SecretKeyRecord {
        store
            .put(vendor, label, &SecretString::new(key.into()))
            .await
            .expect("put")
    }

    #[tokio::test]
    async fn put_list_remove_reveal_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(dir.path());
        let record = put_key(&store, "openai", Some("prod"), "sk-live-abcdef123").await;
        assert_eq!(record.mask, "sk-***f123");
        assert_eq!(record.vendor_id, "openai");
        let listed = store.list(Some("openai")).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, record.id);
        assert_eq!(listed[0].mask, "sk-***f123");
        let revealed = store.reveal(&record.id).await.expect("reveal");
        assert_eq!(revealed.expose_secret(), "sk-live-abcdef123");
        assert!(store.remove(&record.id).await.expect("remove"));
        assert!(matches!(
            store.reveal(&record.id).await,
            Err(VaultError::NotFound(_))
        ));
        assert!(!store.remove(&record.id).await.expect("remove again"));
    }

    #[tokio::test]
    async fn entries_survive_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.bin");
        let record = {
            let store = FileKeyStore::open(vec![7u8; 32], &path).expect("open");
            put_key(&store, "anthropic", None, "sk-ant-xyz98765432").await
        };
        let reopened = FileKeyStore::open(vec![7u8; 32], &path).expect("reopen");
        assert_eq!(
            reopened.list(Some("anthropic")).await.expect("list").len(),
            1
        );
        assert_eq!(
            reopened
                .reveal(&record.id)
                .await
                .expect("reveal")
                .expose_secret(),
            "sk-ant-xyz98765432"
        );
    }

    #[tokio::test]
    async fn same_fingerprint_upserts_instead_of_duplicating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(dir.path());
        let first = put_key(&store, "openai", None, "sk-same-key-123456").await;
        let second = put_key(&store, "openai", Some("renamed"), "sk-same-key-123456").await;
        assert_eq!(first.id, second.id);
        assert_eq!(second.label.as_deref(), Some("renamed"));
        assert_eq!(store.list(Some("openai")).await.expect("list").len(), 1);
    }

    #[test]
    fn magic_mismatch_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.bin");
        std::fs::write(&path, b"GARBAGE-DATA").expect("write garbage");
        let err = FileKeyStore::open(vec![7u8; 32], path);
        assert!(matches!(err, Err(VaultError::Storage { .. })));
    }
}
