// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 消息目录：`locales/{en,zh}/*.ftl` 经 `include_str!` 编译期内嵌，
//! 构建为 `fluent_bundle::concurrent::FluentBundle` 双束（EN/ZH，`OnceLock`
//! 静态缓存，Send+Sync）。
//!
//! 内嵌保证部署产物无需携带磁盘 `locales/` 目录；新增 `.ftl` 文件必须
//! 登记进 `EMBEDDED_LOCALES`，否则守卫测试
//! `test_embedded_locales_cover_locales_dir` 失败。
//!
//! 查找顺序：当前语言束 → en 束 → 返回键名本身（绝不 panic）。

use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

use super::locale::current_locale;

/// 编译期内嵌的 locales 目录镜像：语言目录名 → [(文件名, 内容)]。
/// 文件按迁移桶组织：main → main.rs；api → src/api；vendor → src/vendor；
/// config-protocol-router-middleware → src/{config,protocol,router,middleware}；
/// error-database-utils → src/error.rs + src/{database,utils}。
const EMBEDDED_LOCALES: &[(&str, &[(&str, &str)])] = &[
    (
        "en",
        &[
            ("main.ftl", include_str!("../../locales/en/main.ftl")),
            ("api.ftl", include_str!("../../locales/en/api.ftl")),
            ("vendor.ftl", include_str!("../../locales/en/vendor.ftl")),
            (
                "config-protocol-router-middleware.ftl",
                include_str!("../../locales/en/config-protocol-router-middleware.ftl"),
            ),
            (
                "error-database-utils.ftl",
                include_str!("../../locales/en/error-database-utils.ftl"),
            ),
        ],
    ),
    (
        "zh",
        &[
            ("main.ftl", include_str!("../../locales/zh/main.ftl")),
            ("api.ftl", include_str!("../../locales/zh/api.ftl")),
            ("vendor.ftl", include_str!("../../locales/zh/vendor.ftl")),
            (
                "config-protocol-router-middleware.ftl",
                include_str!("../../locales/zh/config-protocol-router-middleware.ftl"),
            ),
            (
                "error-database-utils.ftl",
                include_str!("../../locales/zh/error-database-utils.ftl"),
            ),
        ],
    ),
];

/// EN 束（未知语言一律回退到此束）。
static EN_BUNDLE: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
/// ZH 束。
static ZH_BUNDLE: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();

/// 按语言选束：`zh` → ZH 束，其余（含未知语言）一律 EN 束。
fn bundle_for(lang: &str) -> &FluentBundle<FluentResource> {
    match lang {
        "zh" => ZH_BUNDLE.get_or_init(build_zh_bundle),
        _ => EN_BUNDLE.get_or_init(build_en_bundle),
    }
}

/// 从指定语言的束格式化消息；缺键/缺值返回 `None`（调用方回退）。
fn format_from_bundle(lang: &str, key: &str, args: &[(&str, String)]) -> Option<String> {
    let bundle = bundle_for(lang);
    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, FluentValue::from(value.clone()));
    }
    let mut errors = vec![];
    Some(
        bundle
            .format_pattern(pattern, Some(&fluent_args), &mut errors)
            .to_string(),
    )
}

/// 翻译：当前语言束 → en 束回退 → 键名本身。
fn translate(key: &str, args: &[(&str, String)]) -> String {
    let locale = current_locale();
    let lang = locale.language.as_str();
    format_from_bundle(lang, key, args)
        .or_else(|| format_from_bundle("en", key, args))
        .unwrap_or_else(|| key.to_string())
}

/// 翻译无参消息。缺键返回键名本身，不 panic。
///
/// 键必须传字面量：键位守卫按调用点字面形态扫描源码，动态键会静默漏检。
pub fn t(key: &str) -> String {
    translate(key, &[])
}

/// 翻译带 `{ $name }` 占位符的消息。缺键返回键名本身，不 panic。
///
/// 实参名必须与 FTL 占位符逐字一致（en/zh 占位符对齐由守卫测试断言）；
/// 键必须传字面量，理由同 [`t`]。
pub fn t_args(key: &str, args: &[(&str, String)]) -> String {
    translate(key, args)
}

/// 预热：触发检测链并构建 EN/ZH 双束。可选调用——[`t`]/[`t_args`]
/// 首次调用本就会惰性初始化。
pub fn init() {
    let _ = super::locale::detected_locale();
    let _ = bundle_for("en");
    let _ = bundle_for("zh");
}

fn build_bundle(lang: &'static str, files: &[(&str, &str)]) -> FluentBundle<FluentResource> {
    let langid: LanguageIdentifier = lang
        .parse()
        .unwrap_or_else(|_| panic!("'{lang}' is a valid language identifier"));
    let mut bundle = FluentBundle::new_concurrent(vec![langid]);
    // 关闭 Unicode 隔离符，避免输出夹带 U+2068/U+2069
    bundle.set_use_isolating(false);
    for (file_name, source) in files {
        // 解析失败保底使用部分资源（参照基线 §7 已知陷阱）
        let resource = FluentResource::try_new(source.to_string()).unwrap_or_else(|e| e.0);
        bundle
            .add_resource(resource)
            .unwrap_or_else(|errs| panic!("FTL resource conflict in {lang}/{file_name}: {errs:?}"));
    }
    bundle
}

fn build_en_bundle() -> FluentBundle<FluentResource> {
    let files = EMBEDDED_LOCALES
        .iter()
        .find(|(locale, _)| *locale == "en")
        .map(|(_, files)| *files)
        .expect("'en' must be registered in EMBEDDED_LOCALES");
    build_bundle("en", files)
}

fn build_zh_bundle() -> FluentBundle<FluentResource> {
    let files = EMBEDDED_LOCALES
        .iter()
        .find(|(locale, _)| *locale == "zh")
        .map(|(_, files)| *files)
        .expect("'zh' must be registered in EMBEDDED_LOCALES");
    build_bundle("zh", files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 提取 FTL 顶层消息键：顶层行（非缩进）取首个 `=` 前的标识符，
    /// 跳过注释与空行。迁移阶段键均为简单 `key = value` 形式。
    fn extract_keys(ftl: &str) -> Vec<String> {
        ftl.lines()
            .filter(|line| !line.trim().is_empty())
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter(|line| !line.starts_with(char::is_whitespace))
            .filter_map(|line| line.split('=').next())
            .map(|key| key.trim().trim_start_matches('-').to_string())
            .filter(|key| !key.is_empty())
            .collect()
    }

    #[test]
    fn test_en_zh_key_sets_identical() {
        assert_eq!(EMBEDDED_LOCALES.len(), 2, "仅支持 en/zh 双语言");
        let (en, zh) = (&EMBEDDED_LOCALES[0], &EMBEDDED_LOCALES[1]);
        assert_eq!(en.0, "en");
        assert_eq!(zh.0, "zh");
        assert_eq!(en.1.len(), zh.1.len(), "en/zh 文件对数量必须一致");
        for ((en_name, en_src), (zh_name, zh_src)) in en.1.iter().zip(zh.1.iter()) {
            assert_eq!(en_name, zh_name, "en/zh 文件必须同名成对");
            let mut en_keys = extract_keys(en_src);
            let mut zh_keys = extract_keys(zh_src);
            en_keys.sort();
            zh_keys.sort();
            assert_eq!(en_keys, zh_keys, "'{en_name}' 的 en/zh 键集合不一致");
        }
    }

    /// 内嵌清单与磁盘目录双向一致：新增 .ftl 漏登记 EMBEDDED_LOCALES 时
    /// 文件不会被打进二进制，运行时对该文件整族键缺键——在此钉死。
    #[test]
    fn embedded_manifest_matches_on_disk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales/en");
        let on_disk: std::collections::BTreeSet<String> = std::fs::read_dir(&dir)
            .expect("locales/en 目录必须存在")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "ftl"))
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        let embedded: std::collections::BTreeSet<String> = EMBEDDED_LOCALES
            .iter()
            .flat_map(|(_, files)| {
                files
                    .iter()
                    .map(|(name, _)| name.strip_suffix(".ftl").unwrap_or(name).to_string())
            })
            .collect();
        assert_eq!(
            embedded, on_disk,
            "EMBEDDED_LOCALES 与 locales/en/*.ftl 失配：新增目录文件必须同步嵌入"
        );
    }

    #[test]
    fn test_no_duplicate_keys_within_locale() {
        for (locale, files) in EMBEDDED_LOCALES {
            let mut seen = std::collections::HashSet::new();
            for (file_name, source) in *files {
                for key in extract_keys(source) {
                    assert!(
                        seen.insert(key.clone()),
                        "{locale}/{file_name}: 键 '{key}' 重复定义"
                    );
                }
            }
        }
    }

    #[test]
    fn test_embedded_locales_cover_locales_dir() {
        // 编译期登记守卫：locales/ 下每个语言目录的每个 .ftl 都必须
        // 出现在 EMBEDDED_LOCALES，否则新文件会在产物中静默缺译
        let locales_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut disk_dirs: Vec<String> = std::fs::read_dir(&locales_dir)
            .expect("locales dir exists")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        disk_dirs.sort();
        let mut registered_dirs: Vec<&str> =
            EMBEDDED_LOCALES.iter().map(|(locale, _)| *locale).collect();
        registered_dirs.sort();
        assert_eq!(
            disk_dirs, registered_dirs,
            "EMBEDDED_LOCALES 必须覆盖 locales/ 全部语言目录"
        );

        for (locale, files) in EMBEDDED_LOCALES {
            let dir = locales_dir.join(locale);
            let mut disk_stems: Vec<String> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("locale dir '{locale}' readable: {e}"))
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("ftl"))
                .filter_map(|entry| {
                    entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str().map(str::to_string))
                })
                .collect();
            disk_stems.sort();
            let mut registered_stems: Vec<&str> = files
                .iter()
                .map(|(name, _)| name.strip_suffix(".ftl").unwrap())
                .collect();
            registered_stems.sort();
            assert_eq!(
                disk_stems, registered_stems,
                "locale '{locale}': 磁盘 .ftl 与 EMBEDDED_LOCALES 登记不一致"
            );
        }
    }

    #[test]
    fn test_unknown_language_falls_back_to_en_bundle() {
        // 未知语言不建第三束，直接落 EN 束
        assert!(
            std::ptr::eq(bundle_for("ar"), bundle_for("en")),
            "未知语言必须回退 EN 束"
        );
        assert!(
            !std::ptr::eq(bundle_for("ar"), bundle_for("zh")),
            "未知语言不得误落 ZH 束"
        );
    }

    #[test]
    fn test_missing_key_returns_key_name_without_panic() {
        // 当前目录尚无真实键（迁移阶段填充）：任何查找都走缺键路径
        assert_eq!(t("no-such-key"), "no-such-key");
        assert_eq!(
            t_args("no-such-key", &[("vendor", String::from("openai"))]),
            "no-such-key"
        );
    }

    #[test]
    fn test_bundles_ready_after_init() {
        init();
        assert!(EN_BUNDLE.get().is_some(), "init 后 EN 束必须就绪");
        assert!(ZH_BUNDLE.get().is_some(), "init 后 ZH 束必须就绪");
        // 双束语言标识必须正确
        let en_id: LanguageIdentifier = "en".parse().unwrap();
        let zh_id: LanguageIdentifier = "zh".parse().unwrap();
        assert_eq!(EN_BUNDLE.get().unwrap().locales, [en_id]);
        assert_eq!(ZH_BUNDLE.get().unwrap().locales, [zh_id]);
    }
}
