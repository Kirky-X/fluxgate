// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 订阅 refresh token 的 vault 封装：账号键 `sub:{account_id}` 到
//! [`KeyStore`] 条目的映射（vendor_id 列 = 键，label 固定 "refresh"）。
//!
//! 轮换语义：先写新条目、成功后清掉旧条目（put 失败时旧 token 仍可读，
//! 不丢凭证）；任意时刻同键至多一条存活条目。全路径零明文——vault 条目
//! 只落密文（cocoon AEAD），日志与错误消息不携带 token。

#[cfg(test)]
use secrecy::ExposeSecret;
use secrecy::SecretString;
use std::sync::Arc;

use crate::vault::{KeyStore, VaultError};

/// vault 键前缀：账号 id → `sub:{account_id}`（design 决策 2）。
pub(crate) fn vault_key(account_id: &str) -> String {
    format!("sub:{account_id}")
}

const REFRESH_LABEL: &str = "refresh";

/// 该账号名下的存活 vault 条目（按 created_at 升序）。
async fn account_entries(
    store: &Arc<dyn KeyStore>,
    account_id: &str,
) -> Result<Vec<crate::vault::SecretKeyRecord>, VaultError> {
    store.list(Some(&vault_key(account_id))).await
}

/// 写入（或轮换）账号的 refresh token：新条目落库成功后清理旧条目。
/// 返回清理掉的旧条目数（可观测，不含任何密文）。
pub(crate) async fn save_refresh_token(
    store: &Arc<dyn KeyStore>,
    account_id: &str,
    token: &SecretString,
) -> Result<usize, VaultError> {
    let fresh = store
        .put(&vault_key(account_id), Some(REFRESH_LABEL), token)
        .await?;
    let stale: Vec<String> = account_entries(store, account_id)
        .await?
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| *id != fresh.id)
        .collect();
    let mut removed = 0usize;
    for id in &stale {
        if store.remove(id).await? {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::warn!(
            account = %account_id,
            stale = removed,
            "{}",
            crate::i18n::t("subscription-refresh-token-rotated")
        );
    }
    Ok(removed)
}

/// 读取账号当前 refresh token；无条目 → `VaultError::NotFound`。
/// 取 **created_at 最新**条目（list 升序 → 末条）：轮换「先写新后删旧」
/// 中途崩溃时磁盘上新旧并存，读新不读旧——旧 token 可能已被上游轮换失效，
/// 读旧会把账号误降级。
pub(crate) async fn load_refresh_token(
    store: &Arc<dyn KeyStore>,
    account_id: &str,
) -> Result<SecretString, VaultError> {
    let entry = account_entries(store, account_id)
        .await?
        .into_iter()
        .next_back()
        .ok_or_else(|| VaultError::NotFound(vault_key(account_id)))?;
    store.reveal(&entry.id).await
}

/// 吊销：删除账号名下全部 vault 条目，返回删除数。
pub(crate) async fn delete_account_tokens(
    store: &Arc<dyn KeyStore>,
    account_id: &str,
) -> Result<usize, VaultError> {
    let ids: Vec<String> = account_entries(store, account_id)
        .await?
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let mut removed = 0usize;
    for id in &ids {
        if store.remove(id).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::FileKeyStore;
    use std::sync::Mutex;

    fn open_store(dir: &tempfile::TempDir) -> Arc<dyn KeyStore> {
        Arc::new(
            FileKeyStore::open(vec![7u8; 32], dir.path().join("vault.bin")).expect("file store"),
        )
    }

    /// 事件捕获 subscriber：字段拼接进共享缓冲（消息 + 结构化字段全收）。
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct FieldText(String);
            impl tracing::field::Visit for FieldText {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.push_str(&format!("{}={value:?} ", field.name()));
                }
            }
            let mut visitor = FieldText(String::new());
            event.record(&mut visitor);
            self.0.lock().unwrap().push(visitor.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// vault 回环契约：存取一致、轮换后旧条目清零、吊销全清、账号键隔离；
    /// 全程捕获 tracing 事件，断言捕获面（含轮换告警）零明文子串。
    #[tokio::test]
    async fn refresh_token_roundtrips_vault_no_plaintext_in_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open_store(&dir);
        let token_v1 = SecretString::new("sub-rt-plaintext-v1-DO-NOT-LOG".into());
        let token_v2 = SecretString::new("sub-rt-plaintext-v2-DO-NOT-LOG".into());

        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _guard = tracing::subscriber::set_default(Arc::new(Capture(buffer.clone())));

        // 存 → 取回环
        let removed = save_refresh_token(&store, "sub-1", &token_v1)
            .await
            .expect("save v1");
        assert_eq!(removed, 0, "首存无旧条目");
        let loaded = load_refresh_token(&store, "sub-1").await.expect("load v1");
        assert_eq!(loaded.expose_secret(), "sub-rt-plaintext-v1-DO-NOT-LOG");

        // 轮换：新 token 可读，旧条目清掉（同键单条存活）+ 触发轮换告警
        let removed = save_refresh_token(&store, "sub-1", &token_v2)
            .await
            .expect("save v2");
        assert_eq!(removed, 1, "轮换清理一条旧条目");
        assert_eq!(
            store
                .list(Some(&vault_key("sub-1")))
                .await
                .expect("list")
                .len(),
            1,
            "同键至多一条存活条目"
        );
        let loaded = load_refresh_token(&store, "sub-1").await.expect("load v2");
        assert_eq!(loaded.expose_secret(), "sub-rt-plaintext-v2-DO-NOT-LOG");

        // 吊销：全清 + 读取回落 NotFound
        assert_eq!(
            delete_account_tokens(&store, "sub-1")
                .await
                .expect("revoke"),
            1
        );
        assert!(matches!(
            load_refresh_token(&store, "sub-1").await,
            Err(VaultError::NotFound(_))
        ));

        // 账号键隔离：sub-2 的条目不因 sub-1 吊销受影响
        save_refresh_token(&store, "sub-2", &token_v1)
            .await
            .expect("save sub-2");
        assert_eq!(
            load_refresh_token(&store, "sub-2")
                .await
                .expect("load sub-2")
                .expose_secret(),
            "sub-rt-plaintext-v1-DO-NOT-LOG"
        );

        // 轮换中断窗：put 成功、旧条目清理失败（「先写新后删旧」中途崩溃/
        // 删除失败同形态）→ 新旧并存，load 必须读 **created_at 最新** 条目
        // ——读旧会把已被上游轮换失效的 token 当当前值，账号误降级。
        let removed = save_refresh_token(&store, "sub-3", &token_v1)
            .await
            .expect("save sub-3 v1");
        assert_eq!(removed, 0);
        // 人为造并存：直写第二条（模拟清理失败的现场）
        let _ = store
            .put(&vault_key("sub-3"), Some(REFRESH_LABEL), &token_v2)
            .await
            .expect("并存写入");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _ = store
            .put(&vault_key("sub-3"), Some(REFRESH_LABEL), &{
                SecretString::new("sub-rt-plaintext-v3-DO-NOT-LOG".into())
            })
            .await
            .expect("并存写入 v3");
        let loaded = load_refresh_token(&store, "sub-3").await.expect("load");
        assert_eq!(
            loaded.expose_secret(),
            "sub-rt-plaintext-v3-DO-NOT-LOG",
            "并存时必须读 created_at 最新条目"
        );
        delete_account_tokens(&store, "sub-3").await.expect("清理");

        // 捕获面非空（轮换告警确已进入日志管线，结构化字段 locale 无关）
        // 且全程零明文
        let captured = buffer.lock().unwrap().join("\n");
        assert!(
            captured.contains("stale=1"),
            "轮换告警必须留痕: {captured:?}"
        );
        for needle in ["sub-rt-plaintext-v1", "sub-rt-plaintext-v2"] {
            assert!(
                !captured.contains(needle),
                "捕获日志不得含 token 明文子串 {needle}: {captured:?}"
            );
        }
    }
}
