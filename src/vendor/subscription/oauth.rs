// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! PKCE 授权码会话状态机（subscription-upstream）。
//!
//! `issue` 产出 (state, PKCE pair)（S256：challenge = BASE64URL(SHA256(verifier))，
//! 无 padding）；`consume` 单次使用——命中即移除，TTL（10 分钟）整点过期，
//! 过期/重放/未知 state 一律拒绝。state 与 verifier 只驻进程内存：授权回调
//! 必须打回同一实例（单实例约束，README 警示段披露）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// state 会话 TTL：10 分钟（design 决策 1：code_verifier/session 存内存 +
/// 10 分钟过期）。
pub const SESSION_TTL: Duration = Duration::from_secs(600);

/// PKCE pair：`verifier` 留给 token 交换，`challenge` 进授权 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

/// 授权会话（内部态；消费即移除，禁止重放）。`account_id` 绑定本次授权
/// 目标账号，回调换码后凭它落库凭证。
struct Session {
    account_id: String,
    /// authorize 时的 redirect_uri（token 交换必须逐字一致，OAuth 契约）。
    redirect_uri: String,
    pair: PkcePair,
    issued_at_ms: u64,
}

/// OAuth 授权会话表（进程内单实例；Sync：admin handler 与出站共享）。
pub struct OauthSessionStore {
    sessions: Mutex<HashMap<String, Session>>,
}

impl Default for OauthSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OauthSessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 发起授权：生成 (state, PKCE pair) 并登记会话（绑定目标账号）；
    /// `now_ms` 由调用方传入（出站用系统时钟，测试注入假时钟做确定性断言）。
    /// 发起侧顺手清扫过期会话——authorize 循环而 callback 不到场时表仍有界。
    pub fn issue(&self, account_id: &str, redirect_uri: &str, now_ms: u64) -> (String, PkcePair) {
        let pair = pkce_pair();
        let state = random_hex(16);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| {
            now_ms.saturating_sub(session.issued_at_ms) < SESSION_TTL.as_millis() as u64
        });
        sessions.insert(
            state.clone(),
            Session {
                account_id: account_id.to_string(),
                redirect_uri: redirect_uri.to_string(),
                pair: pair.clone(),
                issued_at_ms: now_ms,
            },
        );
        (state, pair)
    }

    /// 回调换码：state 未过期且首次使用 → 取出 (账号, PKCE pair)；过期 /
    /// 重放 / 未知 state → None（过期条目顺手清理，防表无界增长）。
    pub fn consume(&self, state: &str, now_ms: u64) -> Option<(String, String, PkcePair)> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| {
            now_ms.saturating_sub(session.issued_at_ms) < SESSION_TTL.as_millis() as u64
        });
        sessions
            .remove(state)
            .map(|session| (session.account_id, session.redirect_uri, session.pair))
    }
}

/// 随机十六进制串（`bytes` 字节熵 → 2×`bytes` 字符；指纹同款口径）。
pub(crate) fn random_hex(bytes: usize) -> String {
    use rand::RngExt;
    let mut buf = vec![0u8; bytes];
    rand::rng().fill(&mut buf[..]);
    hex::encode(buf)
}

/// 生成 PKCE pair：verifier = 48 字节随机的 base64url（64 字符，落在
/// RFC 7636 43-128 字符域内）；challenge = BASE64URL(SHA256(verifier)) 无 padding。
pub(crate) fn pkce_pair() -> PkcePair {
    use rand::RngExt;
    let mut buf = [0u8; 48];
    rand::rng().fill(&mut buf[..]);
    let verifier = base64url_nopad(&buf);
    let digest = Sha256::digest(verifier.as_bytes());
    PkcePair {
        verifier,
        challenge: base64url_nopad(&digest),
    }
}

/// RFC 4648 base64url（无 padding）——挑战串只需编码 32 字节摘要这一处，
/// 不为单点引入 base64 依赖。
pub(crate) fn base64url_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态机契约：state 未过期换码成功、10 分钟整点过期、重放拒绝。
    #[test]
    fn authorize_state_expires_in_10min_and_callback_single_use() {
        let store = OauthSessionStore::new();
        let t0 = 1_000_000u64;

        let (state, pair) = store.issue("sub-1", "http://127.0.0.1:9/cb", t0);
        assert_eq!(pair.verifier.len(), 64, "48 字节 → 64 base64url 字符");
        assert!(
            pair.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier 落在 base64url 字符集"
        );
        let (other, _) = store.issue("sub-2", "http://127.0.0.1:9/cb", t0);
        assert_ne!(state, other, "state 独立随机，不与其它会话撞车");

        let ttl_ms = SESSION_TTL.as_millis() as u64;
        // 未过期（TTL 内 1ms）首次消费成功：带回绑定账号
        assert_eq!(
            store.consume(&state, t0 + ttl_ms - 1),
            Some((
                "sub-1".to_string(),
                "http://127.0.0.1:9/cb".to_string(),
                pair.clone()
            ))
        );
        // 重放拒绝（消费即移除）
        assert_eq!(store.consume(&state, t0 + ttl_ms), None);

        // 过期拒绝：TTL 整点即过期
        let (expired, _) = store.issue("sub-3", "http://127.0.0.1:9/cb", t0);
        assert_eq!(
            store.consume(&expired, t0 + ttl_ms),
            None,
            "10 分钟整点过期"
        );

        // 过期清理不误伤新会话；未知 state 拒绝
        let (fresh, fresh_pair) = store.issue("sub-9", "http://h:1/cb", t0 + 1);
        assert_eq!(
            store.consume(&fresh, t0 + 2),
            Some(("sub-9".to_string(), "http://h:1/cb".to_string(), fresh_pair))
        );
        assert_eq!(store.consume("no-such-state", t0 + 2), None);
    }

    /// 发起侧清扫：authorize 循环而 callback 不到场时，过期会话在下次
    /// issue 时清理——表不无界增长（消费侧清扫同保留）。
    #[test]
    fn issue_sweeps_expired_sessions() {
        let store = OauthSessionStore::new();
        let t0 = 1_000_000u64;
        let ttl_ms = SESSION_TTL.as_millis() as u64;
        let (_s1, _) = store.issue("sub-1", "http://h:1/cb", t0);
        let (_s2, _) = store.issue("sub-2", "http://h:1/cb", t0 + 1);
        // 时间推进越过 TTL：新 issue 触发清扫，旧会话不复活
        let (fresh, _) = store.issue("sub-3", "http://h:1/cb", t0 + ttl_ms + 1);
        assert_eq!(
            store.consume(&_s1, t0 + ttl_ms + 1),
            None,
            "过期会话被 issue 清扫"
        );
        assert_eq!(store.consume(&_s2, t0 + ttl_ms + 1), None);
        assert!(
            store.consume(&fresh, t0 + ttl_ms + 1).is_some(),
            "新会话不受清扫影响"
        );
    }

    /// challenge 口径：BASE64URL(SHA256(verifier))，无 padding；RFC 4648
    /// 标准样本覆盖 1/2 尾字节分支。
    #[test]
    fn pkce_challenge_is_unpadded_base64url_of_sha256_verifier() {
        let pair = pkce_pair();
        let digest = Sha256::digest(pair.verifier.as_bytes());
        assert_eq!(pair.challenge, base64url_nopad(&digest));
        assert!(!pair.challenge.contains('='), "S256 challenge 无 padding");
        assert_eq!(pair.challenge.len(), 43, "32 字节摘要 → 43 字符");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
    }
}
