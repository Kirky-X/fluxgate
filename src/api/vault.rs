// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 密钥保密柜管理面（key-vault）：录入/列表/删除，明文只进不出。
//!
//! 挂载于 `/admin/vault/keys*`，受全局 auth 中间件保护；响应与日志零明文
//!（列表只回 `SecretKeyRecord` 掩码面）。put/remove 后即时重注 inline
//! key pool（热生效；柜清空时清池回退 env 兜底）。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use secrecy::SecretString;
use serde::Deserialize;

use crate::error::{FluxgateError, GateError};
use crate::vault::{SecretKeyRecord, VaultError};
use crate::ApiState;

/// vault 未接线（feature 关闭或装配失败）时的统一错误。
fn vault_unavailable() -> FluxgateError {
    FluxgateError::Gate(GateError::VendorUnavailable {
        vendor: "vault".to_string(),
    })
}

fn store_of(state: &ApiState) -> Result<Arc<dyn crate::vault::KeyStore>, FluxgateError> {
    state.vendor_manager.vault().ok_or_else(vault_unavailable)
}

#[derive(Deserialize)]
pub struct PutKeyRequest {
    pub vendor_id: String,
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// POST /admin/vault/keys：录入（同指纹 upsert）并即时重注。
pub async fn put_key(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<PutKeyRequest>,
) -> Result<Json<SecretKeyRecord>, FluxgateError> {
    let store = store_of(&state)?;
    if body.key.trim().is_empty() {
        return Err(FluxgateError::Gate(GateError::Validation {
            field: "key".to_string(),
        }));
    }
    let known = state
        .config
        .read()
        .vendors
        .iter()
        .any(|v| v.id == body.vendor_id);
    if !known {
        return Err(FluxgateError::Vault(VaultError::UnknownVendor(
            body.vendor_id.clone(),
        )));
    }
    let plaintext = SecretString::new(body.key.into());
    let record = store
        .put(&body.vendor_id, body.label.as_deref(), &plaintext)
        .await?;
    crate::vault::inject_vendor_keys(store.as_ref(), &state.vendor_manager, &body.vendor_id)
        .await?;
    Ok(Json(record))
}

/// GET /admin/vault/keys?vendor_id=：掩码列表，零明文。
pub async fn list_keys(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ListKeysQuery>,
) -> Result<Json<ListKeysResponse>, FluxgateError> {
    let store = store_of(&state)?;
    let keys = store.list(query.vendor_id.as_deref()).await?;
    Ok(Json(ListKeysResponse { keys }))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ListKeysQuery {
    pub vendor_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ListKeysResponse {
    pub keys: Vec<SecretKeyRecord>,
}

/// DELETE /admin/vault/keys/{id}：删除并即时同步 inline pool（柜清空则
/// 清池，该 vendor 回退 env 兜底语义）。
pub async fn delete_key(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<DeleteKeyResponse>, FluxgateError> {
    let store = store_of(&state)?;
    // 删除前定位 vendor（remove 后条目不可查）
    let target_vendor = store
        .list(None)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.vendor_id);
    let removed = store.remove(&id).await?;
    if removed {
        if let Some(vendor_id) = target_vendor {
            let injected =
                crate::vault::inject_vendor_keys(store.as_ref(), &state.vendor_manager, &vendor_id)
                    .await?;
            if !injected {
                state.vendor_manager.clear_inline_key_pool(&vendor_id);
            }
        }
    }
    Ok(Json(DeleteKeyResponse { removed }))
}

#[derive(serde::Serialize)]
pub struct DeleteKeyResponse {
    pub removed: bool,
}
