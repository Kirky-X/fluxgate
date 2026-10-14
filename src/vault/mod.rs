// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 密钥保密柜：LLM 厂商密钥的应用层加密持久化（cocoon AEAD）。
//!
//! 明文边界：密钥明文只允许出现在 API/env 入口瞬间、进程内存使用瞬间、
//! 上游请求拼装瞬间；`SecretKeyRecord` 与一切持久化位置只携带密文与掩码。
//! `reveal` 是唯一明文出口，仅供启动装配与 inline 池重注消费。

use secrecy::SecretString;
use serde::Serialize;

#[cfg(feature = "vendor-management")]
mod bootstrap;
mod crypto;
#[cfg(feature = "database")]
mod db_store;
mod file_store;
mod master_key;

#[cfg(feature = "vendor-management")]
pub use bootstrap::inject_vendor_keys;
pub(crate) use bootstrap::{absorb_env_keys, std_env_lookup};
#[cfg(feature = "database")]
pub use db_store::DbKeyStore;
pub use file_store::{home_vault_path, FileKeyStore};
pub use master_key::load_or_create_master_key;

/// 柜内条目的对外形态：永不携带明文。
#[derive(Debug, Clone, Serialize)]
pub struct SecretKeyRecord {
    pub id: String,
    pub vendor_id: String,
    pub label: Option<String>,
    /// 掩码形态（`mask_key` 产出），仅用于列表展示与人工核对。
    pub mask: String,
    /// 明文 SHA256 hex 前 16 字符；(vendor_id, fingerprint) 唯一。
    pub fingerprint: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// 密钥柜后端抽象：db（默认，`database` feature）与文件（SDK 无库形态）双实现。
/// async 签名：db 后端走 dbnexus 异步会话；`async_trait` 保 `Arc<dyn KeyStore>` dyn 安全。
#[async_trait::async_trait]
pub trait KeyStore: Send + Sync {
    /// 写入或更新（同 vendor_id + fingerprint 的 upsert）。
    async fn put(
        &self,
        vendor_id: &str,
        label: Option<&str>,
        plaintext: &SecretString,
    ) -> Result<SecretKeyRecord, VaultError>;
    /// 列表（`vendor_id` 为 None 时全量），按 created_at_ms 升序。
    async fn list(&self, vendor_id: Option<&str>) -> Result<Vec<SecretKeyRecord>, VaultError>;
    /// 删除；返回是否确有删除。
    async fn remove(&self, id: &str) -> Result<bool, VaultError>;
    /// 唯一明文出口：注入 inline key pool 时消费，调用方即用即弃。
    async fn reveal(&self, id: &str) -> Result<SecretString, VaultError>;
}

/// 保密柜错误：Display 恒英文规范串（对齐 error 双轨惯例），HTTP 映射在 api 层。
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Master key error: {0}")]
    MasterKey(String),
    #[error("Crypto error: {0}")]
    Crypto(String),
    #[error("Storage error: {message}")]
    Storage { message: String },
    #[error("Vault key not found: {0}")]
    NotFound(String),
    #[error("Unknown vendor: {0}")]
    UnknownVendor(String),
}
