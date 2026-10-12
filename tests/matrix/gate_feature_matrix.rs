// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 路由半边特性组合矩阵测试：验证不同 feature 组合下路由核心模块的
//! 可见性。随 cargo test --workspace 全部断言运行；特定组合下只编译
//! 对应 cfg 块
//!
//! ## 已验证的特性组合矩阵（路由半边）
//!
//! | Feature 组合                      | 编译 | 依赖链                          |
//! |----------------------------------|------|--------------------------------|
//! | `--all-features`                 | ✗ 预期失败 | db-* 三组互斥（dbnexus 编译期守卫） |
//! | `--features full`（default）     | OK   | 全部                            |
//! | `--features protocol`            | OK   | protocol（marker）              |
//! | `--features vendor-management`   | OK   | vendor-management（无存储联动） |
//! | `--features router`              | OK   | router → vendor-management      |
//! | `--features resilience`          | OK   | resilience（纯门控）            |
//! | `--features cost-tracking`       | OK   | cost-tracking（dashmap）        |
//! | `--no-default-features`          | OK   | 仅核心（config/error/i18n/protocol/utils）|
//!
//! ## Feature 依赖图（路由半边）
//!
//! ```text
//! router ──→ vendor-management ──→ {reqwest, limiteron, rand, async-channel}
//!        └─→ oxcache
//! resilience / cost-tracking / protocol（marker）独立
//! ```
//!
//! 注意：vendor-management 不再联动任何存储 feature（纯路由零存储，
//! 应用面组合矩阵见 tests/feature_matrix.rs）。

/// 验证 GateError 在所有组合下都可用（核心错误类型，常开）
#[test]
fn test_core_error_always_available() {
    let err = nexusflow::GateError::Config {
        message: "test".to_string(),
    };
    // 双轨约定：Display 恒英文规范串，本地化文案走 user_message()（llm-gate error 模块）
    assert_eq!(err.to_string(), "Configuration error: test");
}

/// 验证 config 模块在所有组合下都可用（常开）
#[test]
fn test_config_always_available() {
    let server_config = nexusflow::config::ServerConfig {
        host: "0.0.0.0".to_string(),
        port: 8080,
    };
    assert_eq!(server_config.host, "0.0.0.0");
    assert_eq!(server_config.port, 8080);
}

/// protocol marker feature：验证 UnifiedRequest 类型可访问（常开模块）
#[cfg(feature = "protocol")]
#[test]
fn test_protocol_module_available() {
    use nexusflow::protocol::UnifiedRequest;
    let _ = std::marker::PhantomData::<UnifiedRequest>;
}

/// vendor-management feature：验证 VendorManager 类型可访问
#[cfg(feature = "vendor-management")]
#[test]
fn test_vendor_module_available() {
    use nexusflow::vendor::VendorManager;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// router feature：验证 Router 类型可访问
#[cfg(feature = "router")]
#[test]
fn test_router_module_available() {
    use nexusflow::router::Router;
    let _ = std::marker::PhantomData::<Router>;
}

/// 依赖链验证：router 开启时，vendor-management 必须也可用
#[cfg(feature = "router")]
#[test]
fn test_router_dependency_chain() {
    use nexusflow::vendor::VendorManager;
    let _ = std::marker::PhantomData::<VendorManager>;
}

/// core-only 等价验证：vendor 栈关闭不侵蚀核心类型。
/// （模块不存在无法在函数体内断言——引用即编译错误；core-only 组合
/// 编译本身由 `cargo check --no-default-features` 命令验证。）
#[cfg(not(feature = "vendor-management"))]
#[test]
fn test_core_types_without_vendor_stack() {
    use nexusflow::{Config, GateError};
    let err = GateError::Config {
        message: "core".to_string(),
    };
    assert_eq!(err.to_string(), "Configuration error: core");
    let _ = std::marker::PhantomData::<Config>;
}
