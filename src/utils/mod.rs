// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

// metrics 观测核：消费者全在 vendor-management 传递范围内，常开基座零引用，
// 随 vendor-management 门控（精简/无 vendor 组合不编译）。
#[cfg(feature = "vendor-management")]
pub mod metrics;
pub mod retry;
// 租户身份辅助：消费面全在 api/middleware（web-server），且依赖 axum/sha2/hex
// 均为 web-server 面的直接依赖——随 web-server 门控，避免精简组合拉入缺席依赖。
#[cfg(feature = "web-server")]
pub mod tenant;
pub mod tokens;

#[cfg(feature = "cost-tracking")]
pub mod cost_tracker;

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
