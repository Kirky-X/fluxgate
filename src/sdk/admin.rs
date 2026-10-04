//! Admin / health 端点客户端方法 — 路径与形状对齐网关真实路由表（T008
//! 失配修复；防回归钉见 `tests/integration_admin.rs` 的路由 diff 测试）。

use serde::Deserialize;

use crate::sdk::client::NexusFlowClient;
use crate::sdk::error::NexusFlowSdkError;

/// 模型能力目录条目 — 对齐网关 `GET /admin/models`（T006 目录）。
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
    /// 工具面（T007）：anthropic 型 vendor 转换出站的缓存断点策略，
    /// 形状对齐 `[[model_catalog.cache_breakpoints]]` 子表（审查 LOW：
    /// 镜像类型化结构体取代裸 Value——upsert 面拼写错误在编译期暴露）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_breakpoints: Option<CacheBreakpointPolicyUpsert>,
}

/// vendor 运行时状态 — 对齐网关 `GET /admin/vendors` 真实形状
///（{id,status,queue_length} + T005 key_pool；旧 {id,name,enabled} 为
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

#[derive(Deserialize)]
struct AdminActionResponse {
    success: bool,
    #[serde(default)]
    message: String,
}

/// 目录条目模型 URL — `model_id` 经 `path_segments_mut().push` 自动
/// 百分号编码（审查 MEDIUM：字符串 join 对 `../` 类输入产生路径穿越，
/// `push` 将段内 `/` 编码为 `%2F`，穿越Neutral 化）。
fn admin_model_url(base_url: &url::Url, model_id: &str) -> Result<url::Url, NexusFlowSdkError> {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .map_err(|_| NexusFlowSdkError::Config {
            message: "base_url cannot be a base".into(),
        })?
        .push("admin")
        .push("models")
        .push(model_id);
    Ok(url)
}

impl NexusFlowClient {
    /// GET /admin/models — 模型能力目录列表（T006 目录，含定价/能力位）。
    ///
    /// 失配修复（T008）：旧实现打 `GET /v1/models`——网关**未注册**该
    /// 路由（恒 404），且 `{data:[{id,vendor}]}` 形状无对应实现；现对齐
    /// 目录路由语义。
    pub async fn models(&self) -> Result<Vec<ModelCatalogInfo>, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("admin/models")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            // client 级只有 read_timeout 停滞看护；admin 无请求级 config，
            // 总超时逐请求落到客户端默认，慢滴漏响应不能无上界
            .timeout(self.effective_timeout)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(resp.json().await?)
    }

    /// PUT /admin/models/{model_id} — upsert 目录条目（T006；path 标识
    /// 权威，载荷 model_id 被网关覆写）。
    pub async fn upsert_model(
        &self,
        model_id: &str,
        entry: &ModelCatalogUpsert,
    ) -> Result<(), NexusFlowSdkError> {
        let url = admin_model_url(&self.base_url, model_id)?;

        let resp = self
            .http
            .put(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .json(entry)
            .send()
            .await?;

        Self::assert_admin_ok(resp).await
    }

    /// DELETE /admin/models/{model_id} — 目录下条（缺失条目网关返回
    /// validation_error 400，映射 [`NexusFlowSdkError::Server`]）。
    pub async fn delete_model(&self, model_id: &str) -> Result<(), NexusFlowSdkError> {
        let url = admin_model_url(&self.base_url, model_id)?;

        let resp = self
            .http
            .delete(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;

        Self::assert_admin_ok(resp).await
    }

    /// GET /admin/vendors — vendor 运行时状态列表（真实形状：状态机词 +
    /// 队列深度 + key 池观测；**非**配置面 {name,enabled}）。
    pub async fn vendors(&self) -> Result<Vec<VendorStatusInfo>, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("admin/vendors")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(resp.json().await?)
    }

    /// GET /api/v1/health — 服务健康状态。
    ///
    /// 失配修复（T008）：旧实现打 `GET /admin/health`——网关不存在该
    /// 路由（`/health` 为 sdforge 明文 "OK"，JSON 健康面在
    /// `/api/v1/health`）；且旧 `HealthStatus.vendors` 字段在线上不存在
    ///（缺字段反序列化必败）。
    pub async fn health(&self) -> Result<HealthStatus, NexusFlowSdkError> {
        let url = self
            .base_url
            .join("api/v1/health")
            .map_err(|e| NexusFlowSdkError::Config {
                message: format!("invalid URL: {e}"),
            })?;

        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(self.effective_timeout)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(resp.json().await?)
    }

    /// admin 写路径的 success 断言（非 2xx → Server 错误；2xx 但
    /// success=false → Protocol 错误显性）。
    async fn assert_admin_ok(resp: reqwest::Response) -> Result<(), NexusFlowSdkError> {
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(NexusFlowSdkError::Server {
                status: status.as_u16(),
                message: text,
            });
        }
        let body: AdminActionResponse = resp.json().await?;
        if !body.success {
            return Err(NexusFlowSdkError::Protocol {
                message: format!("admin action failed: {}", body.message),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod sdk_routes {
    /// SDK admin 面 (method, path) 清单 —— 与网关路由表（
    /// `src/api/router_builder.rs` 的 axum `.route()` 注册）比对的单一
    /// 事实源；消费方为 `tests/integration_admin.rs` 的路由 diff 钉。
    /// 仅测试消费（生产路径各方法硬编码自身 URL）。
    pub(crate) const SDK_ADMIN_ROUTES: &[(&str, &str)] = &[
        ("GET", "admin/models"),
        ("PUT", "admin/models/{id}"),
        ("DELETE", "admin/models/{id}"),
        ("GET", "admin/vendors"),
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
