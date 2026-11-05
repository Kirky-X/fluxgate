// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Fluxweir i18n 基座：Fluent 双束（en/zh）+ 语言检测链。
//!
//! - **检测链**：`FLUXWEIR_LANG` → `LC_ALL` → `LC_MESSAGES` → `LANG`
//!   → sys-locale → `"en"`；`zh*` 归一 `zh`，`C`/`POSIX`/未知语言继续走链。
//! - **消息目录**：`locales/{en,zh}/*.ftl` 编译期内嵌，双束静态缓存。
//! - **结果域**：仅 `{en, zh}`，一切回退终结 en，默认英文。
//!
//! # 示例
//!
//! ```rust,ignore
//! use fluxweir::i18n;
//!
//! i18n::init();                          // 可选预热
//! i18n::t("vendor-upstream-stream-interrupted");
//! i18n::t_args("admin-vendor-not-found", &[("id", "openai".into())]);
//! i18n::set_locale("zh-CN").unwrap();    // 覆盖位优先于检测链
//! ```
//!
//! 缺键不 panic：回退链终结于 en 束，仍缺则原样返回键名——因此新增用户可见
//! 串必须先登记 en 键（zh 同步补齐），键位对齐与引用存在性由
//! `tests/i18n/i18n_parity_test.rs` 守卫（按调用点字面形态扫描，键必须是字面量；
//! `examples/` 不在扫描域，引入 i18n 时须同步扩域）。
//!
//! # 键化边界（调用点形态约定）
//!
//! `GateError` 载荷（`field`/`source`/`vendor`）承载两种形态，边界按内容定：
//! - **裸字段路径**（`"model"`、`"input[3].call_id"`）：机器定位符，经
//!   `error-validation` 模板的 `{ $field }` 槽位原样插值，不进 ftl；
//! - **自然语言整句**：必须以 t / t_args 的**字面键名**形态调用（parity
//!   守卫据此扫描键存在性与占位符完备性，示例中的占位键名会被守卫拦截——
//!   恰好证明扫描在生效）；含用户输入的占位符
//!   用 `format!("{value:?}")` 预转义后再传参（保留 Debug 引号语义）。
//!
//! # 日志面分层
//!
//! `warn`/`info`/`error` 级日志（运维可见）走 t()/t_args()（与 vendor
//! manager 先例一致）；`debug` 级协议调试锚**保持英文**——调试锚靠 grep
//! 固定短语定位，本地化会让诊断关键词随 locale 漂移，收益不抵成本。

pub mod catalog;
pub mod locale;

use thiserror::Error;

/// i18n 模块错误。
#[derive(Debug, Error)]
pub enum I18nError {
    /// 语言串非法或不受支持（结果域仅 `{en, zh}`）。
    #[error("invalid or unsupported locale '{input}' (supported: en, zh)")]
    InvalidLocale {
        /// 无法归一化的原始输入。
        input: String,
    },
}

pub use catalog::{init, t, t_args};
pub use locale::{clear_locale_override, current_locale, detected_locale, set_locale};
