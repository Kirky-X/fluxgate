// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Anthropic Claude OAuth 供应商（订阅账号上游首家实现）。
//!
//! 端点按 Anthropic 公开 OAuth 契约固定（claude.ai/oauth/authorize +
//! console.anthropic.com/v1/oauth/token）；`with_endpoints` 仅供测试注入
//! mock 上游。scope 集合对齐 Claude CLI 公开客户端口径。响应原文可能含
//! token，错误消息只携带状态码/字段名，绝不回显 body。

use reqwest::Client;
use secrecy::SecretString;

#[cfg(test)]
use super::oauth::base64url_nopad;
use super::{SubscriptionError, SubscriptionProvider, TokenSet};

/// Anthropic 授权端点（公开 OAuth 契约）。
pub const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// Anthropic token 端点（公开 OAuth 契约）。
pub const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Anthropic Claude OAuth 供应商。生产构造走 [`AnthropicProvider::new`]；
/// 端点差异只存在于测试（mock 上游）。
pub struct AnthropicProvider {
    authorize_endpoint: String,
    token_endpoint: String,
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicProvider {
    pub fn new() -> Self {
        Self {
            authorize_endpoint: ANTHROPIC_AUTHORIZE_URL.to_string(),
            token_endpoint: ANTHROPIC_TOKEN_URL.to_string(),
        }
    }

    /// 测试注入面：mock 上游替换两端点（生产代码禁用）。
    #[cfg(test)]
    pub(crate) fn with_endpoints(authorize_endpoint: String, token_endpoint: String) -> Self {
        Self {
            authorize_endpoint,
            token_endpoint,
        }
    }
}

/// 查询串值编码：RFC 3986 unreserved 之外全部百分号转义（state/verifier/
/// redirect_uri 均落此域；不引入 url 依赖——authorization URL 是本模块
/// 唯一拼接点）。
fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// token 响应解析：三字段缺一即拒（refresh 缺失会让账号在下次刷新时
/// 失联，宁可当场拒绝）。
fn parse_token_response(body: &str) -> Result<TokenSet, SubscriptionError> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|_| SubscriptionError::InvalidTokenResponse("json"))?;
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or(SubscriptionError::InvalidTokenResponse("access_token"))?;
    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or(SubscriptionError::InvalidTokenResponse("refresh_token"))?;
    let expires_in_secs = json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .ok_or(SubscriptionError::InvalidTokenResponse("expires_in"))?;
    Ok(TokenSet {
        access_token: SecretString::new(access_token.to_string().into()),
        refresh_token: SecretString::new(refresh_token.to_string().into()),
        expires_in_secs,
    })
}

/// 表单体编码：`k=v` 串接（同款 unreserved 转义）。reqwest 的 `form`
/// 方法在本仓 feature 集里未启用（`form` feature 携 serde_urlencoded），
/// OAuth 表单是本模块唯一表单点，不为此扩共享依赖面。
fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", query_encode(key), query_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

#[async_trait::async_trait]
impl SubscriptionProvider for AnthropicProvider {
    fn kind(&self) -> &'static str {
        "anthropic"
    }

    fn scopes(&self) -> &'static [&'static str] {
        &["org:create_api_key", "user:profile", "user:inference"]
    }

    fn authorize_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        challenge: &str,
    ) -> String {
        format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            self.authorize_endpoint,
            query_encode(client_id),
            query_encode(redirect_uri),
            query_encode(&self.scopes().join(" ")),
            query_encode(state),
            query_encode(challenge),
        )
    }

    async fn exchange_code(
        &self,
        http: &Client,
        client_id: &str,
        redirect_uri: &str,
        code: &str,
        verifier: &str,
    ) -> Result<TokenSet, SubscriptionError> {
        let body = form_body(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ]);
        let response = http
            .post(&self.token_endpoint)
            .header(reqwest::header::CONTENT_TYPE, FORM_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .map_err(|e| SubscriptionError::Transport(e.to_string()))?;
        token_response(response).await
    }

    async fn refresh(
        &self,
        http: &Client,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<TokenSet, SubscriptionError> {
        let body = form_body(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ]);
        let response = http
            .post(&self.token_endpoint)
            .header(reqwest::header::CONTENT_TYPE, FORM_CONTENT_TYPE)
            .body(body)
            .send()
            .await
            .map_err(|e| SubscriptionError::Transport(e.to_string()))?;
        token_response(response).await
    }
}

/// token 端点响应统一处理：非 2xx → 状态码错误；2xx → 解析三字段。
async fn token_response(response: reqwest::Response) -> Result<TokenSet, SubscriptionError> {
    let status = response.status();
    if !status.is_success() {
        return Err(SubscriptionError::Oauth {
            status: status.as_u16(),
        });
    }
    let body = response
        .text()
        .await
        .map_err(|e| SubscriptionError::Transport(e.to_string()))?;
    parse_token_response(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// 三端点契约：authorize URL 参数形态、exchange 表单断言 + 解析、
    /// refresh 以 refresh_token 换新集、缺字段响应当场拒绝。
    #[tokio::test]
    async fn token_response_parsed_and_refresh_uses_refresh_token() {
        let mut server = mockito::Server::new_async().await;
        let provider = AnthropicProvider::with_endpoints(
            format!("{}/oauth/authorize", server.url()),
            format!("{}/v1/oauth/token", server.url()),
        );
        let http = Client::new();
        let redirect = "http://127.0.0.1:9/callback";

        // authorize URL：七个参数全在、S256 方法、scope 空格转义
        let url = provider.authorize_url("client-123", redirect, "state-x", "challenge-y");
        assert!(url.starts_with(&format!("{}/oauth/authorize?", server.url())));
        for needle in [
            "response_type=code",
            "client_id=client-123",
            &format!("redirect_uri={}", query_encode(redirect)),
            "scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference",
            "state=state-x",
            "code_challenge=challenge-y",
            "code_challenge_method=S256",
        ] {
            assert!(url.contains(needle), "authorize URL 缺 {needle}: {url}");
        }

        // exchange：form 五字段断言 + 三字段解析
        let exchange = server
            .mock("POST", "/v1/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "authorization_code".into()),
                mockito::Matcher::UrlEncoded("code".into(), "the-code".into()),
                mockito::Matcher::UrlEncoded("client_id".into(), "client-123".into()),
                mockito::Matcher::UrlEncoded("redirect_uri".into(), redirect.into()),
                mockito::Matcher::UrlEncoded("code_verifier".into(), "the-verifier".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600}"#)
            .create_async()
            .await;
        let tokens = provider
            .exchange_code(&http, "client-123", redirect, "the-code", "the-verifier")
            .await
            .expect("exchange 成功");
        exchange.assert_async().await;
        assert_eq!(tokens.access_token.expose_secret(), "at-1");
        assert_eq!(tokens.refresh_token.expose_secret(), "rt-1");
        assert_eq!(tokens.expires_in_secs, 3600);

        // refresh：form 三字段（grant_type=refresh_token），新集解析
        let refresh = server
            .mock("POST", "/v1/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "refresh_token".into()),
                mockito::Matcher::UrlEncoded("refresh_token".into(), "rt-1".into()),
                mockito::Matcher::UrlEncoded("client_id".into(), "client-123".into()),
            ]))
            .with_status(200)
            .with_body(r#"{"access_token":"at-2","refresh_token":"rt-2","expires_in":7200}"#)
            .create_async()
            .await;
        let refreshed = provider
            .refresh(&http, "client-123", "rt-1")
            .await
            .expect("refresh 成功");
        refresh.assert_async().await;
        assert_eq!(refreshed.access_token.expose_secret(), "at-2");
        assert_eq!(refreshed.refresh_token.expose_secret(), "rt-2");
        assert_eq!(refreshed.expires_in_secs, 7200);

        // 缺 refresh_token 的响应当场拒绝（防账号下次刷新失联）
        let incomplete = server
            .mock("POST", "/v1/oauth/token")
            .with_status(200)
            .with_body(r#"{"access_token":"at-3"}"#)
            .create_async()
            .await;
        let err = provider
            .refresh(&http, "client-123", "rt-2")
            .await
            .expect_err("缺字段必须拒绝");
        incomplete.assert_async().await;
        assert!(matches!(err, SubscriptionError::InvalidTokenResponse(_)));

        // 上游 4xx → 状态码错误，body 不入错误消息
        let rejected = server
            .mock("POST", "/v1/oauth/token")
            .with_status(401)
            .with_body(r#"{"error":"invalid_grant","error_description":"rt-2 bad"}"#)
            .create_async()
            .await;
        let err = provider
            .refresh(&http, "client-123", "rt-2")
            .await
            .expect_err("4xx 必须拒绝");
        rejected.assert_async().await;
        assert!(matches!(err, SubscriptionError::Oauth { status: 401 }));
        assert!(!err.to_string().contains("rt-2"), "错误消息零 token");
    }

    /// query_encode：unreserved 保留、其余百分号转义。
    #[test]
    fn query_encode_escapes_beyond_unreserved() {
        assert_eq!(query_encode("aB3-._~"), "aB3-._~");
        assert_eq!(query_encode("user:inference"), "user%3Ainference");
        assert_eq!(query_encode(" "), "%20");
        assert_eq!(query_encode("http://h:9/p"), "http%3A%2F%2Fh%3A9%2Fp");
    }

    /// base64url 编码导出复用（PKCE 主实现在 oauth.rs，此处防导出断链）。
    #[test]
    fn base64url_helper_remains_wired_to_oauth() {
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
    }
}
