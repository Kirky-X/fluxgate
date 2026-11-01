// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! fluxgate 应用面特性组合矩阵测试。
//! 应用面 feature 只保留可见性断言；路由核心矩阵见 gate_feature_matrix.rs。
//! vendor-management→database 联动已解除，database 由 web-server 显式携带
//! 与 database feature 自身声明
//!
//! ## 已验证的特性组合矩阵（fluxgate 侧）
//!
//! | Feature 组合                      | 编译 | 依赖链                                     |
//! |----------------------------------|------|-------------------------------------------|
//! | `--all-features`                 | ✗ 预期失败 | db-sqlite/postgres/mysql 三组互斥，dbnexus 编译期守卫必触发 `compile_error!` |
//! | `--features full`（default = full + db-sqlite） | OK | 全面向 sqlite 后端        |
//! | `-p fluxgate --no-default-features --features "full,db-postgres"` | OK | server-side 后端 |
//! | `-p fluxgate --no-default-features --features "full,db-mysql"`    | OK | server-side 后端 |
//! | `--no-default-features --features "database,db-sqlite"` | OK | database → protocol + url + logging（inklog kit 需任一 db-* 组） |
//! | `--features vendor-management`   | OK   | vendor-management 本模块直控               |
//! | `--features router`              | OK   | router → vendor-management + oxcache       |
//! | `--features infrastructure`      | OK   | infrastructure → oxcache + trait-kit       |
//! | `--no-default-features --features "web-server,db-sqlite"` | OK | web-server → router + sdforge + database + infrastructure + hex/sha2/subtle/once_cell |
//! | `--no-default-features --features "logging,db-sqlite"` | OK | logging（inklog kit 需任一 db-* 组，单独开不编译） |
//! | `--features sdk`                 | OK   | sdk（reqwest + eventsource-stream + url）   |
//! | `--features "web-server logging grpc"` | OK | grpc → sdforge/grpc（需 protoc）      |
//! | `--no-default-features`          | OK   | 门面 re-export（config/error/i18n/protocol/utils）|
//! | `--no-default-features --features "db-sqlite,vendor-management,masking"` | OK | masking 不进 full、无 resilience——非流式回程还原分支的编译面（E0061 回归钉） |
//! | `--no-default-features --features "db-sqlite,vendor-management,subscription-vendor,masking"` | OK | 订阅池 + 掩码全开（vault 联动）|
//! | `--no-default-features --features "cost-replay,db-sqlite"` | OK | cost-replay 不进 full → database + dbnexus/sea-orm，独立 bin `fluxgate-replay` |
//!
//! 【门禁口径】默认特性集（full + db-sqlite）覆盖不到上两行——`masking`
//! 刻意不进 `full`，`cargo check --benches` 与 clippy 默认面也不含。组合
//! 编译随修复/复审以 cargo check 显式复验（见审查报告口径）。
//!
//! 【矩阵命令口径（重要）】非默认后端必须用 `-p fluxgate` 限定到本包：
//! `cargo check --workspace --no-default-features --features "full,db-postgres"`
//! 会失败——`fluxgate-examples` 成员（default 含 `embedded` feature）以
//! `fluxgate = { path = ".." }` 拉入 `full` + `db-sqlite`，Cargo 特性联合把 sqlite 注入共享的
//! dbnexus 依赖，与 db-postgres 同时置位即触发其互斥守卫。

/// database feature：验证 Database 类型可访问
#[cfg(feature = "database")]
#[test]
fn test_database_module_available() {
    use fluxgate::Database;
    let _ = std::marker::PhantomData::<Database>;
}

/// vendor-management feature：验证 VendorManager 门面可访问（llm-gate 转发）
#[cfg(feature = "vendor-management")]
#[test]
fn test_vendor_module_available() {
    use fluxgate::VendorManager;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// infrastructure feature：验证 di 模块可访问
#[cfg(all(
    feature = "infrastructure",
    feature = "database",
    feature = "vendor-management",
    feature = "logging"
))]
#[test]
fn test_infrastructure_module_available() {
    // di 模块路径存在即表示 infrastructure feature 可用
    let _ = fluxgate::di::build_kit_with_config;
}

/// web-server feature：验证 ApiState 类型可访问
#[cfg(feature = "web-server")]
#[test]
fn test_api_module_available() {
    use fluxgate::ApiState;
    let _ = std::marker::PhantomData::<ApiState>;
}

/// web-server feature：验证 ApiAuthState 类型可访问
#[cfg(feature = "web-server")]
#[test]
fn test_middleware_module_available() {
    use fluxgate::ApiAuthState;
    let _ = std::marker::PhantomData::<ApiAuthState>;
}

/// 依赖链验证：web-server 开启时，database/vendor 必须也可用
///（database 自 N1 feature 重切起由 web-server 显式携带）
#[cfg(feature = "web-server")]
#[test]
fn test_web_server_dependency_chain() {
    use fluxgate::{ApiState, Database, VendorManager};
    let _ = std::marker::PhantomData::<ApiState>;
    let _ = std::marker::PhantomData::<Database>;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// 依赖链验证：router 开启时，vendor-management 必须也可用
#[cfg(feature = "router")]
#[test]
fn test_router_dependency_chain() {
    use fluxgate::VendorManager;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// 依赖链验证：database 开启时 logging 与 dep:trait-kit 必须也可用
///（async_logger 无条件消费 inklog；脱敏面自 inklog rc.12 起消费
/// trait-kit 的 MaskError/SecretScanGate，故 database→trait-kit 亦为编译前提）
#[cfg(feature = "database")]
#[test]
fn test_database_logging_chain() {
    let _ = std::marker::PhantomData::<fluxgate::database::Database>;
    #[cfg(not(feature = "logging"))]
    compile_error!("database 开启必须携带 logging（async_logger 消费 inklog）");
    // `dep:trait-kit` 不是可探测 feature；用其 masking 面符号作在场断言
    #[cfg(feature = "database")]
    let _ = std::marker::PhantomData::<trait_kit::masking::MaskError>;

    #[cfg(feature = "database")]
    let _: fn() = || {
        // 编译期断言：脱敏面类型在 database 组合下必须可见
        let _ = std::marker::PhantomData::<trait_kit::masking::MaskError>;
    };
}

/// 依赖链验证：web-server 开启时 infrastructure 必须也可用
///（server/shutdown 使用 trait-kit 协调器，web-server→infrastructure 为编译前提）
#[cfg(feature = "web-server")]
#[test]
fn test_web_server_infrastructure_chain() {
    // fluxgate::di 受 infrastructure+database+vendor-management+logging
    // 四联动门控，web-server 组合下全部在场
    let _ = fluxgate::di::verify_kit;
    #[cfg(not(feature = "infrastructure"))]
    compile_error!("web-server 开启必须携带 infrastructure（shutdown 用 trait-kit）");
}

/// sdk feature：验证 sdk 模块与客户端类型可访问
#[cfg(feature = "sdk")]
#[test]
fn test_sdk_module_available() {
    use fluxgate::sdk::{FluxgateClient, FluxgateSdkError, RequestConfig};
    let _ = std::marker::PhantomData::<FluxgateClient>;
    let _ = std::marker::PhantomData::<FluxgateSdkError>;
    let _ = RequestConfig::default();
}

/// server 模块门控（all(web-server, logging)）：run/build_app 可访问
#[cfg(all(feature = "web-server", feature = "logging"))]
#[test]
fn test_server_module_available() {
    let _ = fluxgate::server::run;
    let _ = fluxgate::server::build_app;
}

/// grpc feature：gRPC 服务器模块可访问（server 子模块，另受
/// web-server + logging 门控——实际可运行组合为三 feature 联合）
#[cfg(feature = "grpc")]
#[test]
fn test_grpc_module_available() {
    let _ = std::marker::PhantomData::<fluxgate::server::GrpcServer>;
    let _ = fluxgate::config::GrpcConfig {
        enabled: true,
        ..Default::default()
    };
}

/// core-only 等价验证：sdk 关闭不侵蚀门面核心类型。
/// （模块不存在无法在函数体内断言——引用即编译错误；core-only 组合
/// 编译本身由 `cargo check --no-default-features` 命令验证。）
#[cfg(not(feature = "sdk"))]
#[test]
fn test_core_types_without_sdk() {
    use fluxgate::{Config, FluxgateError, GateError};
    let err = FluxgateError::from(GateError::Config {
        message: "core".to_string(),
    });
    // Gate 变体 transparent 透传 llm-gate 英文规范串
    assert_eq!(err.to_string(), "Configuration error: core");
    let _ = std::marker::PhantomData::<Config>;
}

/// masking feature：掩码门与占位符注册表可见（不进 full，组合编译面回归钉）
#[cfg(feature = "masking")]
#[test]
fn test_masking_module_available() {
    let _ = fluxgate::masking::MaskGate::new;
    let _ = fluxgate::masking::PlaceholderRegistry::new;
}

/// subscription-vendor feature：订阅池可见（不进 full，装配面回归钉）
#[cfg(feature = "subscription-vendor")]
#[test]
fn test_subscription_module_available() {
    let _ = fluxgate::vendor::subscription::pool::AccountPool::new;
}

/// cost-replay feature：回放引擎与报告类型可见（不进 full，独立 bin 的库面回归钉）
#[cfg(feature = "cost-replay")]
#[test]
fn test_cost_replay_module_available() {
    let _ = fluxgate::utils::replay::load_rows;
    let empty: &[fluxgate::utils::replay::ReplayRow] = &[];
    let report = fluxgate::utils::replay::build_report(empty, &[], 0, 0, "test");
    assert_eq!(report.requests, 0);
    assert_eq!(
        report.scenarios.len(),
        fluxgate::utils::replay::TTL_SWEEP.len() + 1,
        "legacy + TTL_SWEEP 三档"
    );
}
