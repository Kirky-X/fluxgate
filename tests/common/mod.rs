// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 集成测试共享装配基座（tests/ 各文件是独立编译单元，经 `mod common;`
//! 引入本模块；未被某二进制用到的导出项以 `#![allow(dead_code)]` 放行）。
//!
//! 收敛目标：Config / VendorConfig 的 18 行字面量基座此前在 10+ 个测试
//! 文件各复制一份（字段新增需同步改 35+ 处）。本模块提供
//! 「多数派缺省 + 调用点 struct-update 覆盖差异」的单一事实源；有语义含义
//! 的差异（如 CB 失败计数依赖 `retry.max_retries = 0`）必须留在调用点
//! 覆盖处并以注释说明，禁止静默吞进基座。

#![allow(dead_code)]

use fluxgate::config::{
    ConcurrencyConfig, Config, DatabaseConfig, GlobalModelConfig, ModelConfig, RateLimitConfig,
    ServerConfig, VendorConfig,
};
use std::sync::Once;

/// mock 厂商凭证的环境变量名（各测试装配统一走它，值恒定）
pub const TEST_API_KEY_ENV: &str = "FLUXGATE_TEST_KEY";

/// mock 厂商凭证值（出站鉴权断言复用）。
pub const TEST_API_KEY_VALUE: &str = "test-key-for-mockito";

/// 进程内一次性注入 mock 厂商凭证。
///
/// edition 2021 下 `set_var` 与并行测试线程的 getenv 存在理论竞态：收敛到
/// 单点后同值只写一次，窗口收窄到进程首装配；迁移 edition 2024 时此处是
/// 唯一需要改 unsafe 块/一次性初始化的位置。
pub fn ensure_test_api_key() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::env::set_var(TEST_API_KEY_ENV, TEST_API_KEY_VALUE);
    });
}

/// 多数派缺省 vendor：openai 型、大 Token 窗口、rps 1000、单模型 gpt-4。
/// 差异字段（api_key_env、权重、并发上限等）由调用点 struct-update 覆盖。
pub fn base_vendor(id: &str, api_base: impl Into<String>) -> VendorConfig {
    VendorConfig {
        timeout_ms: 60_000,
        stream_ttft_ms: 60_000,
        stream_idle_ms: 30_000,
        supports_batch: false,
        chat_path: None,
        api_style: None,
        id: id.into(),
        name: format!("Test {}", id),
        type_: "openai".into(),
        api_key_env: TEST_API_KEY_ENV.into(),
        api_key_envs: Vec::new(),
        extra_headers: Default::default(),
        api_base: api_base.into(),
        enabled: true,
        priority: 1,
        models: vec![ModelConfig {
            model_id: "gpt-4".into(),
            abstract_id: Some("gpt-4".into()),
            weight: 1.0,
            enabled: true,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }],
        rate_limit: RateLimitConfig::Token {
            max_tokens: 1_000_000,
            window_hours: None,
            sliding: false,
        },
        concurrency: ConcurrencyConfig {
            requests_per_second: 1000,
            max_concurrent_requests: None,
            token_weighted: false,
            token_ticket_min: 1,
            token_ticket_max: 16384,
            token_ticket_default: 4096,
        },
    }
}

/// 多数派缺省 Config：回环地址 + 内存库 + 全局模型 weighted 开启 +
/// 其余段取 Default。端口、retry 上限、认证等由调用点覆盖。
pub fn test_config(vendors: Vec<VendorConfig>) -> Config {
    Config {
        rate_limit: Default::default(),
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
        },
        database: DatabaseConfig {
            path: ":memory:".into(),
            pool_size: 1,
            url: String::new(),
        },
        vendors,
        global_model: Some(GlobalModelConfig {
            enabled: true,
            strategy: "weighted".into(),
        }),
        pricing: vec![],
        model_catalog: Vec::new(),
        auth: None,
        cors: None,
        retry: Default::default(),
        routing: Default::default(),
        cache: Default::default(),
        admission: Default::default(),
        health: Default::default(),
        quota: Default::default(),
        grpc: None,
        resilience: Default::default(),
        docs: Default::default(),
        lexcompress: Default::default(),
        responses: Default::default(),
    }
}
