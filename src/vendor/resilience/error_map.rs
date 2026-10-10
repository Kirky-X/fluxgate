// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! GateError 直驱熔断器的分类与还原层。
//!
//! 泛型收口：`limiteron::circuit::CircuitBreaker<E>` 的 `execute`
//! （limiteron src/circuit/types.rs）与 `FailureClassifier<E>`
//! （types.rs）均为泛型——本 crate 执行路径的错误 [`GateError`]
//! 直接作为 `E`，业务错误经 `CircuitCallError::Inner` 原样透传，
//! **无损、无编解码**（此前的 `llm-gate:` 前缀串双向编解码及其有损
//! 降级路径已随之删除；CB Open 拒绝由 `CircuitCallError::Open` 独立承载）。
//!
//! 变体集冻结（design.md D1.3）：分类器只覆盖 GateError 核心变体集；
//! `Database` 变体留 nexusflow、本层不引用。

use limiteron::circuit::{CircuitCallError, FailureClassifier};

use crate::error::GateError;

/// 熔断 `execute` 结果 → GateError（`execute` 返回
/// `CircuitCallError<GateError>`，区分「熔断拒绝未执行」与「调用透传失败」）。
///
/// - `Open` → `VendorUnavailable{vendor: "{id} (circuit open)"}`
///   （`is_circuit_open_error` 据此同步 `VendorState`）
/// - `Inner(e)` → 原样透传（无损）
pub fn from_circuit_call(err: CircuitCallError<GateError>, vendor_id: &str) -> GateError {
    match err {
        CircuitCallError::Open => GateError::VendorUnavailable {
            vendor: format!("{} (circuit open)", vendor_id),
        },
        CircuitCallError::Inner(e) => e,
    }
}

/// 判断错误是否为熔断打开（`from_circuit_call` 对 CB 拒绝的还原形态）。
///
/// 供 worker 完成分支把 CB Open 同步回写 `VendorState`（路由过滤
/// `== Available` 借此感知熔断，无需 async `get_state` 扩散到 router）。
pub fn is_circuit_open_error(err: &GateError) -> bool {
    matches!(err, GateError::VendorUnavailable { vendor }
        if vendor.contains("circuit open"))
}

/// key 错误分类（WP6 密钥池三态）：永久错误 → 黑名单（不参与轮换、
/// 不随时间自动恢复）；瞬时错误 → 维持现状（冷却 TTL 自动恢复、429 退避）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyErrorClass {
    /// 永久错误：401/402/403，或错误消息明确命中账务类形态
    Permanent,
    /// 瞬时错误：429/超时/5xx/网络及其他
    Transient,
}

/// 账务类错误消息形态（小写子串匹配）：命中即永久——配额/订阅/过期是
/// 账户级失败，同账户换 key 亦无用。刻意不含裸 `quota`（429 限流文案常
/// 带 quota，须保持瞬时；匹配见单测 `rate_limit_quota_text_stays_transient`）。
const BILLING_MESSAGE_MARKERS: &[&str] = &[
    "insufficient_quota",
    "insufficient quota",
    "expired",
    "billing",
    "delinquent",
];

/// 按 HTTP 状态码 + 上游错误文本判定 key 错误分类（WP6）。
/// 账务形态优先（显式形状胜出）：5xx 正文明确命中账务形态同样永久；
/// 普通 5xx 文本不误伤（保持瞬时）。
pub fn classify_key_error(status: u16, message: &str) -> KeyErrorClass {
    let lower = message.to_lowercase();
    if BILLING_MESSAGE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return KeyErrorClass::Permanent;
    }
    match status {
        401..=403 => KeyErrorClass::Permanent,
        _ => KeyErrorClass::Transient,
    }
}

/// 按 GateError variant 语义分类的失败分类器.
///
/// 语义对齐 limiteron `DefaultFailureClassifier` 的「保护机制错误不计失败」
/// （旧实现的库内 LimitError/CircuitBreakerError 分支随泛型收口失效——
/// CB Open 拒绝经 `CircuitCallError::Open` 承载，不再进入分类器）：
/// - `VendorUnavailable` / `Timeout`（上游故障）→ 计失败
/// - `RateLimited` / `Validation` / `Protocol`（网关自身保护 / 客户端侧
///   问题，非 vendor 故障信号）→ 不计
/// - `Config` / `Routing` / `AllVendorsThrottled` → 保守计失败
#[derive(Debug)]
pub struct GateErrorClassifier;

impl FailureClassifier<GateError> for GateErrorClassifier {
    fn is_counted_as_failure(&self, error: &GateError) -> bool {
        match error {
            GateError::VendorUnavailable { .. } | GateError::Timeout { .. } => true,
            GateError::RateLimited { .. }
            | GateError::Validation { .. }
            | GateError::Protocol { .. } => false,
            GateError::Config { .. }
            | GateError::Routing { .. }
            | GateError::AllVendorsThrottled { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_statuses_are_permanent() {
        for status in [401u16, 402, 403] {
            assert_eq!(
                classify_key_error(status, "Unauthorized"),
                KeyErrorClass::Permanent,
                "status {status} 必须永久"
            );
        }
    }

    #[test]
    fn transient_statuses_stay_transient() {
        for (status, message) in [
            (429u16, "rate limited, retry later"),
            (500u16, "internal server error"),
            (502u16, "bad gateway"),
            (503u16, "upstream overloaded"),
            (400u16, "bad request"),
        ] {
            assert_eq!(
                classify_key_error(status, message),
                KeyErrorClass::Transient,
                "status {status} 无账务形态必须瞬时"
            );
        }
    }

    #[test]
    fn billing_message_markers_are_permanent_on_any_status() {
        for (status, message) in [
            (
                500u16,
                r#"{"error":{"code":"insufficient_quota","message":"quota hit"}}"#,
            ),
            (400u16, "Insufficient quota for this account"),
            (403u16, "api key expired, rotate it"),
            (500u16, "billing account delinquent, service suspended"),
        ] {
            assert_eq!(
                classify_key_error(status, message),
                KeyErrorClass::Permanent,
                "账务形态必须永久（含 5xx 正文）: {message}"
            );
        }
    }

    #[test]
    fn rate_limit_quota_text_stays_transient() {
        // 429 文案常带 quota——裸 quota 不得触发永久（不误伤限流）。
        assert_eq!(
            classify_key_error(429, "quota exceeded for key, retry after 60s"),
            KeyErrorClass::Transient,
        );
        assert_eq!(
            classify_key_error(500, "upstream blew up: connection reset by peer"),
            KeyErrorClass::Transient,
            "普通 5xx 文本不误伤",
        );
    }
}
