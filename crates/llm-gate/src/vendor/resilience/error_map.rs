// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! GateError 直驱熔断器的分类与还原层。
//!
//! 泛型收口：`limiteron::circuit::CircuitBreaker<E>` 的 `execute`
//! （limiteron src/circuit/types.rs:463）与 `FailureClassifier<E>`
//! （types.rs:44）均为泛型——本 crate 执行路径的错误 [`GateError`]
//! 直接作为 `E`，业务错误经 `CircuitCallError::Inner` 原样透传，
//! **无损、无编解码**（此前的 `llm-gate:` 前缀串双向编解码及其有损
//! 降级路径已随之删除；CB Open 拒绝由 `CircuitCallError::Open` 独立承载）。
//!
//! 变体集冻结（design.md D1.3）：分类器只覆盖 llm-gate 核心变体集；
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

/// 按 GateError variant 语义分类的失败分类器。
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
