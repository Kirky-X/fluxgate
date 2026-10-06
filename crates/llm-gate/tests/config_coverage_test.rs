// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! config 模块覆盖率补充测试。
//!
//! 新建目的：src/config/loader.rs 的文件加载链路（路径不存在 / TOML 语法
//! 错 / 类型不匹配 / vendor·model 重复 id / api_key_env 缺失只告警）与
//! src/config/mod.rs 的 serde default 函数（port/db_path/weight/model
//! enabled/rps/global_model/auth·cors enabled）、RoutingSection::preset
//! 此前 0 覆盖；ConfigManager 的 RMW / 无路径拒绝 / 快照目录 "." 回退
//! 分支也一并锁定。
//!
//! 全部走公开 API：ConfigLoader::load 读临时目录中的 TOML，ConfigManager
//! 做 RMW 与模型解析断言；错误分支断言 GateError 变体与插值参数，
//! 不做"调用不报错"式断言。

use llm_gate::config::loader::ConfigLoader;
use llm_gate::config::{Config, ConfigManager, RateLimitConfig, RoutingSection, ServerConfig};
use llm_gate::error::GateError;
use std::path::PathBuf;

mod common;

/// 写临时 TOML 并返回路径（临时目录 + 进程号隔离，避免并行任务互踩）
fn temp_toml(name: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nx-config-cov-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write toml");
    path
}

fn load_from(path: &std::path::Path) -> Result<ConfigManager, GateError> {
    ConfigLoader::new(Some(path.to_string_lossy().to_string())).load()
}

/// 缺省键全覆盖的 TOML：port / database 两键 / model weight·enabled /
/// rps / global_model 两键 / auth·cors enabled 均缺席，由 serde default
/// 函数回退；vendor_b 与 vendor_a 共享 abstract-a（映射聚合）、model-e
/// 缺 abstract_id（model_id 回退）、vendor_c 整体禁用（解析剔除）。
const DEFAULTS_TOML: &str = r#"
[server]
host = "127.0.0.1"

[database]

[[vendors]]
id = "vendor_a"
name = "Vendor A"
type_ = "openai"
api_key_env = "NX_COV_KEY_A"
rate_limit = { type = "token", max_tokens = 1000 }
concurrency = {}

[[vendors.models]]
model_id = "model-a"
abstract_id = "abstract-a"

[[vendors.models]]
model_id = "model-b"
abstract_id = "abstract-b"
enabled = false

[[vendors]]
id = "vendor_b"
name = "Vendor B"
type_ = "anthropic"
api_key_env = "NX_COV_KEY_B"
rate_limit = { type = "interval", hours = 2 }
concurrency = { requests_per_second = 5 }

[[vendors.models]]
model_id = "model-c"
abstract_id = "abstract-a"
weight = 0.5

[[vendors.models]]
model_id = "model-e"

[[vendors]]
id = "vendor_c"
name = "Vendor C"
type_ = "openai"
api_key_env = "NX_COV_KEY_C"
enabled = false
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }

[[vendors.models]]
model_id = "model-d"
abstract_id = "abstract-d"

[global_model]

[auth]
api_keys = ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]

[cors]
allowed_origins = ["https://a.example.com"]
"#;

#[test]
fn loader_applies_serde_defaults_and_records_path() {
    let path = temp_toml("defaults.toml", DEFAULTS_TOML);
    let manager = load_from(&path).expect("合法 TOML 必须加载成功");
    let config = manager.read();

    // [server] 缺 port → default_port
    assert_eq!(config.server.host, "127.0.0.1");
    assert_eq!(config.server.port, 8080, "缺省 port 回退 default_port");
    // [database] 全缺 → default_db_path / default_pool_size
    assert_eq!(
        config.database.path, "./data/llm_proxy.db",
        "缺省 db path 回退 default_db_path"
    );
    assert_eq!(config.database.pool_size, 10);

    // 空 concurrency 表 → 全默认（default_rps 等）
    let vendor_a = &config.vendors[0];
    assert_eq!(
        vendor_a.concurrency.requests_per_second, 1,
        "缺省 rps 回退 default_rps"
    );
    assert_eq!(vendor_a.concurrency.max_concurrent_requests, None);
    // Token 限流缺 window_hours/sliding → 默认
    match &vendor_a.rate_limit {
        RateLimitConfig::Token {
            max_tokens,
            window_hours,
            sliding,
        } => {
            assert_eq!(*max_tokens, 1000);
            assert_eq!(
                *window_hours,
                Some(24),
                "缺省窗口回退 default_token_window_hours"
            );
            assert!(!sliding);
        }
        other => panic!("expected Token rate limit, got {other:?}"),
    }
    // 模型缺 weight/enabled → default_weight / default_model_enabled
    let model_a = &vendor_a.models[0];
    assert!(
        (model_a.weight - 1.0).abs() < f64::EPSILON,
        "缺省 weight 回退 default_weight"
    );
    assert!(model_a.enabled, "缺省 enabled 回退 default_model_enabled");

    // [global_model] 空表 → default_global_enabled / default_strategy
    let global = config.global_model.as_ref().expect("global_model 存在");
    assert!(global.enabled);
    assert_eq!(global.strategy, "weighted");
    assert!(manager.is_global_model_enabled());

    // [auth] 缺 enabled → default_auth_enabled；缺 header → 结构体默认
    let auth = config.auth.as_ref().expect("auth 存在");
    assert!(auth.enabled, "缺省 enabled 回退 default_auth_enabled");
    assert_eq!(auth.header.name, "Authorization");
    assert_eq!(auth.header.prefix, "Bearer");

    // [cors] 缺 enabled → default_cors_enabled
    assert!(
        config.cors.as_ref().expect("cors 存在").enabled,
        "缺省 enabled 回退 default_cors_enabled"
    );

    // loader 记忆加载路径（update_config 写回同一路径的依据）
    assert_eq!(manager.path(), Some(path.clone()));
}

#[test]
fn resolve_and_mappings_respect_enabled_case_and_fallback() {
    let path = temp_toml("resolve.toml", DEFAULTS_TOML);
    let manager = load_from(&path).expect("load ok");

    // 大小写不敏感 + 多 vendor 同 abstract 聚合（vendor_a/b 各一）
    let hits = manager.resolve_model("ABSTRACT-A");
    assert_eq!(hits.len(), 2, "abstract 匹配不区分大小写且跨 vendor 聚合");
    // 禁用的 model 被剔除
    assert!(manager.resolve_model("abstract-b").is_empty());
    // 禁用的 vendor 被剔除
    assert!(manager.resolve_model("abstract-d").is_empty());
    // abstract_id 缺失回退 model_id（model-e）
    let fallback = manager.resolve_model("Model-E");
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].1.model_id, "model-e");

    // 只有启用的 vendor 进入列表
    let enabled = manager.get_enabled_vendors();
    assert_eq!(enabled.len(), 2, "vendor_c 已禁用");

    // 映射表：聚合启用项、剔除禁用项、缺 abstract 用 model_id 归一
    let mappings = manager.get_all_model_mappings();
    assert_eq!(mappings.get("abstract-a").map(Vec::len), Some(2));
    assert!(!mappings.contains_key("abstract-b"), "禁用 model 不进映射");
    assert!(!mappings.contains_key("abstract-d"), "禁用 vendor 不进映射");
    assert_eq!(
        mappings.get("model-e").map(Vec::len),
        Some(1),
        "缺 abstract_id 时以 model_id 为映射键"
    );
}

#[test]
fn loader_rejects_duplicate_vendor_ids() {
    let path = temp_toml(
        "dup-vendor.toml",
        r#"
[server]
host = "127.0.0.1"
[database]
[[vendors]]
id = "dup_vendor"
name = "A"
type_ = "openai"
api_key_env = "K1"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
[[vendors]]
id = "dup_vendor"
name = "B"
type_ = "openai"
api_key_env = "K2"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
"#,
    );
    // ConfigManager 无 Debug，map 抹掉 Ok 载荷后仅对错误分支做形态断言
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("dup_vendor"),
                "重复 vendor 错误必须插值携带 id: {message}"
            );
        }
        other => panic!("重复 vendor id 必须报 Config 错误，实际 {other:?}"),
    }
}

#[test]
fn loader_rejects_duplicate_model_ids_within_vendor() {
    let path = temp_toml(
        "dup-model.toml",
        r#"
[server]
host = "127.0.0.1"
[database]
[[vendors]]
id = "dup_models_vendor"
name = "A"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
[[vendors.models]]
model_id = "dup-model"
[[vendors.models]]
model_id = "dup-model"
"#,
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("dup-model") && message.contains("dup_models_vendor"),
                "重复 model 错误必须插值携带 vendor 与 model id: {message}"
            );
        }
        other => panic!("重复 model id 必须报 Config 错误，实际 {other:?}"),
    }
}

/// [resilience].circuit_open_timeout_ms 下限校验：窗口 <100ms 使 Open→HalfOpen
/// 即刻转换，故障期退化为探针风暴（等于关闭熔断冷却）。加载与热更新共用
/// validate_config，两路同拒；下限值本身必须放行。
#[test]
fn loader_rejects_circuit_open_timeout_below_floor() {
    const VENDOR_TOML: &str = r#"
[server]
host = "127.0.0.1"
[database]
[[vendors]]
id = "cb_window_vendor"
name = "A"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
"#;

    for value in [0u64, 50, 99] {
        let path = temp_toml(
            "cb-window-small.toml",
            &format!("{VENDOR_TOML}\n[resilience]\ncircuit_open_timeout_ms = {value}\n"),
        );
        match load_from(&path).map(|_| ()) {
            Err(GateError::Config { message }) => {
                assert!(
                    message.contains(&value.to_string()),
                    "窗口下限错误必须插值携带配置值: {message}"
                );
            }
            other => panic!("circuit_open_timeout_ms={value} 必须被拒绝，实际 {other:?}"),
        }
    }

    let path = temp_toml(
        "cb-window-floor.toml",
        &format!("{VENDOR_TOML}\n[resilience]\ncircuit_open_timeout_ms = 100\n"),
    );
    load_from(&path).expect("100ms 下限值必须放行");
}

#[test]
fn loader_reports_toml_syntax_and_type_errors_distinctly() {
    // TOML 语法错 → confers 加载层错误（config-load-failed）
    let syntax = temp_toml("bad-syntax.toml", "this is not [ valid toml");
    match load_from(&syntax).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert_ne!(
                message, "config-load-failed",
                "加载错误键必须注册进 FTL 目录"
            );
        }
        other => panic!("非法 TOML 必须报 Config 错误，实际 {other:?}"),
    }

    // 类型不匹配（port 为字符串）→ serde 解析层错误（config-parse-failed）。
    // 经 confers AnnotatedValue::to_typed 投影后错误有意脱敏：只携带字段
    // 路径（server.port），绝不回显出错值——配置值可能是密钥原文，回显会
    // 经错误通道泄进日志/HTTP 响应（confers/src/types.rs to_typed 错误构造）。
    let mismatch = temp_toml(
        "type-mismatch.toml",
        r#"
[server]
host = "127.0.0.1"
port = "not-a-number"
[database]
[[vendors]]
id = "v"
name = "V"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
"#,
    );
    match load_from(&mismatch).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert_ne!(
                message, "config-parse-failed",
                "解析错误键必须注册进 FTL 目录"
            );
            assert!(
                message.contains("server.port"),
                "解析错误必须携带字段路径: {message}"
            );
            assert!(
                !message.contains("not-a-number"),
                "解析错误不得回显出错值（防密钥误填泄入错误通道）: {message}"
            );
        }
        other => panic!("类型不匹配必须报 Config 错误，实际 {other:?}"),
    }
}

/// [auth] 管理面密钥启动拒绝：`api_keys` 与 `[[auth.keys]].hash` 的配置
/// 契约是 SHA256 hex 摘要（64 个小写 hex 字符），违约条目必须在加载路径
/// fail-fast 拒绝，防止无效/弱凭证在重启用时静默生效。校验分两层（与
/// loader.rs validate_admin_secrets 的双层描述一致）：
/// ① 形态契约（本仓库强制）：64 位小写 hex——用例 ①②④⑤ 均在本层被拒
///    （"changeme"/"short"/长口令不是小写 hex-64，大写摘要被小写关拦下）；
/// ② confers JwtSecretValidator 规则内核（长度 + 弱词表，词表不复制进
///    本仓库）：过形态关的条目必然 ≥32 字节且不可能是弱词表任何词，
///    内核对真实输入恒 Ok、路径当前不可达，为规则内核后续增强保留
///    前向继承钩子（故无内核分支的独立测试用例）。
/// 错误信息只报条目位置与规则结论，不回显条目原文（脱敏先例同
/// validate_config 的 env 告警）。
#[test]
fn loader_rejects_weak_admin_secrets_at_startup() {
    const VENDOR_TOML: &str = r#"
[server]
host = "127.0.0.1"
[database]
[[vendors]]
id = "weak_secret_vendor"
name = "A"
type_ = "openai"
api_key_env = "K"
rate_limit = { type = "interval", hours = 1 }
concurrency = { requests_per_second = 1 }
"#;

    // ① api_keys 明文误填 → 形态契约层拒绝（非小写 hex-64；confers 内核
    //    长度/弱词表路径对真实输入不可达，见函数头注释），报条目位置
    let path = temp_toml(
        "weak-api-key.toml",
        &format!("{VENDOR_TOML}\n[auth]\napi_keys = [\"changeme\"]\n"),
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("auth.api_keys[0]"),
                "弱密钥错误必须报条目位置: {message}"
            );
            assert!(
                !message.contains("changeme"),
                "弱密钥错误不得回显条目原文: {message}"
            );
        }
        other => panic!("明文误填 api_keys 必须拒绝启动，实际 {other:?}"),
    }

    // ② [[auth.keys]].hash 同形态同拒（档位表条目也是管理面凭证）
    let path = temp_toml(
        "weak-tier-hash.toml",
        &format!("{VENDOR_TOML}\n[auth]\n[[auth.keys]]\nhash = \"short\"\ntier = \"dedicated\"\n"),
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("auth.keys[0].hash"),
                "档位表弱摘要错误必须报条目位置: {message}"
            );
        }
        other => panic!("弱 hash 条目必须拒绝启动，实际 {other:?}"),
    }

    // ③ 合法摘要形态放行：64 位小写十六进制（认证侧 sha256_hex 的输出
    //    形态）过形态关逐位可用
    let digest = "a".repeat(64);
    let path = temp_toml(
        "strong-api-key.toml",
        &format!("{VENDOR_TOML}\n[auth]\napi_keys = [\"{digest}\"]\n"),
    );
    load_from(&path).expect("SHA256 hex 摘要形态的密钥必须放行");

    // ④ 64 字节非 hex 明文（长口令误填当摘要）→ 形态契约拒绝。这是
    //    confers 长度（≥32 字节）/弱词表检查拦不住的最常见误填形态——
    //    错误消息宣称 SHA256 hex 契约，形态检查必须真实强制该契约。
    let long_passphrase = "x".repeat(64);
    let path = temp_toml(
        "long-plaintext-api-key.toml",
        &format!("{VENDOR_TOML}\n[auth]\napi_keys = [\"{long_passphrase}\"]\n"),
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("auth.api_keys[0]"),
                "非 hex 明文错误必须报条目位置: {message}"
            );
            assert!(
                !message.contains(&long_passphrase),
                "错误不得回显条目原文: {message}"
            );
        }
        other => panic!("64 字节非 hex 明文误填必须拒绝启动，实际 {other:?}"),
    }

    // ⑤ 大写 hex 摘要同样拒绝：认证侧 sha256_hex 经 hex::encode 恒输出
    //    小写（src/middleware/auth.rs）并逐字节比较，大写条目（certutil /
    //    Get-FileHash 的真实输出形态）能过宽形态关却永远无法认证任何
    //    key——重启后全量 401 的惰性误填，必须在形态关拦下。
    let upper_digest = "A".repeat(64);
    let path = temp_toml(
        "upper-digest-api-key.toml",
        &format!("{VENDOR_TOML}\n[auth]\napi_keys = [\"{upper_digest}\"]\n"),
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains("auth.api_keys[0]"),
                "大写摘要错误必须报条目位置: {message}"
            );
            assert!(
                !message.contains(&upper_digest),
                "错误不得回显条目原文: {message}"
            );
        }
        other => panic!("大写 hex 摘要条目必须拒绝启动，实际 {other:?}"),
    }

    // ⑥ 多条目同时不达标 → 错误明细按配置索引固定顺序列出（api_keys
    //    升序）。随机序会让同一份坏配置重启后日志文案漂移，妨碍日志
    //    比对与告警去重。
    let path = temp_toml(
        "multi-weak-api-key.toml",
        &format!("{VENDOR_TOML}\n[auth]\napi_keys = [\"bad-one\", \"{digest}\", \"bad-two\"]\n"),
    );
    match load_from(&path).map(|_| ()) {
        Err(GateError::Config { message }) => {
            let first = message.find("auth.api_keys[0]").expect("必须报条目 [0]");
            let second = message.find("auth.api_keys[2]").expect("必须报条目 [2]");
            assert!(first < second, "错误明细必须按配置索引顺序列出: {message}");
            assert!(
                !message.contains("bad-one") && !message.contains("bad-two"),
                "错误不得回显条目原文: {message}"
            );
        }
        other => panic!("多条弱条目必须拒绝启动，实际 {other:?}"),
    }
}

#[test]
fn loader_reports_file_not_found_with_path() {
    let missing = std::env::temp_dir().join("nx-config-cov-definitely-missing.toml");
    let _ = std::fs::remove_file(&missing);
    match load_from(&missing).map(|_| ()) {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains(missing.to_string_lossy().as_ref()),
                "文件缺失错误必须插值携带路径: {message}"
            );
        }
        other => panic!("路径不存在必须报 Config 错误，实际 {other:?}"),
    }
}

#[test]
fn missing_api_key_env_only_warns_and_load_still_succeeds() {
    // env 键名独占（NX_COV_*_7Q2 唯一），与其他并行测试无共享态；
    // edition 2021 下 set_var/remove_var 的理论竞态随迁移 2024 需一次性初始化
    // ① env 未设置：validate 只告警不阻断，加载成功
    const UNSET: &str = "NX_COV_DEFINITELY_UNSET_7Q2";
    std::env::remove_var(UNSET);
    let path = temp_toml("env-unset.toml", &vendor_toml_with_env(UNSET));
    let manager = load_from(&path).expect("缺 env 只告警不阻断加载");
    assert_eq!(manager.read().vendors[0].api_key_env, UNSET);

    // ② env 已设置：走非告警分支，同样成功
    const SET: &str = "NX_COV_PRESENT_7Q2";
    std::env::set_var(SET, "token");
    let path = temp_toml("env-set.toml", &vendor_toml_with_env(SET));
    load_from(&path).expect("env 已设置时加载成功");
}

fn vendor_toml_with_env(env: &str) -> String {
    format!(
        r#"
[server]
host = "127.0.0.1"
[database]
[[vendors]]
id = "env_vendor"
name = "Env Vendor"
type_ = "openai"
api_key_env = "{env}"
rate_limit = {{ type = "interval", hours = 1 }}
concurrency = {{ requests_per_second = 1 }}
"#
    )
}

fn minimal_config(vendor_id: &str) -> Config {
    // 与共享基座的差异字段（显式保留原值）：本文件测 RMW/落盘链路，不解析
    // 模型——空模型列表、Interval 限流、rps 10、无全局模型是"最小可落盘"形态
    let mut vendor = common::base_vendor(vendor_id, String::new());
    vendor.name = "Cov Vendor".into();
    vendor.api_key_env = "NX_COV_KEY".into();
    vendor.models = vec![];
    vendor.rate_limit = RateLimitConfig::Interval {
        hours: 1,
        sliding: false,
    };
    vendor.concurrency.requests_per_second = 10;
    Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 8080,
        },
        global_model: None,
        ..common::test_config(vec![vendor])
    }
}

#[test]
fn update_without_path_is_rejected_and_memory_unchanged() {
    let manager = ConfigManager::new(minimal_config("v1"));
    let result = manager.update_config(minimal_config("v2"));
    assert!(
        matches!(result, Err(GateError::Config { .. })),
        "无路径记忆时 update 必须拒绝落盘（config-path-unknown）"
    );
    assert!(manager.get_vendor("v1").is_some(), "内存态不变");
    assert!(manager.get_vendor("v2").is_none(), "新配置未切换进内存");
}

#[test]
fn rmw_updates_disk_and_memory_atomically() {
    let dir = std::env::temp_dir().join(format!("nx-config-cov-rmw-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "seed").unwrap();

    let manager = ConfigManager::new(minimal_config("v1"));
    manager.set_path(path.clone());

    manager
        .update_config(minimal_config("v1"))
        .expect("seed update ok");

    // RMW 只改端口，vendor 段在写锁内基于最新内存态保留
    manager
        .update_config_with(|config| {
            config.server.port = 9449;
            Ok(())
        })
        .expect("rmw ok");

    assert_eq!(manager.read().server.port, 9449, "内存态立即切换");
    assert_eq!(
        manager.get_vendor("v1").map(|v| v.id),
        Some("v1".to_string()),
        "未改字段不得丢失"
    );
    let disk: Config =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("disk parses");
    assert_eq!(disk.server.port, 9449, "磁盘与内存一致");

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rmw_closure_error_keeps_memory_and_disk() {
    let dir = std::env::temp_dir().join(format!("nx-config-cov-rmw-err-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "seed").unwrap();

    let manager = ConfigManager::new(minimal_config("v1"));
    manager.set_path(path.clone());
    manager
        .update_config(minimal_config("v1"))
        .expect("seed update ok");
    let before = std::fs::read(&path).unwrap();

    let result = manager.update_config_with(|_config| {
        Err(GateError::Config {
            message: "mutate rejected".to_string(),
        })
    });
    assert!(result.is_err(), "闭包 Err 必须传播");
    assert_eq!(manager.read().server.port, 8080, "内存态保持闭包前值");
    assert_eq!(std::fs::read(&path).unwrap(), before, "磁盘文件字节不变");

    let _ = std::fs::remove_dir_all(dir);
}

/// 快照目录 "." 回退：相对文件名（无父目录）时，更新前快照与唯一 tmp
/// 文件都落 CWD，不 panic 不静默丢快照。
///
/// 进程 CWD 是全局态：切进独占临时目录再执行，既不向仓库工作树落盘，
/// 也保证 Drop 清理不会触达生产快照目录（`config-backups` 与
/// src/config/manager.rs 的 SNAPSHOT_DIR_NAME 同名，不能在真实 CWD 删它）。
/// 本二进制内其余测试全部使用绝对路径，切 CWD 不影响并行用例。
#[test]
fn snapshot_and_tmp_write_fall_back_to_cwd_for_bare_relative_paths() {
    let sandbox = std::env::temp_dir().join(format!(
        "nx-config-cov-cwd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&sandbox).expect("create sandbox dir");
    let original_cwd = std::env::current_dir().expect("read cwd");

    struct Guard {
        sandbox: PathBuf,
        original_cwd: PathBuf,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original_cwd);
            let _ = std::fs::remove_dir_all(&self.sandbox);
        }
    }
    let _guard = Guard {
        sandbox: sandbox.clone(),
        original_cwd,
    };
    std::env::set_current_dir(&sandbox).expect("chdir into sandbox");

    std::fs::write("nx-cov-cwd-config.toml", "seed").expect("seed cwd config");

    let manager = ConfigManager::new(minimal_config("cwd_v1"));
    manager.set_path(PathBuf::from("nx-cov-cwd-config.toml"));

    manager
        .update_config(minimal_config("cwd_v2"))
        .expect("update ok");

    let snapshots = std::fs::read_dir("config-backups")
        .expect("快照目录必须落在 CWD（空 parent 回退 \".\"）")
        .filter_map(|e| e.ok())
        .count();
    assert!(snapshots >= 1, "至少留一份更新前快照");

    let on_disk = std::fs::read_to_string("nx-cov-cwd-config.toml").unwrap();
    assert!(on_disk.contains("cwd_v2"), "磁盘写回同一路径: {on_disk}");

    let tmp_leftovers = std::fs::read_dir(".")
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("nx-cov-cwd-config.toml.tmp-")
        })
        .count();
    assert_eq!(tmp_leftovers, 0, "tmp 文件应被 rename 消费，无残留");
}

#[test]
fn routing_preset_variants_and_unknown() {
    let quality = RoutingSection::preset("quality").expect("quality 预设存在");
    assert_eq!(quality.w_model, 0.7);
    assert_eq!(quality.w_queue, 0.2);
    assert_eq!(quality.w_window, 0.1);
    assert_eq!(quality.w_latency, 0.0, "预设未列维度保持默认关闭");
    assert_eq!(quality.selection, "weighted");

    let latency = RoutingSection::preset("latency").expect("latency 预设存在");
    assert_eq!(latency.w_model, 0.2);
    assert_eq!(latency.w_queue, 0.2);
    assert_eq!(latency.w_window, 0.2);
    assert_eq!(latency.w_latency, 0.4);
    assert_eq!(latency.latency_ref_ms, 500.0);

    let cost = RoutingSection::preset("cost").expect("cost 预设存在");
    assert_eq!(cost.w_model, 0.2);
    assert_eq!(cost.w_queue, 0.2);
    assert_eq!(cost.w_window, 0.6);

    assert!(
        RoutingSection::preset("no-such-preset").is_none(),
        "未知名必须返回 None"
    );
}
