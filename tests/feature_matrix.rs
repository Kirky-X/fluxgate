// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 特性组合矩阵测试
//!
//! 验证不同 feature 组合下模块的可见性是否符合预期。
//! 此文件本身在 `--all-features` 下编译运行所有断言；
//! 在特定 feature 组合下只编译对应的 cfg 块。
//!
//! ## 已验证的特性组合矩阵
//!
//! | Feature 组合                      | 编译  | 依赖链                                  |
//! |----------------------------------|-------|----------------------------------------|
//! | `--all-features`                 | OK    | 全部                                    |
//! | `--features full`                | OK    | 全部                                    |
//! | `--features protocol`            | OK    | protocol                                |
//! | `--features infrastructure`      | OK    | infrastructure                          |
//! | `--features logging`             | OK    | logging                                |
//! | `--features database`            | OK    | database → protocol                     |
//! | `--features vendor-management`   | OK    | vendor-management → database → protocol |
//! | `--features router`              | OK    | router → vendor-management → database   |
//! | `--features web-server`          | OK    | web-server → router → ... → sdforge     |
//! | `--features sdforge-integration` | OK    | sdforge-integration                     |
//! | `--features "protocol infra"`    | OK    | protocol + infrastructure               |
//! | `--features "protocol logging"`  | OK    | protocol + logging                      |
//! | `--features sdk`                 | OK    | sdk（reqwest + eventsource-stream）      |
//! | `--features "web-server logging grpc"` | OK | grpc → sdforge/grpc（另受 web-server+logging 门控，需 protoc）|
//! | `--no-default-features`          | OK    | 仅核心（config/error/i18n/protocol/utils）|
//!
//! ## Feature 依赖图
//!
//! ```text
//! web-server ──┬─→ router ──→ vendor-management ──→ database ──→ protocol
//!              └─→ sdforge-integration
//! grpc            (独立 feature；server::grpc 实际可见组合 = web-server + logging + grpc)
//! infrastructure  (独立)
//! logging         (独立)
//! ```

use nexusflow::NexusFlowError;

/// 验证 NexusFlowError 在所有组合下都可用（核心错误类型）
#[test]
fn test_core_error_always_available() {
    let err = NexusFlowError::Config {
        message: "test".to_string(),
    };
    // 双轨约定：Display 恒英文规范串，本地化文案走 user_message()（src/error.rs:13）
    assert_eq!(err.to_string(), "Configuration error: test");
}

/// 验证 config 模块在所有组合下都可用
#[test]
fn test_config_always_available() {
    let server_config = nexusflow::ServerConfig {
        host: "0.0.0.0".to_string(),
        port: 8080,
    };
    assert_eq!(server_config.host, "0.0.0.0");
    assert_eq!(server_config.port, 8080);
}

/// protocol feature：验证 UnifiedRequest 类型可访问
#[cfg(feature = "protocol")]
#[test]
fn test_protocol_module_available() {
    use nexusflow::protocol::UnifiedRequest;
    let _ = std::marker::PhantomData::<UnifiedRequest>;
}

/// database feature：验证 Database 类型可访问
#[cfg(feature = "database")]
#[test]
fn test_database_module_available() {
    use nexusflow::Database;
    let _ = std::marker::PhantomData::<Database>;
}

/// vendor-management feature：验证 VendorManager 类型可访问
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

/// 依赖链验证：web-server 开启时，router/vendor-management/database 必须也可用
#[cfg(feature = "web-server")]
#[test]
fn test_web_server_dependency_chain() {
    use nexusflow::{ApiState, Database, VendorManager};
    let _ = std::marker::PhantomData::<ApiState>;
    let _ = std::marker::PhantomData::<Database>;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// 依赖链验证：vendor-management 开启时，database 必须也可用
#[cfg(feature = "vendor-management")]
#[test]
fn test_vendor_management_dependency_chain() {
    use nexusflow::{Database, VendorManager};
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

/// core-only 等价验证：sdk 关闭不侵蚀核心类型。
/// （模块不存在无法在函数体内断言——引用即编译错误；core-only 组合
/// 编译本身由 `cargo check --no-default-features` 命令验证。如实注明：
/// 该组合无 CI 持续强制——ci.yml check 仅跑 --workspace 与 --all-features。）
#[cfg(not(feature = "sdk"))]
#[test]
fn test_core_types_without_sdk() {
    use nexusflow::{Config, NexusFlowError};
    let err = NexusFlowError::Config {
        message: "core".to_string(),
    };
    assert_eq!(err.to_string(), "Configuration error: core");
    let _ = std::marker::PhantomData::<Config>;
}
