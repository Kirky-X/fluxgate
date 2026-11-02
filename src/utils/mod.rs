// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

// metrics 观测核：消费者全在 vendor-management 传递范围内，常开基座零引用，
// 随 vendor-management 门控（精简/无 vendor 组合不编译）。
#[cfg(feature = "vendor-management")]
pub mod metrics;
pub mod retry;
/// 崩溃安全状态文件读取（magpie-absorb-r1：主坏隔离+`.bak` 回退）。
pub mod state_file;
// 租户身份辅助：消费面全在 api/middleware（web-server），且依赖 axum/sha2/hex
// 均为 web-server 面的直接依赖——随 web-server 门控，避免精简组合拉入缺席依赖。
#[cfg(feature = "web-server")]
pub mod tenant;
// OIDC 短时键别名表（auth-and-masking R-auth-002 常驻半边）：纯 std 单锁表，
// oidc feature 的验签路径写、api_key_digest/档位解析读；表空零开销直通。
pub mod oidc_keys;
pub mod tokens;

// 出站 SSRF 防护核（web-search-inject）：特殊网段判定 + GuardedFetcher
//（fetcher 面依赖 reqwest，随 web-search 门控——netguard 单开只编译纯判定）。
#[cfg(feature = "netguard")]
pub mod netguard;

#[cfg(feature = "cost-tracking")]
pub mod cost_tracker;

// 离线成本回放（cost-replay）：feature 门控的只读分析引擎，随独立 bin 提供。
#[cfg(feature = "cost-replay")]
pub mod replay;

// OTLP 桥（usage-observability）：otlp feature 隐含 cost-tracking 与
// sdforge-integration（文本真源 + 成本快照为其编译前提），见 Cargo.toml 特性注释。
#[cfg(feature = "otlp")]
pub mod otlp;

// OTLP 尝试级追踪（usage-observability R-otlp-001/002）：与 otlp.rs（Prometheus
// 文本桥）职责分离——本模块是 trace 面（span），otlp.rs 是 metrics 面。
#[cfg(feature = "otlp")]
pub mod otlp_export;

#[cfg(feature = "vendor-management")]
pub use metrics::{GlobalMetrics, MetricsSnapshot, VendorMetrics};
pub use retry::{extract_status_code, ErrorCategory};
pub use tokens::{
    estimate_content_tokens, estimate_content_tokens_with, estimate_messages_tokens,
    estimate_messages_tokens_with, estimate_text_tokens, estimate_text_tokens_with,
    BLOCK_STRUCT_OVERHEAD, CHARS_PER_TOKEN, MESSAGE_STRUCT_OVERHEAD,
};

#[cfg(feature = "cost-tracking")]
pub use cost_tracker::{CostSnapshot, CostTracker};
