// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 订阅账号池（subscription-upstream）：进程内账号注册表 + token 生命周期。
//!
//! 状态机：`PendingAuthorization`（已登记未授权）→ `Active`（服务中）→
//! `Degraded`（刷新失败降级：30 分钟后允许重试一次，仍败摘除）→ 移除。
//! access token 只驻内存；refresh token 经 `store.rs` 落 vault。
//! 出站取凭证按 vendor 轮换（每 vendor 独立游标，只轮 Active/重试到期账号）；
//! 临期（到期前 5 分钟）抢占刷新，同账号并发只发一次刷新请求（per-account
//! `tokio::sync::Mutex` 串行 + 拿锁后复查）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use secrecy::{ExposeSecret, SecretString};

use crate::utils::metrics::GlobalMetrics;
use crate::vault::KeyStore;

use super::oauth::OauthSessionStore;
use super::store;
use super::{SubscriptionError, SubscriptionProvider, TokenSet};

/// authorize 发起产物（admin 面回显）：授权 URL 与回调核对用的 state。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthorizationStart {
    pub account_id: String,
    pub state: String,
    pub authorize_url: String,
}

/// 到期前抢占刷新窗口（design 决策 3：到期前 5 分钟刷新）。
pub const PREEMPTIVE_REFRESH_WINDOW_MS: u64 = 5 * 60 * 1000;

/// 刷新失败降级冷却：30 分钟后允许重试一次（design 决策 3）。
pub const DEGRADE_RETRY_AFTER_MS: u64 = 30 * 60 * 1000;

/// 账号池状态（admin 列表可见；`Removed` 不落枚举——移除即从注册表消失）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// 已登记未授权（等待 authorize/callback 完成换码）。
    PendingAuthorization,
    /// 服务中（token 可用或可经 refresh token 刷新）。
    Active,
    /// 刷新失败降级：`retry_at_ms` 前不参与出站；到点重试一次，仍败移除。
    Degraded { retry_at_ms: u64 },
}

/// 内存 access token 条目（绝不落盘；Debug 由 `SecretString` 脱敏）。
#[derive(Debug)]
pub struct TokenEntry {
    pub access: SecretString,
    pub expires_at_ms: u64,
}

/// admin 列表行（脱敏：仅 id/vendor/状态/过期时间戳，零 token 片段）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountSummary {
    pub id: String,
    pub vendor_id: String,
    pub state: AccountState,
    /// access token 过期时间戳（毫秒）；未授权/未取到 token 时为 None。
    pub expires_at_ms: Option<u64>,
    /// 是否来自 `[[subscription_accounts]]` 静态声明（重启可恢复）。
    pub declared: bool,
}

struct Account {
    vendor_id: String,
    state: AccountState,
    token: Option<TokenEntry>,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    declared: bool,
}

struct PoolInner {
    /// 插入序账号表（order 保证轮换与列表现为确定性顺序）。
    order: Vec<String>,
    accounts: HashMap<String, Account>,
    /// 每 vendor 的轮换游标。
    cursors: HashMap<String, usize>,
    /// ad-hoc 账号自增序列（`sub-<n>`）。
    next_seq: u64,
}

/// 订阅账号池。`store` 为 None 时（SDK 无库形态）refresh token 不落 vault，
/// 仅本次进程内有效（进程内 HashMap 不持久——重启即失效，文档披露）。
pub struct AccountPool {
    inner: Mutex<PoolInner>,
    provider: Arc<dyn SubscriptionProvider>,
    http: reqwest::Client,
    client_id: String,
    store: Option<Arc<dyn KeyStore>>,
    metrics: Arc<GlobalMetrics>,
    /// 测试时钟覆盖（None = 系统时间；Some = 固定时刻，测试手动推进）。
    clock_override: Mutex<Option<u64>>,
    /// PKCE 授权会话表（authorize → callback 两跳的进程内状态）。
    sessions: OauthSessionStore,
}

impl AccountPool {
    pub fn new(
        provider: Arc<dyn SubscriptionProvider>,
        http: reqwest::Client,
        client_id: String,
        store: Option<Arc<dyn KeyStore>>,
        metrics: Arc<GlobalMetrics>,
    ) -> Self {
        Self {
            inner: Mutex::new(PoolInner {
                order: Vec::new(),
                accounts: HashMap::new(),
                cursors: HashMap::new(),
                next_seq: 0,
            }),
            provider,
            http,
            client_id,
            store,
            metrics,
            clock_override: Mutex::new(None),
            sessions: OauthSessionStore::new(),
        }
    }

    fn now(&self) -> u64 {
        if let Some(fixed) = *self
            .clock_override
            .lock()
            .unwrap_or_else(|p| p.into_inner())
        {
            return fixed;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// 登记静态声明账号（config `[[subscription_accounts]]`）：vault 有
    /// refresh token → 直接 Active（首次使用时按需刷新）；无 → 待授权。
    /// 返回是否新登记（重复声明幂等跳过）。
    pub async fn register_declared(&self, account_id: &str, vendor_id: &str) -> bool {
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if inner.accounts.contains_key(account_id) {
                return false;
            }
        }
        let has_refresh = match &self.store {
            Some(s) => store::load_refresh_token(s, account_id).await.is_ok(),
            None => false,
        };
        let state = if has_refresh {
            AccountState::Active
        } else {
            AccountState::PendingAuthorization
        };
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.accounts.contains_key(account_id) {
            return false;
        }
        inner.order.push(account_id.to_string());
        inner.accounts.insert(
            account_id.to_string(),
            Account {
                vendor_id: vendor_id.to_string(),
                state,
                token: None,
                refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
                declared: true,
            },
        );
        true
    }

    /// authorize 发起：`account_id` 给定 → 复用声明账号（不存在或 vendor
    /// 不符即拒绝）；None → ad-hoc 自增账号 `sub-<n>`。返回账号 id。
    pub fn ensure_account(
        &self,
        vendor_id: &str,
        account_id: Option<&str>,
    ) -> Result<String, SubscriptionError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = account_id {
            let account = inner
                .accounts
                .get_mut(id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(id.to_string()))?;
            if account.vendor_id != vendor_id {
                return Err(SubscriptionError::UnknownAccount(id.to_string()));
            }
            return Ok(id.to_string());
        }
        inner.next_seq += 1;
        let mut id = format!("sub-{}", inner.next_seq);
        while inner.accounts.contains_key(&id) {
            inner.next_seq += 1;
            id = format!("sub-{}", inner.next_seq);
        }
        inner.order.push(id.clone());
        inner.accounts.insert(
            id.clone(),
            Account {
                vendor_id: vendor_id.to_string(),
                state: AccountState::PendingAuthorization,
                token: None,
                refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
                declared: false,
            },
        );
        Ok(id)
    }

    /// callback 完成：refresh token 回存 vault（无 store 则仅内存），
    /// access token 入内存池，账号转 Active。
    pub async fn complete_authorization(
        &self,
        account_id: &str,
        tokens: TokenSet,
    ) -> Result<(), SubscriptionError> {
        if let Some(s) = &self.store {
            store::save_refresh_token(s, account_id, &tokens.refresh_token)
                .await
                .map_err(|e| SubscriptionError::Transport(e.to_string()))?;
        }
        let now = self.now();
        let expires_at_ms = now + tokens.expires_in_secs.saturating_mul(1000);
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let account = inner
            .accounts
            .get_mut(account_id)
            .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?;
        account.state = AccountState::Active;
        account.token = Some(TokenEntry {
            access: tokens.access_token,
            expires_at_ms,
        });
        Ok(())
    }

    /// 发起授权（admin authorize）：复用/创建账号 → 生成 PKCE 会话 →
    /// 拼供应商授权 URL。`redirect_uri` 必须打回同一实例（单实例约束）。
    pub fn begin_authorization(
        &self,
        vendor_id: &str,
        account_id: Option<&str>,
        redirect_uri: &str,
    ) -> Result<AuthorizationStart, SubscriptionError> {
        let account_id = self.ensure_account(vendor_id, account_id)?;
        let (state, pair) = self.sessions.issue(&account_id, redirect_uri, self.now());
        let authorize_url =
            self.provider
                .authorize_url(&self.client_id, redirect_uri, &state, &pair.challenge);
        Ok(AuthorizationStart {
            account_id,
            state,
            authorize_url,
        })
    }

    /// 完成授权（admin callback）：核对 state（单次、10 分钟）→ 授权码换
    /// token → 凭证落池/落 vault。返回账号 id。
    pub async fn finish_authorization(
        &self,
        state: &str,
        code: &str,
    ) -> Result<String, SubscriptionError> {
        let Some((account_id, redirect_uri, pair)) = self.sessions.consume(state, self.now())
        else {
            return Err(SubscriptionError::InvalidCallback);
        };
        let tokens = self
            .provider
            .exchange_code(
                &self.http,
                &self.client_id,
                &redirect_uri,
                code,
                &pair.verifier,
            )
            .await?;
        self.complete_authorization(&account_id, tokens).await?;
        Ok(account_id)
    }

    /// 出站取凭证：vendor 的 Active/重试到期账号按游标轮换，命中即做
    /// 临期抢占刷新（单飞）；候选刷新失败（内部已降级/摘除）自动换下一
    /// 候选，全员失败 → None（调用方回落 key 池）。
    pub async fn credential_for(&self, vendor_id: &str) -> Option<(String, String)> {
        let candidates: Vec<String> = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let now = self.now();
            let candidates: Vec<String> = inner
                .order
                .iter()
                .filter(|id| {
                    inner.accounts.get(*id).is_some_and(|a| {
                        a.vendor_id == vendor_id
                            && match &a.state {
                                AccountState::Active => true,
                                AccountState::Degraded { retry_at_ms } => now >= *retry_at_ms,
                                AccountState::PendingAuthorization => false,
                            }
                    })
                })
                .cloned()
                .collect();
            if candidates.is_empty() {
                return None;
            }
            let cursor = inner.cursors.entry(vendor_id.to_string()).or_insert(0);
            let start = *cursor % candidates.len();
            *cursor = start + 1;
            // 轮换起点起环序展开（锁内只做索引，刷新在锁外逐个尝试）
            let rotated: Vec<String> = (0..candidates.len())
                .map(|offset| candidates[(start + offset) % candidates.len()].clone())
                .collect();
            rotated
        };
        for account_id in &candidates {
            // 失败账号已在池内降级/摘除（on_refresh_failure 内聚）；换下一候选
            if let Ok(access) = self.ensure_fresh(account_id).await {
                return Some((account_id.clone(), access));
            }
        }
        None
    }

    /// 上游 401 处置：强制刷新一次（单飞）。成功 → 新 access token；
    /// 失败 → 账号降级/摘除（见 [`Self::on_refresh_failure`]），返回错误，
    /// 调用方走既有 Fallback 换路。
    pub async fn refresh_on_unauthorized(
        &self,
        account_id: &str,
    ) -> Result<String, SubscriptionError> {
        self.ensure_fresh_force(account_id).await
    }

    /// admin 吊销：内存摘除（vault 条目由调用方经 store 层删除）。
    /// 吊销前处于 Degraded 的账号扣减降级 gauge（gauge 口径 = 当前池内
    /// Degraded 账号数）。返回是否确有删除。
    pub fn remove(&self, account_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(account) = inner.accounts.remove(account_id) else {
            return false;
        };
        inner.order.retain(|id| id != account_id);
        if matches!(account.state, AccountState::Degraded { .. }) {
            self.metrics.record_subscription_account_degraded_leave();
        }
        true
    }

    /// 降级账号（401 强制刷新成功后仍被上游拒绝的处置）：进入 30 分钟
    /// 冷却，gauge +1。冷却到点由刷新路径重试一次，仍败摘除。
    pub fn degrade_account(&self, account_id: &str) {
        self.metrics.record_subscription_refresh_fail();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(account) = inner.accounts.get_mut(account_id) else {
            return;
        };
        if matches!(account.state, AccountState::Degraded { .. }) {
            return;
        }
        account.state = AccountState::Degraded {
            retry_at_ms: self.now() + DEGRADE_RETRY_AFTER_MS,
        };
        self.metrics.record_subscription_account_degraded_enter();
    }

    /// 池持有的 vault 句柄（admin 吊销清理 refresh token 条目用）；
    /// None = 无库形态（refresh token 仅进程内，重启即失效）。
    pub fn vault_store(&self) -> Option<Arc<dyn KeyStore>> {
        self.store.clone()
    }

    /// admin 列表（脱敏快照）。
    pub fn summaries(&self) -> Vec<AccountSummary> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .order
            .iter()
            .filter_map(|id| {
                inner.accounts.get(id).map(|a| AccountSummary {
                    id: id.clone(),
                    vendor_id: a.vendor_id.clone(),
                    state: a.state.clone(),
                    expires_at_ms: a.token.as_ref().map(|t| t.expires_at_ms),
                    declared: a.declared,
                })
            })
            .collect()
    }

    /// 单飞刷新核：per-account 锁串行 + 拿锁后复查。降级账号到点重试时
    /// 强制刷新（Degraded 的唯一出路是刷新成功）。
    async fn ensure_fresh(&self, account_id: &str) -> Result<String, SubscriptionError> {
        let force = {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let account = inner
                .accounts
                .get(account_id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?;
            match &account.state {
                AccountState::Degraded { retry_at_ms } => {
                    if self.now() < *retry_at_ms {
                        return Err(SubscriptionError::Degraded);
                    }
                    true
                }
                _ => false,
            }
        };
        self.ensure_fresh_impl(account_id, force).await
    }

    async fn ensure_fresh_force(&self, account_id: &str) -> Result<String, SubscriptionError> {
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let account = inner
                .accounts
                .get(account_id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?;
            if let AccountState::Degraded { retry_at_ms } = &account.state {
                if self.now() < *retry_at_ms {
                    return Err(SubscriptionError::Degraded);
                }
            }
        }
        self.ensure_fresh_impl(account_id, true).await
    }

    async fn ensure_fresh_impl(
        &self,
        account_id: &str,
        force: bool,
    ) -> Result<String, SubscriptionError> {
        let lock = {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner
                .accounts
                .get(account_id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?
                .refresh_lock
                .clone()
        };
        let _guard = lock.lock().await;
        let now = self.now();

        // 拿锁后复查（单飞快速路径：并发第二请求直接取新 token）
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let account = inner
                .accounts
                .get(account_id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?;
            if let Some(token) = &account.token {
                if !force
                    && account.state == AccountState::Active
                    && token.expires_at_ms.saturating_sub(now) > PREEMPTIVE_REFRESH_WINDOW_MS
                {
                    return Ok(token.access.expose_secret().to_owned());
                }
            }
        }

        // 刷新：refresh token 只从 vault 读（内存不持 refresh token）
        let refresh_token = match &self.store {
            Some(s) => store::load_refresh_token(s, account_id)
                .await
                .map_err(|e| {
                    self.on_refresh_failure(account_id);
                    SubscriptionError::Transport(e.to_string())
                })?,
            None => {
                self.on_refresh_failure(account_id);
                return Err(SubscriptionError::Transport(
                    "vault store not wired".to_string(),
                ));
            }
        };
        let tokens = match self
            .provider
            .refresh(&self.http, &self.client_id, refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => tokens,
            Err(e) => {
                self.on_refresh_failure(account_id);
                return Err(e);
            }
        };

        // 成功：refresh token 落 vault（轮换回存），内存换新 access
        if let Some(s) = &self.store {
            store::save_refresh_token(s, account_id, &tokens.refresh_token)
                .await
                .map_err(|e| {
                    self.on_refresh_failure(account_id);
                    SubscriptionError::Transport(e.to_string())
                })?;
        }
        let now = self.now();
        let expires_at_ms = now + tokens.expires_in_secs.saturating_mul(1000);
        let recovered = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let account = inner
                .accounts
                .get_mut(account_id)
                .ok_or_else(|| SubscriptionError::UnknownAccount(account_id.to_string()))?;
            let recovered = matches!(account.state, AccountState::Degraded { .. });
            account.state = AccountState::Active;
            account.token = Some(TokenEntry {
                access: tokens.access_token.clone(),
                expires_at_ms,
            });
            recovered
        };
        if recovered {
            self.metrics.record_subscription_account_degraded_leave();
        }
        Ok(access_token_of(&tokens))
    }

    /// 刷新失败处置：降级冷却 30 分钟；降级中到点重试再败 → 摘除。
    /// 每次失败都计 `subscription_refresh_fail_total`；进入降级 gauge +1，
    /// 降级账号被摘除 gauge −1。
    fn on_refresh_failure(&self, account_id: &str) {
        self.metrics.record_subscription_refresh_fail();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = self.now();
        let Some(account) = inner.accounts.get_mut(account_id) else {
            return;
        };
        match &account.state {
            AccountState::Degraded { retry_at_ms } if now >= *retry_at_ms => {
                inner.accounts.remove(account_id);
                inner.order.retain(|id| id != account_id);
                self.metrics.record_subscription_account_degraded_leave();
            }
            _ => {
                account.state = AccountState::Degraded {
                    retry_at_ms: now + DEGRADE_RETRY_AFTER_MS,
                };
                self.metrics.record_subscription_account_degraded_enter();
            }
        }
    }

    /// 测试注入：固定时钟（Some(ms)）与恢复系统时钟（None）。
    #[cfg(test)]
    pub(crate) fn set_clock(&self, ms: Option<u64>) {
        *self
            .clock_override
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = ms;
    }

    /// 测试观测：注册表内账号数。
    #[cfg(test)]
    pub(crate) fn account_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .accounts
            .len()
    }
}

fn access_token_of(tokens: &TokenSet) -> String {
    tokens.access_token.expose_secret().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::subscription::anthropic::AnthropicProvider;
    use secrecy::ExposeSecret;

    async fn pool_with_store(
        server_url: &str,
    ) -> (
        AccountPool,
        Arc<GlobalMetrics>,
        Arc<dyn KeyStore>,
        tempfile::TempDir,
    ) {
        let provider = Arc::new(AnthropicProvider::with_endpoints(
            format!("{server_url}/oauth/authorize"),
            format!("{server_url}/v1/oauth/token"),
        ));
        let metrics = Arc::new(GlobalMetrics::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn KeyStore> = Arc::new(
            crate::vault::FileKeyStore::open(vec![7u8; 32], dir.path().join("vault.bin"))
                .expect("file store"),
        );
        let pool = AccountPool::new(
            provider,
            reqwest::Client::new(),
            "client-123".to_string(),
            Some(store.clone()),
            metrics.clone(),
        );
        (pool, metrics, store, dir)
    }

    fn token_body(access: &str, refresh: &str, expires: u64) -> String {
        format!(
            r#"{{"access_token":"{access}","refresh_token":"{refresh}","expires_in":{expires}}}"#
        )
    }

    /// 经真实链路播种：ensure_account + complete_authorization（refresh
    /// token 落 vault），与生产 callback 同路径。
    async fn seed_authorized(
        pool: &AccountPool,
        id: &str,
        vendor: &str,
        access: &str,
        refresh: &str,
        expires_in_secs: u64,
    ) {
        pool.register_declared(id, vendor).await;
        pool.complete_authorization(
            id,
            TokenSet {
                access_token: SecretString::new(access.to_string().into()),
                refresh_token: SecretString::new(refresh.to_string().into()),
                expires_in_secs,
            },
        )
        .await
        .expect("complete");
    }

    /// 单飞契约：两个并发临期请求只打一次刷新端点，且拿到同一新 token；
    /// 刷新后远期有效的第三次请求零刷新直取内存。
    #[tokio::test]
    async fn concurrent_requests_trigger_single_refresh() {
        let mut server = mockito::Server::new_async().await;
        let (pool, _metrics, store, _dir) = pool_with_store(&server.url()).await;
        pool.set_clock(Some(1_000_000));
        // 4 分钟后到期 → 落在 5 分钟抢占窗内
        seed_authorized(&pool, "sub-1", "v1", "at-old", "rt-v1", 240).await;
        assert!(
            store::load_refresh_token(&store, "sub-1").await.is_ok(),
            "播种后 vault 必有 refresh token"
        );

        let refresh = server
            .mock("POST", "/v1/oauth/token")
            .expect(1)
            .with_status(200)
            .with_body(token_body("at-new", "rt-new", 3600))
            .create_async()
            .await;

        let (a, b) = tokio::join!(pool.credential_for("v1"), pool.credential_for("v1"));
        refresh.assert_async().await;
        let (id_a, token_a) = a.expect("first credential");
        let (_, token_b) = b.expect("second credential");
        assert_eq!(id_a, "sub-1");
        assert_eq!(token_a, "at-new");
        assert_eq!(token_b, "at-new", "并发双方拿到同一新 token");

        // 刷新后 token 远期有效 → 第三次请求零刷新（expect(1) 已满，多打即断言失败）
        let (_, token_c) = pool.credential_for("v1").await.expect("third");
        assert_eq!(token_c, "at-new");
        refresh.assert_async().await;
    }

    /// 刷新失败处置时序：401 → Degraded（冷却 30 分钟、计数 +1、gauge +1、
    /// 池不供凭证＝请求走既有 Fallback）；冷却到点重试仍败 → 摘除；
    /// 冷却到点重试成功 → 恢复 Active（gauge 回落）。
    #[tokio::test]
    async fn refresh_failure_degrades_account_and_fallback_continues() {
        let mut server = mockito::Server::new_async().await;
        let (pool, metrics, _store, _dir) = pool_with_store(&server.url()).await;
        pool.set_clock(Some(2_000_000));
        seed_authorized(&pool, "sub-1", "v1", "at-old", "rt-v1", 3_600).await;

        // expect(2)：首败 + 冷却后重试败；冷却期若泄漏请求会打出第 3 次命中而失败
        let rejected = server
            .mock("POST", "/v1/oauth/token")
            .expect(2)
            .with_status(401)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;

        let err = pool
            .refresh_on_unauthorized("sub-1")
            .await
            .expect_err("401 必须失败");
        assert!(matches!(err, SubscriptionError::Oauth { status: 401 }));
        assert_eq!(
            pool.summaries()[0].state,
            AccountState::Degraded {
                retry_at_ms: 2_000_000 + DEGRADE_RETRY_AFTER_MS
            }
        );
        assert_eq!(
            metrics.snapshot().subscription_refresh_fail_total,
            1,
            "刷新失败计数 +1"
        );
        assert_eq!(metrics.snapshot().subscription_accounts_degraded, 1);

        // 冷却期内：池不供该 vendor 凭证（调用方回落 key 池/Fallback 链），
        // 强制刷新也被冷却拒绝
        assert!(pool.credential_for("v1").await.is_none());
        assert!(matches!(
            pool.refresh_on_unauthorized("sub-1").await,
            Err(SubscriptionError::Degraded)
        ));

        // 冷却到点：重试一次仍败 → 摘除（注册表清空，池永不供凭证）
        pool.set_clock(Some(2_000_000 + DEGRADE_RETRY_AFTER_MS));
        let err = pool
            .refresh_on_unauthorized("sub-1")
            .await
            .expect_err("重试仍败");
        assert!(matches!(err, SubscriptionError::Oauth { status: 401 }));
        assert_eq!(pool.account_count(), 0, "仍败摘除");
        assert!(pool.credential_for("v1").await.is_none());
        assert_eq!(
            metrics.snapshot().subscription_refresh_fail_total,
            2,
            "两次失败各计一次"
        );
        assert_eq!(
            metrics.snapshot().subscription_accounts_degraded,
            0,
            "摘除扣减降级 gauge"
        );
        rejected.assert_async().await;

        // 对照：冷却到点重试成功 → 恢复 Active
        seed_authorized(&pool, "sub-2", "v1", "at-old", "rt-v2", 3_600).await;
        pool.set_clock(Some(3_000_000));
        let fail_once = server
            .mock("POST", "/v1/oauth/token")
            .expect(1)
            .with_status(500)
            .with_body(r#"{"error":"upstream"}"#)
            .create_async()
            .await;
        pool.refresh_on_unauthorized("sub-2")
            .await
            .expect_err("先败一次");
        fail_once.assert_async().await;
        assert_eq!(metrics.snapshot().subscription_accounts_degraded, 1);

        pool.set_clock(Some(3_000_000 + DEGRADE_RETRY_AFTER_MS));
        let recovered = server
            .mock("POST", "/v1/oauth/token")
            .expect(1)
            .with_status(200)
            .with_body(token_body("at-fresh", "rt-fresh", 3600))
            .create_async()
            .await;
        let token = pool
            .refresh_on_unauthorized("sub-2")
            .await
            .expect("重试成功");
        recovered.assert_async().await;
        assert_eq!(token, "at-fresh");
        assert_eq!(pool.summaries()[0].state, AccountState::Active);
        assert_eq!(
            metrics.snapshot().subscription_accounts_degraded,
            0,
            "恢复扣减降级 gauge"
        );
    }

    /// 轮换与登记契约：双账号按游标交替；未授权账号不参与出站；
    /// ensure_account 复用声明账号、拒绝 vendor 错配、ad-hoc id 避让撞名；
    /// 列表脱敏（零 token 片段）。
    #[tokio::test]
    async fn pool_rotates_and_gates_unauthorized_accounts() {
        let server = mockito::Server::new_async().await;
        let (pool, _metrics, _store, _dir) = pool_with_store(&server.url()).await;
        pool.set_clock(Some(1_000_000));
        seed_authorized(&pool, "sub-1", "v1", "at-1", "rt-1", 3_600).await;
        seed_authorized(&pool, "sub-2", "v1", "at-2", "rt-2", 3_600).await;

        let (id1, t1) = pool.credential_for("v1").await.expect("first");
        let (id2, t2) = pool.credential_for("v1").await.expect("second");
        assert_ne!(id1, id2, "双账号轮换");
        assert_eq!(t1, "at-1");
        assert_eq!(t2, "at-2");

        // vendor 错配与未授权账号
        assert!(pool.credential_for("v2").await.is_none());
        assert!(matches!(
            pool.ensure_account("v1", Some("ghost")),
            Err(SubscriptionError::UnknownAccount(_))
        ));
        let id = pool.ensure_account("v1", None).expect("ad-hoc");
        assert_eq!(id, "sub-3", "ad-hoc id 自增且避让已注册账号");
        assert_eq!(
            pool.summaries()
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.state.clone()),
            Some(AccountState::PendingAuthorization)
        );
        assert!(
            pool.credential_for("v1").await.is_some(),
            "待授权账号不影响在服账号轮换"
        );
        assert_eq!(pool.summaries().len(), 3, "列表含待授权账号（脱敏字段面）");
        let summary_json = serde_json::to_string(&pool.summaries()).expect("serialize");
        assert!(
            !summary_json.contains("at-1") && !summary_json.contains("at-2"),
            "列表序列化零 token 片段: {summary_json}"
        );
    }

    /// 吊销降级账号：降级 gauge 扣减（admin 吊销 Degraded 账号后
    /// `/admin/metrics` 的 `subscription_accounts_degraded` 不得永久虚高）。
    #[tokio::test]
    async fn remove_decremented_degraded_gauge() {
        let mut server = mockito::Server::new_async().await;
        let (pool, metrics, _store, _dir) = pool_with_store(&server.url()).await;
        pool.set_clock(Some(1_000_000));
        seed_authorized(&pool, "sub-1", "v1", "at-old", "rt-v1", 3_600).await;
        let rejected = server
            .mock("POST", "/v1/oauth/token")
            .expect(1)
            .with_status(401)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;
        pool.refresh_on_unauthorized("sub-1")
            .await
            .expect_err("先败一次进降级");
        rejected.assert_async().await;
        assert_eq!(metrics.snapshot().subscription_accounts_degraded, 1);

        assert!(pool.remove("sub-1"), "吊销确有删除");
        assert_eq!(
            metrics.snapshot().subscription_accounts_degraded,
            0,
            "吊销 Degraded 账号必须扣减 gauge"
        );
        // 吊销 Active 账号不动 gauge；重复吊销返回 false
        seed_authorized(&pool, "sub-2", "v1", "at-x", "rt-x", 3_600).await;
        assert!(pool.remove("sub-2"));
        assert_eq!(metrics.snapshot().subscription_accounts_degraded, 0);
        assert!(!pool.remove("sub-2"), "重复吊销返回 false");
    }

    /// 候选轮换跳故障：首候选刷新失败（内部已降级）→ 换下一候选服务本
    /// 请求，不再整请求回落 key 池。
    #[tokio::test]
    async fn credential_for_skips_failing_candidate() {
        let mut server = mockito::Server::new_async().await;
        let (pool, _metrics, _store, _dir) = pool_with_store(&server.url()).await;
        pool.set_clock(Some(1_000_000));
        seed_authorized(&pool, "sub-1", "v1", "at-dead", "rt-1", 3_600).await;
        seed_authorized(&pool, "sub-2", "v1", "at-live", "rt-2", 3_600).await;
        let rejected = server
            .mock("POST", "/v1/oauth/token")
            .expect(1)
            .with_status(401)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .create_async()
            .await;

        let (id, token) = pool.credential_for("v1").await.expect("第二候选服务");
        rejected.assert_async().await;
        assert_eq!(id, "sub-2", "首候选失败必须换下一候选");
        assert_eq!(token, "at-live");
        // sub-1 已降级出候选：下轮直接 sub-2，零刷新
        let (id, token) = pool.credential_for("v1").await.expect("续供");
        assert_eq!(id, "sub-2");
        assert_eq!(token, "at-live");
    }

    /// TokenSet 的 Debug 面零明文（防手滑日志）。
    #[test]
    fn token_debug_stays_redacted() {
        let tokens = TokenSet {
            access_token: SecretString::new("at-plain-marker".into()),
            refresh_token: SecretString::new("rt-plain-marker".into()),
            expires_in_secs: 60,
        };
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("at-plain-marker"));
        assert!(!rendered.contains("rt-plain-marker"));
        let _ = ExposeSecret::expose_secret(&tokens.access_token);
    }
}
