// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 语言检测与全局 locale 上下文。
//!
//! 检测链（强制顺序，全仓一致）：
//! 1. `NEXUSFLOW_LANG` 项目覆盖变量
//! 2. `LC_ALL` → `LC_MESSAGES` → `LANG`（显式读取，保证 Windows/边缘环境确定性）
//! 3. `sys-locale` 系统探测
//! 4. `"en"` 终极回退
//!
//! 归一化只产出 `en`/`zh` 两个值：`zh*` 全部归一 `zh`；`C`/`POSIX`/未知语言
//! 跳过当前环节继续走链；链尾必为 `en`。全局存储为 `OnceLock` 检测缓存 +
//! `RwLock` 覆盖位，[`set_locale`] 仅写覆盖位。

use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

use unic_langid::LanguageIdentifier;

use super::I18nError;

/// 项目级语言覆盖变量（检测链第一优先级，大写项目名约定）。
const LANG_ENV_VAR: &str = "NEXUSFLOW_LANG";

/// POSIX 显式环境链，按序回退。
const POSIX_ENV_KEYS: [&str; 3] = ["LC_ALL", "LC_MESSAGES", "LANG"];

/// 首次访问时经检测链初始化的全局 locale。
static GLOBAL_LOCALE: OnceLock<LanguageIdentifier> = OnceLock::new();

/// [`set_locale`] 写入的覆盖位，优先于检测缓存。
static OVERRIDE_LOCALE: RwLock<Option<LanguageIdentifier>> = RwLock::new(None);

/// 归一化语言串：去 `@modifier`、去 `.codeset`、`_` 转 `-`
/// （如 `zh_CN.UTF-8` → `zh`）。`C`/`POSIX`/不受支持的语言返回
/// `None`，由调用方继续检测链。
fn normalize(raw: &str) -> Option<LanguageIdentifier> {
    let s = raw.split('@').next()?.split('.').next()?.replace('_', "-");
    if matches!(s.as_str(), "C" | "POSIX") {
        return None;
    }
    let locale = LanguageIdentifier::from_str(&s).ok()?;
    match locale.language.as_str() {
        // zh-TW/HK/SG/Hans 全部归一 zh；结果域仅 {en, zh}
        "zh" => Some("zh".parse().expect("'zh' is a valid language identifier")),
        "en" => Some("en".parse().expect("'en' is a valid language identifier")),
        _ => None,
    }
}

/// 纯函数版检测链：环境读取与系统探测均由调用方注入，
/// 测试无需修改进程环境（并行安全、机器无关）。
pub(crate) fn detect_from(
    getenv: &dyn Fn(&str) -> Option<String>,
    sys_locale: Option<&str>,
) -> LanguageIdentifier {
    // 1. 项目覆盖变量（空值视为未设置）
    if let Some(lang) = getenv(LANG_ENV_VAR) {
        if !lang.trim().is_empty() {
            if let Some(l) = normalize(&lang) {
                return l;
            }
        }
    }
    // 2. POSIX 显式环境链
    for key in POSIX_ENV_KEYS {
        if let Some(lang) = getenv(key) {
            if !lang.trim().is_empty() {
                if let Some(l) = normalize(&lang) {
                    return l;
                }
            }
        }
    }
    // 3. sys-locale 系统探测
    if let Some(sys) = sys_locale {
        if let Some(l) = normalize(sys) {
            return l;
        }
    }
    // 4. 终极回退
    "en".parse().expect("'en' is a valid language identifier")
}

/// 生产检测入口：读真实进程环境 + sys-locale。
fn detect_locale() -> LanguageIdentifier {
    detect_from(
        &|key| std::env::var(key).ok(),
        sys_locale::get_locale().as_deref(),
    )
}

/// 当前 locale：覆盖位优先，否则取检测缓存（首访触发检测链）。
pub fn current_locale() -> LanguageIdentifier {
    if let Ok(guard) = OVERRIDE_LOCALE.read() {
        if let Some(locale) = guard.as_ref() {
            return locale.clone();
        }
    }
    GLOBAL_LOCALE.get_or_init(detect_locale).clone()
}

/// 设置全局 locale 覆盖位。
///
/// 入参经与检测链相同的归一化（`zh-CN` → `zh`），保证结果域仍为
/// `{en, zh}`：不受支持或非法的语言串返回 [`I18nError::InvalidLocale`]。
pub fn set_locale(locale: &str) -> Result<(), I18nError> {
    let normalized = normalize(locale).ok_or_else(|| I18nError::InvalidLocale {
        input: locale.to_string(),
    })?;
    let mut guard = OVERRIDE_LOCALE
        .write()
        .expect("locale override RwLock poisoned");
    *guard = Some(normalized);
    Ok(())
}

/// 清除覆盖位，回退到检测 locale。
pub fn clear_locale_override() {
    let mut guard = OVERRIDE_LOCALE
        .write()
        .expect("locale override RwLock poisoned");
    *guard = None;
}

/// 忽略覆盖位的检测结果（诊断用）。
pub fn detected_locale() -> LanguageIdentifier {
    GLOBAL_LOCALE.get_or_init(detect_locale).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 由键值对构造 getenv 注入闭包；未列出的键视为不存在。
    fn env_lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |key| map.get(key).map(|v| (*v).to_string())
    }

    #[test]
    fn test_detect_chain_zh_cn_utf8_maps_to_zh() {
        let getenv = env_lookup(&[("LC_ALL", "zh_CN.UTF-8")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");
    }

    #[test]
    fn test_detect_chain_zh_tw_maps_to_zh() {
        let getenv = env_lookup(&[("LC_ALL", "zh_TW")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");
    }

    #[test]
    fn test_detect_chain_fr_fr_falls_back_to_en() {
        let getenv = env_lookup(&[("LC_ALL", "fr_FR.UTF-8")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_detect_chain_c_maps_to_en() {
        let getenv = env_lookup(&[("LC_ALL", "C")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_detect_chain_posix_maps_to_en() {
        let getenv = env_lookup(&[("LC_ALL", "POSIX")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_detect_chain_all_empty_falls_to_en() {
        // 四个变量都存在但为空值：视为未设置，链尾回退 en
        let getenv = env_lookup(&[
            (LANG_ENV_VAR, ""),
            ("LC_ALL", ""),
            ("LC_MESSAGES", ""),
            ("LANG", ""),
        ]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_project_lang_overrides_lc_all() {
        let getenv = env_lookup(&[(LANG_ENV_VAR, "en_US"), ("LC_ALL", "zh_CN.UTF-8")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_chain_order_lc_all_beats_lc_messages_beats_lang() {
        let getenv = env_lookup(&[("LC_MESSAGES", "zh_CN"), ("LANG", "en_US.UTF-8")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");

        let getenv = env_lookup(&[("LC_ALL", ""), ("LC_MESSAGES", "zh_CN"), ("LANG", "en_US")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");

        let getenv = env_lookup(&[("LANG", "zh_TW.UTF-8")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");
    }

    #[test]
    fn test_unsupported_env_value_continues_chain() {
        // NEXUSFLOW_LANG 不支持 → 不中断，继续走 LC_ALL
        let getenv = env_lookup(&[(LANG_ENV_VAR, "de_DE"), ("LC_ALL", "zh_CN")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "zh");
    }

    #[test]
    fn test_sys_locale_fallback() {
        assert_eq!(
            detect_from(&env_lookup(&[]), Some("zh-CN"))
                .language
                .as_str(),
            "zh"
        );
        assert_eq!(
            detect_from(&env_lookup(&[]), Some("en_US.UTF-8"))
                .language
                .as_str(),
            "en"
        );
        // 系统探测结果为 C/不支持语言 → 终极回退 en
        assert_eq!(
            detect_from(&env_lookup(&[]), Some("C")).language.as_str(),
            "en"
        );
        assert_eq!(
            detect_from(&env_lookup(&[]), Some("ja_JP.UTF-8"))
                .language
                .as_str(),
            "en"
        );
    }

    #[test]
    fn test_detect_from_malformed_values_fall_to_en() {
        let getenv = env_lookup(&[("LC_ALL", "@@@ . !")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
        let getenv = env_lookup(&[(LANG_ENV_VAR, "   ")]);
        assert_eq!(detect_from(&getenv, None).language.as_str(), "en");
    }

    #[test]
    fn test_normalize_result_domain() {
        assert_eq!(normalize("zh_CN.UTF-8").unwrap().language.as_str(), "zh");
        assert_eq!(normalize("zh-Hans-SG").unwrap().language.as_str(), "zh");
        assert_eq!(normalize("en_US.UTF-8").unwrap().language.as_str(), "en");
        assert_eq!(normalize("en").unwrap().language.as_str(), "en");
        assert!(normalize("C").is_none());
        assert!(normalize("POSIX").is_none());
        assert!(normalize("fr_FR").is_none());
        assert!(normalize("").is_none());
        assert!(normalize("not a locale!!").is_none());
    }

    #[test]
    fn test_detect_locale_result_domain_with_real_env() {
        // 生产入口读真实进程环境，只断言结果域（不假设机器语言）
        let detected = detect_locale();
        let lang = detected.language.as_str();
        assert!(
            lang == "en" || lang == "zh",
            "result domain violated: {lang}"
        );
    }

    #[test]
    fn test_set_locale_override_and_clear() {
        clear_locale_override();
        set_locale("zh-CN").expect("zh-CN should normalize to zh");
        assert_eq!(current_locale().language.as_str(), "zh");
        set_locale("en_US.UTF-8").expect("en_US.UTF-8 should normalize to en");
        assert_eq!(current_locale().language.as_str(), "en");
        // 清理先于断言，避免失败时污染其他测试
        clear_locale_override();
        let fallback = current_locale();
        let lang = fallback.language.as_str();
        assert!(
            lang == "en" || lang == "zh",
            "result domain violated: {lang}"
        );
    }

    #[test]
    fn test_set_locale_rejects_unsupported_or_invalid() {
        assert!(
            set_locale("fr").is_err(),
            "unsupported language must be rejected"
        );
        assert!(set_locale("C").is_err());
        assert!(set_locale("POSIX").is_err());
        assert!(set_locale("not a locale!!").is_err());
        assert!(set_locale("").is_err());
        match set_locale("fr") {
            Err(I18nError::InvalidLocale { input }) => assert_eq!(input, "fr"),
            other => panic!("expected InvalidLocale, got {other:?}"),
        }
    }
}
