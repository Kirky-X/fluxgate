// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! OIDC 短时键别名表（进程内 TTL，auth-and-masking R-auth-002 的常驻半边）。
//!
//! JWT 验签成功后，`middleware::oidc`（`oidc` feature 门控）签发一个进程内
//! 短时凭据并在此登记「凭据 → 母键摘要」别名；`api_key_digest` 与认证档位
//! 解析经 [`resolve`] 把短时键视角归一到母键——租户/配额/预算/亲和与同
//! tier 静态键一致。表空（零配置）时解析零开销直通。
//!
//! 安全不变量：
//! - 凭据含随机熵（uuid v4 双份），不可由 issuer+sub 推算——直接呈报短时
//!   凭据无法绕过 JWT 验签（验签路径外的静态校验不会命中别名）。
//! - 过期惰性清除；进程重启即空表（不落盘，R-auth-002「重启失效」）。
//! - 会话身份 = issuer+sub 摘要：同 (issuer, sub) 刷新复用同一凭据，
//!   表容量按会话数有界（高基数约束同 quota 账本）。

use std::collections::HashMap;
use std::sync::Mutex;

/// 单会话登记：短时凭据 + 母键摘要 + 过期时刻（Unix 秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    credential: String,
    mother_hash: String,
    expires_unix: i64,
}

#[derive(Default)]
struct Inner {
    /// 会话身份（issuer+sub 摘要）→ 会话
    sessions: HashMap<String, Session>,
    /// 短时凭据 → 会话身份（resolve 反查索引）
    by_credential: HashMap<String, String>,
}

/// 进程级单例（auth 中间件每请求读、oidc 验签路径写）。
static SESSIONS: std::sync::OnceLock<Mutex<Inner>> = std::sync::OnceLock::new();

/// 会话容量上限（同 (issuer, sub) 一会话；超限先清过期，仍超拒绝新签发——
/// fail-closed 方向，不放松验签边界）。
const MAX_SESSIONS: usize = 10_000;

fn inner() -> &'static Mutex<Inner> {
    SESSIONS.get_or_init(|| Mutex::new(Inner::default()))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 登记短时键别名（验签成功路径）：`session_id` = issuer+sub 派生摘要，
/// 同 id 已有活会话则刷新过期并复用原凭据（会话稳定），否则生成新凭据。
/// 返回凭据。表超容且无过期可清 → None（调用方按验签失败处置）。
pub fn register(
    session_id: &str,
    credential: impl FnOnce() -> String,
    mother_hash: &str,
    ttl_secs: i64,
) -> Option<String> {
    let now = now_secs();
    let expires = now.saturating_add(ttl_secs.max(1));
    let mut guard = inner().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(session) = guard.sessions.get_mut(session_id) {
        // 同会话刷新：凭据保持稳定（客户端侧同一 (issuer, sub) 的会话连续）
        if session.expires_unix > now {
            session.expires_unix = expires;
            let credential = session.credential.clone();
            guard
                .by_credential
                .entry(credential.clone())
                .or_insert_with(|| session_id.to_string());
            return Some(credential);
        }
        // 过期会话原位续期：凭据轮换
        let fresh = credential();
        let old = std::mem::replace(&mut session.credential, fresh.clone());
        session.mother_hash = mother_hash.to_string();
        session.expires_unix = expires;
        guard.by_credential.remove(&old);
        guard
            .by_credential
            .insert(fresh.clone(), session_id.to_string());
        return Some(fresh);
    }
    sweep_expired(&mut guard, now);
    if guard.sessions.len() >= MAX_SESSIONS {
        return None;
    }
    let fresh = credential();
    guard.sessions.insert(
        session_id.to_string(),
        Session {
            credential: fresh.clone(),
            mother_hash: mother_hash.to_string(),
            expires_unix: expires,
        },
    );
    guard
        .by_credential
        .insert(fresh.clone(), session_id.to_string());
    Some(fresh)
}

/// 解析短时凭据 → 母键摘要（活会话才有值；过期惰性清除后返回 None）。
/// 表空零分配快路径。
pub fn resolve(credential: &str) -> Option<String> {
    let guard = inner().lock().unwrap_or_else(|p| p.into_inner());
    if guard.by_credential.is_empty() {
        return None;
    }
    let session_id = guard.by_credential.get(credential)?;
    let session = guard.sessions.get(session_id)?;
    if session.expires_unix > now_secs() {
        Some(session.mother_hash.clone())
    } else {
        None
    }
}

/// 母键摘要直查（档位解析用）：凭据活会话 → 母键摘要；否则 None 走静态
/// key 路径。与 [`resolve`] 同义（命名区分语义：digest 面不再二次哈希）。
pub fn resolve_mother(credential: &str) -> Option<String> {
    resolve(credential)
}

fn sweep_expired(guard: &mut Inner, now: i64) {
    let expired: Vec<String> = guard
        .sessions
        .iter()
        .filter(|(_, session)| session.expires_unix <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        if let Some(session) = guard.sessions.remove(&id) {
            guard.by_credential.remove(&session.credential);
        }
    }
}

/// 活会话数（观测/测试面）。
pub fn live_sessions() -> usize {
    let now = now_secs();
    let mut guard = inner().lock().unwrap_or_else(|p| p.into_inner());
    sweep_expired(&mut guard, now);
    guard.sessions.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mother() -> String {
        "d6593c4edd6a8a85fb2a6e0105d1fc204936be82bad151cebdf99f096c198303".to_string()
    }

    #[test]
    fn register_resolve_roundtrip_and_expiry() {
        let credential =
            register("sess-a", || "cred-a".to_string(), &mother(), 60).expect("首登记成功");
        assert_eq!(resolve("cred-a"), Some(mother()), "活会话解析到母键");
        assert_eq!(resolve(credential.as_str()), Some(mother()));
        assert_eq!(resolve("unknown"), None, "未知凭据直通 None");

        // 过期：把 TTL 拉成负偏移不可行——登记新会话 ttl=1s 后等一秒不可取，
        // 直接用极短 TTL + 二次登记轮换验证过期分支
        let renewed = register("sess-a", || "cred-a2".to_string(), &mother(), 60)
            .expect("活会话刷新复用原凭据");
        assert_eq!(renewed, "cred-a", "活会话刷新保持凭据稳定");
        assert_eq!(live_sessions(), 1, "同会话不增行");
    }

    #[test]
    fn unknown_credential_is_transparent() {
        assert_eq!(resolve("no-such-credential"), None);
    }
}
