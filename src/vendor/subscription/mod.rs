// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 订阅账号上游（`subscription-vendor` feature，不进 `full`）。
//!
//! 合规披露：以订阅账号（OAuth）作服务端出站上游可能违反厂商服务条款，
//! 本模块只实现标准 OAuth 授权码 + PKCE 流程，不含任何绕过限制的技术；
//! 启用与否及合规风险由部署者自行评估（README「Subscriptions」警示段）。
//!
//! 双门控：编译门为本 feature；运行时门为 `[subscription].enabled`。
//! refresh token 只存 vault（cocoon AEAD，键 `sub:{account_id}`）；
//! access token 仅驻进程内存，到期前 5 分钟单飞抢占刷新。

pub mod anthropic;
pub mod oauth;
pub mod pool;
pub mod store;

use secrecy::SecretString;

/// OAuth token 集（token 端点响应解析产物；双 token 均按 secret 处理，
/// Debug 零明文——`SecretString` 自带 `[REDACTED]`）。`expires_in_secs`
/// 为上游声明的 access token 寿命。
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: SecretString,
    pub refresh_token: SecretString,
    pub expires_in_secs: u64,
}

/// 订阅供应商 OAuth 错误：Display 恒英文规范串（对齐 error 双轨惯例），
/// 只携带状态码与字段名——响应原文可能含 token，绝不入错误消息。
#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("Subscription oauth endpoint error: status {status}")]
    Oauth { status: u16 },
    #[error("Subscription token response invalid: missing field {0}")]
    InvalidTokenResponse(&'static str),
    #[error("Subscription transport error: {0}")]
    Transport(String),
    #[error("Subscription account not found: {0}")]
    UnknownAccount(String),
    #[error("Subscription account is in degraded cooldown; retry later")]
    Degraded,
    #[error("Subscription callback rejected: unknown, expired, or already-used state")]
    InvalidCallback,
}

/// 订阅供应商抽象：一家 OAuth 契约一个实现（首个：Anthropic Claude）。
/// 端点常量与请求形态在实现文件；`http` 客户端由调用方注入（连接池复用）。
#[async_trait::async_trait]
pub trait SubscriptionProvider: Send + Sync {
    /// 供应商标识（admin 面与配置取值，如 "anthropic"）。
    fn kind(&self) -> &'static str;
    /// 授权申请 scope 集合（按供应商公开 OAuth 契约固定）。
    fn scopes(&self) -> &'static [&'static str];
    /// 拼授权 URL：response_type=code、client_id、redirect_uri、scope、
    /// state、code_challenge（S256）。
    fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        challenge: &str,
    ) -> String;
    /// 授权码换 token 集（form：grant_type=authorization_code）。
    async fn exchange_code(
        &self,
        http: &reqwest::Client,
        client_id: &str,
        redirect_uri: &str,
        code: &str,
        verifier: &str,
    ) -> Result<TokenSet, SubscriptionError>;
    /// refresh token 换新 token 集（form：grant_type=refresh_token）；
    /// 上游轮换时返回新 refresh token，调用方必须回存 vault。
    async fn refresh(
        &self,
        http: &reqwest::Client,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<TokenSet, SubscriptionError>;
}
