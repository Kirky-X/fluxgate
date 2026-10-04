// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! GateError ↔ LimiteronError 双向映射层。
//!
//! `limiteron::circuit::CircuitBreaker::execute` 的签名锁定
//! `Fut: Future<Output = Result<T, LimiteronError>>`，rc.6 起熔断打开拒绝以
//! `CircuitCallError::Open` 独立承载；`FailureClassifier::is_counted_as_failure`
//! 亦以 `&LimiteronError` 为参；而本 crate 执行路径的错误是 [`GateError`] ——
//! 在此以带 `llm-gate:` 前缀的 `LimiteronError::Other` 串为载体做双向转换。
//!
//! 变体集冻结（design.md D1.3）：映射层只覆盖 llm-gate 核心变体集；
//! `Database` 变体留 nexusflow、本映射层不引用——前缀串载荷
//! `llm-gate:database:*` 自拆分起不可还原，`from_limiteron` 走有损降级。
//!
//! 有损风险声明：`from_limiteron` 对「不带前缀的 Other」或「前缀后
//! variant 无法识别」（含 database）的载荷无法还原原 variant，降级为
//! `VendorUnavailable`（详情保留在 vendor 字符串中），该降级行为由
//! `tests/circuit_breaker_test.rs` 锁定。

use limiteron::circuit::{CircuitCallError, FailureClassifier};
use limiteron::error::LimiteronError;

use crate::error::GateError;

/// `LimiteronError::Other` 载荷的命名空间前缀
pub const GATE_ERROR_PREFIX: &str = "llm-gate:";

/// GateError → LimiteronError。
///
/// 编码为 `llm-gate:<variant>:<detail>` 前缀串；`detail` 中允许出现
/// `:`（解码按首个 `:` 切分 variant，其余全部归属 detail），往返无损。
pub fn to_limiteron(err: &GateError) -> LimiteronError {
    let (variant, detail) = match err {
        GateError::VendorUnavailable { vendor } => ("vendor_unavailable", vendor.clone()),
        GateError::Timeout { vendor } => ("timeout", vendor.clone()),
        GateError::RateLimited { vendor } => ("rate_limited", vendor.clone()),
        GateError::Validation { field } => ("validation", field.clone()),
        GateError::Protocol { source, .. } => ("protocol", source.clone()),
        GateError::Config { message } => ("config", message.clone()),
        GateError::Routing { message } => ("routing", message.clone()),
        // recId=19(c)：api 层收尾产物，实际不流经 CB execute 包裹（生成
        // 点在 fallback 循环顶部），补全映射仅为保持编码全性
        GateError::AllVendorsThrottled { retry_after_secs } => {
            ("all_vendors_throttled", retry_after_secs.to_string())
        }
    };
    LimiteronError::Other(format!("{}{}:{}", GATE_ERROR_PREFIX, variant, detail))
}

/// LimiteronError → GateError。
///
/// - `LimitError`（limiteron 限流 / 准入拒绝）→
///   `VendorUnavailable{vendor: "{id} (circuit open)"}`（与 CB Open 同形态，
///   CB `execute` 的 Open 拒绝现经 [`from_circuit_call`] 走同一形态）
/// - 带前缀的 `Other` → 解码还原原 variant
/// - 其余 → 有损降级为 `VendorUnavailable`（详情保留在 vendor 串中）
pub fn from_limiteron(err: LimiteronError, vendor_id: &str) -> GateError {
    match err {
        LimiteronError::LimitError(_) => GateError::VendorUnavailable {
            vendor: format!("{} (circuit open)", vendor_id),
        },
        LimiteronError::Other(payload) if payload.starts_with(GATE_ERROR_PREFIX) => {
            decode(&payload[GATE_ERROR_PREFIX.len()..])
                .unwrap_or_else(|| degraded(vendor_id, &payload))
        }
        other => degraded(vendor_id, &other.to_string()),
    }
}

/// 熔断 `execute` 结果 → GateError（limiteron rc.6 起 `execute`
/// 返回 `CircuitCallError<LimiteronError>`，区分「熔断拒绝未执行」与
/// 「调用透传失败」）。
///
/// - `Open` → `VendorUnavailable{vendor: "{id} (circuit open)"}`
///   （与 [`from_limiteron`] 的 LimitError 形态一致，`is_circuit_open_error`
///   据此同步 `VendorState`）
/// - `Inner(e)` → [`from_limiteron`] 透传
pub fn from_circuit_call(err: CircuitCallError<LimiteronError>, vendor_id: &str) -> GateError {
    match err {
        CircuitCallError::Open => GateError::VendorUnavailable {
            vendor: format!("{} (circuit open)", vendor_id),
        },
        CircuitCallError::Inner(e) => from_limiteron(e, vendor_id),
    }
}

/// 判断错误是否为熔断打开（`from_limiteron` 对 CB 拒绝的还原形态）。
///
/// 供 worker 完成分支把 CB Open 同步回写 `VendorState`（路由过滤
/// `== Available` 借此感知熔断，无需 async `get_state` 扩散到 router）。
pub fn is_circuit_open_error(err: &GateError) -> bool {
    matches!(err, GateError::VendorUnavailable { vendor }
        if vendor.contains("circuit open"))
}

/// 解析失败 / 非本 crate 错误的降级形态
fn degraded(vendor_id: &str, detail: &str) -> GateError {
    GateError::VendorUnavailable {
        vendor: crate::i18n::t_args(
            "vendor-circuit-error",
            &[
                ("vendor", vendor_id.to_string()),
                ("detail", detail.to_string()),
            ],
        ),
    }
}

/// 解码 `llm-gate:<variant>:<detail>` 载荷
fn decode(payload: &str) -> Option<GateError> {
    let (variant, detail) = payload.split_once(':')?;
    let detail = detail.to_string();
    match variant {
        "vendor_unavailable" => Some(GateError::VendorUnavailable { vendor: detail }),
        "timeout" => Some(GateError::Timeout { vendor: detail }),
        "rate_limited" => Some(GateError::RateLimited { vendor: detail }),
        "validation" => Some(GateError::Validation { field: detail }),
        "protocol" => Some(GateError::Protocol {
            source: detail,
            inner: None,
        }),
        "config" => Some(GateError::Config { message: detail }),
        "routing" => Some(GateError::Routing { message: detail }),
        _ => None,
    }
}

/// 按映射前缀分类的错误分类器。
///
/// 语义对齐 limiteron `DefaultFailureClassifier` 的「保护机制错误不计失败」：
/// - `VendorUnavailable` / `Timeout`（上游故障）→ 计失败
/// - `RateLimited` / `Validation` / `Protocol`（网关自身保护 / 客户端侧
///   问题，非 vendor 故障信号）→ 不计
/// - `Config` / `Routing` 及无法识别的载荷（含已冻结移出的 database）→ 保守计失败
#[derive(Debug)]
pub struct GateErrorClassifier;

impl FailureClassifier<LimiteronError> for GateErrorClassifier {
    fn is_counted_as_failure(&self, error: &LimiteronError) -> bool {
        match error {
            LimiteronError::Other(payload) if payload.starts_with(GATE_ERROR_PREFIX) => {
                match payload[GATE_ERROR_PREFIX.len()..].split_once(':') {
                    Some(("vendor_unavailable", _)) | Some(("timeout", _)) => true,
                    Some(("rate_limited", _)) | Some(("validation", _)) | Some(("protocol", _)) => {
                        false
                    }
                    _ => true,
                }
            }
            // 库自身的限流 / 熔断保护错误不算失败
            LimiteronError::LimitError(_) | LimiteronError::CircuitBreakerError(_) => false,
            _ => true,
        }
    }
}
