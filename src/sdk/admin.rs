// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Admin / health 端点客户端方法 — 路径与形状对齐网关真实路由表（
//! 失配修复；防回归钉见 `tests/integration_admin.rs` 的路由 diff 测试）。

use serde::Deserialize;

use crate::sdk::client::FluxweirClient;
use crate::sdk::error::FluxweirSdkError;
use crate::sdk::transport::send_with_retry;

/// 模型能力目录条目 — 对齐网关 `GET /admin/models`（目录）。
/// 定价单位 =「currency 计价，每 1M tokens」。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelCatalogInfo {
    pub model_id: String,
    pub context_window: u64,
    #[serde(default)]
    pub input_price: f64,
    #[serde(default)]
    pub output_price: f64,
    #[serde(default)]
    pub cache_read_price: f64,
    #[serde(default)]
    pub cache_creation_price: f64,
    /// `usd` | `cny`（四价共用币种）
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_thinking: bool,
}

/// Anthropic 缓存断点策略 upsert 载荷 — 镜像网关
/// `CacheBreakpointPolicy`（字段语义见其 doc：锚序 tools→system→last_user，
/// 超限保留尾部；平台断点上限 4）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CacheBreakpointPolicyUpsert {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_breakpoints: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchors: Option<Vec<String>>,
}

/// 目录条目 upsert 载荷（构造侧；`model_id` 以 URL path 为权威——
/// 网关覆写 body 中 model_id）。`Default`：context_window=0 会被网关
/// validate 拒绝（登记条目必须真实窗口），仅作字段展开基座。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ModelCatalogUpsert {
    pub model_id: String,
    pub context_window: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    /// 工具面：anthropic 型 vendor 转换出站的缓存断点策略，
    /// 形状对齐 `[[model_catalog.cache_breakpoints]]` 子表（审查 LOW：
    /// 镜像类型化结构体取代裸 Value——upsert 面拼写错误在编译期暴露）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_breakpoints: Option<CacheBreakpointPolicyUpsert>,
}

/// vendor 运行时状态 — 对齐网关 `GET /admin/vendors` 真实形状
///（{id,status,queue_length} + key_pool；旧 {id,name,enabled} 为
/// 配置面形状，运行面不存在）。
#[derive(Debug, Clone, Deserialize)]
pub struct VendorStatusInfo {
    pub id: String,
    /// `Available` | `Unavailable` | `Throttled` | `Busy`（Debug 词）
    pub status: String,
    pub queue_length: usize,
    /// key 池观测（仅 env 名/数量/下标/指纹，无 key 原文；未初始化 None）
    #[serde(default)]
    pub key_pool: Option<KeyPoolInfo>,
}

/// vendor key 池观测快照。
#[derive(Debug, Clone, Deserialize)]
pub struct KeyPoolInfo {
    /// 合并去重定序后的 env 名列表
    pub envs: Vec<String>,
    /// 解析成功的 key 数量（0 = 池耗尽/全缺 env，已回退 legacy 单 key）
    pub size: usize,
    /// 当前游标下标
    pub current_index: usize,
    /// 各 key 的 16 位十六进制指纹（进程内随机密钥，不可离线复算）
    pub fingerprints: Vec<String>,
}

/// 健康状态 — 对齐网关 `GET /api/v1/health`（`admin/health` 不存在）。
#[derive(Debug, Clone, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub version: String,
    pub timestamp: String,
}

/// 配额账本条目 — 对齐网关 `GET /admin/quota/ledger` 的 `rows[]`
///（quota-aware-routing：多窗预算账本读侧）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QuotaLedgerInfo {
    /// 租户摘要 / `__global__` / `__vendor__:{id}`
    pub api_key_hash: String,
    pub window_id: String,
    /// `tenant` | `global` | `vendor`
    pub scope: String,
    pub window_start: i64,
    pub tokens_used: i64,
    pub cost_used_millicents: i64,
    /// 重置时刻（Unix 秒）
    pub reset_at: i64,
}

/// 配额账本响应。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QuotaLedgerResponse {
    pub rows: Vec<QuotaLedgerInfo>,
}

/// vendor 预算状态条目 — 对齐网关 `GET /admin/vendor-budgets` 的 `rows[]`。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VendorBudgetInfo {
    pub vendor_id: String,
    pub tokens_limit: i64,
    pub cost_limit_millicents: i64,
    pub tokens_used: i64,
    pub cost_used_millicents: i64,
    /// 消耗率比（pace 口径：>1.1 超带 / <0.9 欠带）
    pub pace_ratio: f64,
    pub depleted: bool,
    /// 距重置秒数 / 窗长秒数
    pub reset_secs: u64,
    pub period_secs: u64,
}

/// vendor 预算状态响应。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VendorBudgetsResponse {
    pub rows: Vec<VendorBudgetInfo>,
}

/// 目录同步 diff 条目 — 对齐网关 `POST /admin/catalog/sync` 的 `entries[]`
///（`price_source: "fallback"` 标注该条目至少一档价格回退 catalog 默认 0 价）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CatalogSyncEntry {
    pub vendor_id: String,
    pub model_id: String,
    /// `source`（四档来自源）| `fallback`（缺档回退）
    pub price_source: String,
}

/// 目录同步响应 — 对齐网关 `POST /admin/catalog/sync`（只增不改合并计划；
/// `status = "not_modified"` 时为 304 短路形状，entries 为空）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CatalogSyncResponse {
    pub dry_run: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub added: usize,
    #[serde(default)]
    pub skipped: usize,
    #[serde(default)]
    pub duplicates: usize,
    #[serde(default)]
    pub unchanged: usize,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub entries: Vec<CatalogSyncEntry>,
}

#[derive(Deserialize)]
struct AdminActionResponse {
    success: bool,
    #[serde(default)]
    message: String,
}

/// 会话聚合条目 — 对齐网关 `GET /admin/sessions` 的 `rows[]`。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub session_key: String,
    pub request_count: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_cost: f64,
    pub first_at: i64,
    pub last_at: i64,
    /// 首条用户消息前 80 字符
    pub title: String,
}

/// 会话列表响应（含服务端生效的 limit）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SessionsResponse {
    pub limit: u32,
    pub rows: Vec<SessionInfo>,
}

/// 上下文利用率条目 — 对齐网关 `GET /admin/context-stats` 的 `rows[]`。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ContextStatInfo {
    pub model: String,
    pub request_count: i64,
    pub avg_occupancy: f64,
    pub cache_read_share: f64,
    /// 五桶：<20% / 20-40% / 40-60% / 60-80% / >=80%
    pub bucket_counts: [i64; 5],
}

/// 上下文统计响应。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ContextStatsResponse {
    pub rows: Vec<ContextStatInfo>,
}

/// 目录条目模型 URL — `model_id` 经 `path_segments_mut.push` 自动
/// 百分号编码（审查 MEDIUM：字符串 join 对 `../` 类输入产生路径穿越，
/// `push` 将段内 `/` 编码为 `%2F`，穿越Neutral 化）。
fn admin_model_url(base_url: &url::Url, model_id: &str) -> Result<url::Url, FluxweirSdkError> {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .map_err(|_| FluxweirSdkError::Config {
            message: "base_url cannot be a base".into(),
        })?
        .push("admin")
        .push("models")
        .push(model_id);
    Ok(url)
}

impl FluxweirClient {
    /// GET /admin/models — 模型能力目录列表（目录，含定价/能力位）。
    ///
    /// 失配修复：旧实现打 `GET /v1/models`——网关**未注册**该
    /// 路由（恒 404），且 `{data:[{id,vendor}]}` 形状无对应实现；现对齐
    /// 目录路由语义。
    pub async fn models(&self) -> Result<Vec<ModelCatalogInfo>, FluxweirSdkError> {
        let url = self
            .base_url
            .join("admin/models")
            .map_err(|e| FluxweirSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            // client 级只有 read_timeout 停滞看护；admin 无请求级 config，
            // 总超时逐请求落到客户端默认，慢滴漏响应不能无上界
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// PUT /admin/models/{model_id} — upsert 目录条目（path 标识
    /// 权威，载荷 model_id 被网关覆写）。
    pub async fn upsert_model(
        &self,
        model_id: &str,
        entry: &ModelCatalogUpsert,
    ) -> Result<(), FluxweirSdkError> {
        let url = admin_model_url(&self.base_url, model_id)?;

        let request = self
            .http
            .put(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .json(entry);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Self::assert_admin_ok(resp).await
    }

    /// DELETE /admin/models/{model_id} — 目录下条（缺失条目网关返回
    /// validation_error 400，映射 [`FluxweirSdkError::Api`]）。
    pub async fn delete_model(&self, model_id: &str) -> Result<(), FluxweirSdkError> {
        let url = admin_model_url(&self.base_url, model_id)?;

        let request = self
            .http
            .delete(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Self::assert_admin_ok(resp).await
    }

    /// GET /admin/vendors — vendor 运行时状态列表（真实形状：状态机词 +
    /// 队列深度 + key 池观测；**非**配置面 {name,enabled}）。
    pub async fn vendors(&self) -> Result<Vec<VendorStatusInfo>, FluxweirSdkError> {
        let url = self
            .base_url
            .join("admin/vendors")
            .map_err(|e| FluxweirSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// GET /admin/sessions — 会话级聚合（成本/token/请求数/首末时间/标题）。
    ///
    /// `[usage_insights].enabled=false` 时网关返回 404，映射
    /// [`FluxweirSdkError::Api`]；`limit` 缺省由服务端取 50、上限 500。
    pub async fn list_sessions(
        &self,
        limit: Option<u32>,
    ) -> Result<SessionsResponse, FluxweirSdkError> {
        let mut url =
            self.base_url
                .join("admin/sessions")
                .map_err(|e| FluxweirSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;
        if let Some(limit) = limit {
            url.query_pairs_mut()
                .append_pair("limit", &limit.to_string());
        }

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// GET /admin/context-stats — 按模型聚合上下文占用率与缓存份额。
    pub async fn context_stats(&self) -> Result<ContextStatsResponse, FluxweirSdkError> {
        let url =
            self.base_url
                .join("admin/context-stats")
                .map_err(|e| FluxweirSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// GET /api/v1/health — 服务健康状态（JSON 健康面在 /api/v1/health，
    /// 非 /admin/health）
    pub async fn health(&self) -> Result<HealthStatus, FluxweirSdkError> {
        let url = self
            .base_url
            .join("api/v1/health")
            .map_err(|e| FluxweirSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// GET /admin/quota/ledger — 配额账本读侧（逐窗 used/重置/作用域）。
    /// `key` 可选过滤租户摘要；网关未接线账本时返回空 rows。
    pub async fn quota_ledger(
        &self,
        key: Option<&str>,
    ) -> Result<QuotaLedgerResponse, FluxweirSdkError> {
        let mut url =
            self.base_url
                .join("admin/quota/ledger")
                .map_err(|e| FluxweirSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;
        if let Some(key) = key {
            url.query_pairs_mut().append_pair("key", key);
        }

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// GET /admin/vendor-budgets — vendor 预算状态（占用/消耗率/重置倒计时）。
    pub async fn vendor_budgets(&self) -> Result<VendorBudgetsResponse, FluxweirSdkError> {
        let url =
            self.base_url
                .join("admin/vendor-budgets")
                .map_err(|e| FluxweirSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let request = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout);
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// POST /admin/catalog/sync — 触发模型目录自动同步（add-only 合并）。
    /// `dry_run=true` 只取 diff 不落盘；409 = 同步进行中、502/413 = 拉取
    /// 失败（映射 [`FluxweirSdkError::Api`]）。
    pub async fn sync_catalog(
        &self,
        dry_run: bool,
    ) -> Result<CatalogSyncResponse, FluxweirSdkError> {
        let url =
            self.base_url
                .join("admin/catalog/sync")
                .map_err(|e| FluxweirSdkError::Config {
                    message: format!("invalid URL: {e}"),
                })?;

        let request = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .json(&serde_json::json!({ "dry_run": dry_run }));
        let request = self.apply_default_headers(request);
        let resp = send_with_retry(&request, &self.retry_policy).await?;

        Ok(resp.json().await?)
    }

    /// admin 写路径的 success 断言（2xx 由重试层
    /// [`send_with_retry`] 保证；`success=false` → Protocol 错误显性）。
    async fn assert_admin_ok(resp: reqwest::Response) -> Result<(), FluxweirSdkError> {
        let body: AdminActionResponse = resp.json().await?;
        if !body.success {
            return Err(FluxweirSdkError::Protocol {
                message: format!("admin action failed: {}", body.message),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod sdk_routes {
    /// SDK admin 面 (method, path) 清单 —— 与网关路由表（
    /// `src/api/router_builder.rs` 的 axum `.route` 注册）比对的单一
    /// 事实源；消费方为 `tests/integration_admin.rs` 的路由 diff 钉。
    /// 仅测试消费（生产路径各方法硬编码自身 URL）。
    pub(crate) const SDK_ADMIN_ROUTES: &[(&str, &str)] = &[
        ("GET", "admin/models"),
        ("PUT", "admin/models/{id}"),
        ("DELETE", "admin/models/{id}"),
        ("GET", "admin/vendors"),
        ("GET", "admin/sessions"),
        ("GET", "admin/context-stats"),
        ("GET", "admin/quota/ledger"),
        ("GET", "admin/vendor-budgets"),
        ("POST", "admin/catalog/sync"),
        ("GET", "api/v1/health"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 审查 MEDIUM：model_id 路径段百分号编码——`../` 类输入无法穿越到
    /// 其他路由（`..%2F` 段由网关 404，而非命中 /admin/config）。
    #[test]
    fn admin_model_url_encodes_traversal_segments() {
        let base = url::Url::parse("http://gw.local/").unwrap();
        let url = admin_model_url(&base, "../admin/config").unwrap();
        assert_eq!(
            url.path(),
            "/admin/models/..%2Fadmin%2Fconfig",
            "段内 / 必须编码为 %2F（穿越 Neutral 化）"
        );
        // 空白与特殊字符同样编码
        let url = admin_model_url(&base, "a b/c?d").unwrap();
        assert_eq!(url.path(), "/admin/models/a%20b%2Fc%3Fd");
        // cannot-be-a-base URL 显性报错
        let mailto = url::Url::parse("mailto:user@example.com").unwrap();
        assert!(admin_model_url(&mailto, "m").is_err());
    }
}
