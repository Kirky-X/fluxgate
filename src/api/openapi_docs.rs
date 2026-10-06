// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! OpenAPI 文档面：主 REST 端点经 sdforge
//! 原生 inventory 机制（`OpenApiRouteInfo` + `inventory::submit!`，与
//! `#[forge]` 宏同源）注册进全局 spec；文档路由（`/api-docs/*` +
//! `/swagger-ui/*`）由 `[docs].enabled` 开关挂载（默认 false = 404）。
//!
//! 安全姿态：开关本身即访问门——打开后文档静态面免认证免限流（浏览器可用性；
//! spec 非敏感）；默认关闭与网关攻击面最小化一致。
//! 路径穿越：已核实——sdforge serve_swagger_ui 拒绝含
//! `..` 的请求且资源来自内嵌包（非文件系统），`{*rest}` 无穿越面。

/// OpenAPI 版本号（全条目统一；升级大版本时改此处一处）。
const OPENAPI_VERSION: &str = "v1";

/// 提交简单条目（无路径参数）。
macro_rules! submit_route {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo::new(
                $path, $method, $summary, $desc, OPENAPI_VERSION, $tags
            )
        }
    };
}

/// 提交带 `{id}` 路径参数的条目。
macro_rules! submit_route_with_id {
    ($path:expr, $method:expr, $summary:expr, $desc:expr, $tags:expr, $id_desc:expr) => {
        sdforge::inventory::submit! {
            sdforge::openapi::OpenApiRouteInfo::with_path_params(
                $path,
                $method,
                $summary,
                $desc,
                OPENAPI_VERSION,
                $tags,
                &[sdforge::openapi::OpenApiPathParam::new(
                    "id", $id_desc, true, "string", ""
                )],
            )
        }
    };
}

// OpenAI 兼容面
submit_route!(
    "/v1/chat/completions",
    "POST",
    "OpenAI chat completions",
    "Unified chat completions (OpenAI wire format) across vendors.",
    &["openai"]
);
submit_route!(
    "/v1/messages",
    "POST",
    "Anthropic messages",
    "Unified messages API (Anthropic wire format) across vendors.",
    &["anthropic"]
);
submit_route!(
    "/v1/embeddings",
    "POST",
    "OpenAI embeddings",
    "Unified embeddings API across vendors.",
    &["openai"]
);
// 管理面
submit_route!(
    "/admin/logs",
    "GET",
    "Query request logs",
    "Paginated request log query with filters.",
    &["admin"]
);
submit_route!(
    "/admin/logs/search",
    "GET",
    "Search request logs",
    "Keyword search over request logs.",
    &["admin"]
);
submit_route!(
    "/admin/stats/tokens",
    "GET",
    "Token usage stats",
    "Aggregated token usage statistics.",
    &["admin"]
);
submit_route!(
    "/admin/metrics",
    "GET",
    "Operational metrics",
    "Internal operational metrics snapshot (JSON).",
    &["admin"]
);
submit_route!(
    "/admin/vendors",
    "GET",
    "List vendors",
    "Vendor status and pool overview.",
    &["admin"]
);
submit_route!(
    "/admin/config",
    "GET",
    "Get runtime config",
    "Current runtime configuration snapshot.",
    &["admin"]
);
submit_route!(
    "/admin/config",
    "POST",
    "Update runtime config",
    "Hot-update runtime configuration (validated, atomic).",
    &["admin"]
);
submit_route!(
    "/admin/models",
    "GET",
    "List models",
    "Model catalog listing.",
    &["admin"]
);
submit_route_with_id!(
    "/admin/vendors/{id}/status",
    "GET",
    "Vendor status",
    "Per-vendor status detail.",
    &["admin"],
    "Vendor ID"
);
submit_route_with_id!(
    "/admin/vendors/{id}/reset",
    "POST",
    "Reset vendor",
    "Reset per-vendor breakers and quotas.",
    &["admin"],
    "Vendor ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "POST",
    "Update model entry",
    "Legacy global-strategy update path (kept for compatibility).",
    &["admin"],
    "Model ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "PUT",
    "Upsert model entry",
    "Create or replace a catalog entry.",
    &["admin"],
    "Model ID"
);
submit_route_with_id!(
    "/admin/models/{id}",
    "DELETE",
    "Delete model entry",
    "Remove a catalog entry.",
    &["admin"],
    "Model ID"
);
// 健康与基础设施
submit_route!(
    "/api/v1/health",
    "GET",
    "Liveness probe",
    "Forge-registered liveness endpoint.",
    &["health"]
);
submit_route!(
    "/readyz",
    "GET",
    "Readiness probe",
    "Aggregated component readiness (DB, cache, vendors).",
    &["health"]
);
submit_route!(
    "/metrics",
    "GET",
    "Prometheus metrics",
    "RED metrics in Prometheus text exposition format.",
    &["health"]
);
