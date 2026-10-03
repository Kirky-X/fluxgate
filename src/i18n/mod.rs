// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! NexusFlow i18n 基座：Fluent 双束（en/zh）+ 语言检测链。
//!
//! - **检测链**：`NEXUSFLOW_LANG` → `LC_ALL` → `LC_MESSAGES` → `LANG`
//!   → sys-locale → `"en"`；`zh*` 归一 `zh`，`C`/`POSIX`/未知语言继续走链。
//! - **消息目录**：`locales/{en,zh}/*.ftl` 编译期内嵌，双束静态缓存。
//! - **结果域**：仅 `{en, zh}`，一切回退终结 en，默认英文。
//!
//! # 示例
//!
//! ```rust,ignore
//! use nexusflow::i18n;
//!
//! i18n::init();                          // 可选预热
//! i18n::t("vendor-unavailable");
//! i18n::t_args("request-timeout", &[("vendor", "openai".into())]);
//! i18n::set_locale("zh-CN").unwrap();    // 覆盖位优先于检测链
//! ```
//!
//! 业务字符串迁移由下一阶段进行；本模块不含真实键。

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
