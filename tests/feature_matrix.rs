// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! nexusflow 应用面特性组合矩阵测试（自原 feature_matrix 按归属拆分）。
//!
//! 应用面 feature 只保留可见性断言；路由核心矩阵已随 llm-gate 走
//! （crates/llm-gate/tests/feature_matrix.rs）。**语义变化（design.md D1）**：
//! vendor-management→database 联动已解除——原「vendor-management 开启时
//! database 必须也可用」的链条断言随之删除，database 现由 web-server 显式
//! 携带（服务器装配 bootstrap 需要）与 database feature 自身声明。
//!
//! ## 已验证的特性组合矩阵（nexusflow 侧）
//!
//! | Feature 组合                      | 编译 | 依赖链                                     |
//! |----------------------------------|------|-------------------------------------------|
//! | `--all-features`                 | OK   | 全部                                       |
//! | `--features full`（default）     | OK   | 全部                                       |
//! | `--features database`            | OK   | database → protocol（llm-gate 转发）       |
//! | `--features vendor-management`   | OK   | vendor-management → llm-gate/vendor-management |
//! | `--features router`              | OK   | router → vendor-management + oxcache       |
//! | `--features web-server`          | OK   | web-server → router + sdforge + database   |
//! | `--features infrastructure`      | OK   | infrastructure                              |
//! | `--features logging`             | OK   | logging                                     |
//! | `--features sdk`                 | OK   | sdk（reqwest + eventsource-stream）         |
//! | `--features "web-server logging grpc"` | OK | grpc → sdforge/grpc（需 protoc）      |
//! | `--no-default-features`          | OK   | 门面 re-export（config/error/i18n/protocol/utils）|

/// database feature：验证 Database 类型可访问
#[cfg(feature = "database")]
#[test]
fn test_database_module_available() {
    use nexusflow::Database;
    let _ = std::marker::PhantomData::<Database>;
}

/// vendor-management feature：验证 VendorManager 门面可访问（llm-gate 转发）
#[cfg(feature = "vendor-management")]
#[test]
fn test_vendor_module_available() {
    use nexusflow::VendorManager;
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
    let _ = nexusflow::di::build_kit_with_config;
}

/// web-server feature：验证 ApiState 类型可访问
#[cfg(feature = "web-server")]
#[test]
fn test_api_module_available() {
    use nexusflow::ApiState;
    let _ = std::marker::PhantomData::<ApiState>;
}

/// web-server feature：验证 ApiAuthState 类型可访问
#[cfg(feature = "web-server")]
#[test]
fn test_middleware_module_available() {
    use nexusflow::ApiAuthState;
    let _ = std::marker::PhantomData::<ApiAuthState>;
}

/// 依赖链验证：web-server 开启时，database/vendor 必须也可用
///（database 自 N1 feature 重切起由 web-server 显式携带）
#[cfg(feature = "web-server")]
#[test]
fn test_web_server_dependency_chain() {
    use nexusflow::{ApiState, Database, VendorManager};
    let _ = std::marker::PhantomData::<ApiState>;
    let _ = std::marker::PhantomData::<Database>;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// 依赖链验证：router 开启时，vendor-management 必须也可用
#[cfg(feature = "router")]
#[test]
fn test_router_dependency_chain() {
    use nexusflow::VendorManager;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// sdk feature：验证 sdk 模块与客户端类型可访问
#[cfg(feature = "sdk")]
#[test]
fn test_sdk_module_available() {
    use nexusflow::sdk::{NexusFlowClient, NexusFlowSdkError, RequestConfig};
    let _ = std::marker::PhantomData::<NexusFlowClient>;
    let _ = std::marker::PhantomData::<NexusFlowSdkError>;
    let _ = RequestConfig::default();
}

/// server 模块门控（all(web-server, logging)）：run/build_app 可访问
#[cfg(all(feature = "web-server", feature = "logging"))]
#[test]
fn test_server_module_available() {
    let _ = nexusflow::server::run;
    let _ = nexusflow::server::build_app;
}

/// grpc feature：gRPC 服务器模块可访问（server 子模块，另受
/// web-server + logging 门控——实际可运行组合为三 feature 联合）
#[cfg(feature = "grpc")]
#[test]
fn test_grpc_module_available() {
    let _ = std::marker::PhantomData::<nexusflow::server::GrpcServer>;
    let _ = nexusflow::config::GrpcConfig {
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
    use nexusflow::{Config, GateError, NexusFlowError};
    let err = NexusFlowError::from(GateError::Config {
        message: "core".to_string(),
    });
    // Gate 变体 transparent 透传 llm-gate 英文规范串
    assert_eq!(err.to_string(), "Configuration error: core");
    let _ = std::marker::PhantomData::<Config>;
}
