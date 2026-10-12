// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! OpenAPI 文档面：主 REST 端点经 sdforge inventory 机制注册进全局 spec；
//! 文档路由由 [docs].enabled 开关挂载（默认 false = 404）——开关本身即
//! 访问门（打开后免认证免限流，spec 非敏感）

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
submit_route!(
    "/v1/models",
    "GET",
    "OpenAI models list",
    "Union of model catalog and enabled vendor models, deduplicated. Never cached.",
    &["openai"]
);
submit_route!(
    "/v1/messages/count_tokens",
    "POST",
    "Anthropic token count",
    "Approximate input token estimation (chars/4 + per-message overhead, deterministic).",
    &["anthropic"]
);
submit_route!(
    "/admin/route/explain",
    "POST",
    "Route explanation",
    "Read-only per-candidate routing assessment (reason codes + weight breakdown).",
    &["admin"]
);
submit_route!(
    "/admin/cost",
    "GET",
    "Cost ledger query",
    "Aggregated token and cost ledger grouped by vendor or model, with optional time window and API key filter.",
    &["admin"]
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
