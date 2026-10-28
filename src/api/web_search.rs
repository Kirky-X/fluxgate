// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 搜索能力注入端点（`web-search` feature）：`POST /v1/search`（多源搜索）
//! 与 `POST /v1/fetch`（netguard 守护抓取）+ `/admin/web-search/stats`。
//!
//! 双门语义：`web-search` feature 编译门 + `[web_search].enabled` 运行时热读
//! 门（缺省 false），门未开时端点一律 404（与 Responses 端点开关同口径）。
//! fetch 走 [`GuardedFetcher`](crate::utils::netguard::GuardedFetcher)：判定
//! 在解析后建连前（pin 防 rebinding 二跳），重定向禁自动跟随。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::api::state::ApiState;
use crate::config::WebSearchProviderConfig;
use crate::error::GateError;
use crate::i18n::{t, t_args};
use crate::utils::netguard::{FetchError, GuardedFetcher};

/// key 池身份 → 轮转状态缓存上限（provider 基数由配置枚举约束，守卫兜底）
const MAX_POOLS: usize = 256;

/// provider 出站超时（provider base_url 是管理员配置的可信面，不挂 netguard
/// 判定——design Alternatives；超时独立于 fetch 的守护约束）
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(15);

/// 近期失败 key 的冷却窗口（5 分钟内选择时跳过，全失败才回头）
pub(crate) const KEY_FAIL_COOLDOWN: Duration = Duration::from_secs(300);

/// /v1/search 单次结果数边界
const SEARCH_N_MIN: u8 = 1;
const SEARCH_N_MAX: u8 = 20;
const SEARCH_N_DEFAULT: u8 = 5;

/// 搜索命中条目（OpenAI 风格响应单元）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 搜索失败（红acted：错误串不含 key 材料——key 只进请求头/体，不进错误路径）
#[derive(Debug)]
pub enum SearchError {
    /// 上游非 2xx / 响应形状非法
    Upstream { message: String },
    /// key 不可用（env 缺失 / vault 未接线或条目不存在）
    KeyUnavailable { message: String },
}

/// 运行时：守护抓取器 + per-provider key 池缓存（热配置按身份续用轮转状态）
/// + 搜索成败计数。
pub struct WebSearchRuntime {
    fetcher: GuardedFetcher,
    client: reqwest::Client,
    pools: Mutex<HashMap<String, Arc<KeyPool>>>,
    stats: Arc<WebSearchStats>,
}

impl std::fmt::Debug for WebSearchRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchRuntime")
            .field("fetcher", &"GuardedFetcher")
            .field("pools", &self.pools.lock().map(|p| p.len()).unwrap_or(0))
            .finish()
    }
}

impl WebSearchRuntime {
    /// 生产构造（10s/1MB 守护约束）。
    pub fn new() -> Self {
        Self::with_fetcher(GuardedFetcher::new())
    }

    /// 注入构造（测试换装 resolver/判定缝/超时）。
    pub fn with_fetcher(fetcher: GuardedFetcher) -> Self {
        Self {
            fetcher,
            client: provider_client(),
            pools: Mutex::new(HashMap::new()),
            stats: Arc::new(WebSearchStats::default()),
        }
    }

    pub(crate) fn fetcher(&self) -> &GuardedFetcher {
        &self.fetcher
    }

    /// provider 共享出站 client。
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// 取（或惰性建）身份对应的 key 池：身份含 kind/base_url/key 源名——
    /// 配置热更后身份变化自然生成新池，同身份续用轮转与冷却状态。
    /// 达容量守卫整体清空（有界内存）。
    pub(crate) fn pool_for(&self, identity: &str, sources: Vec<KeySource>) -> Arc<KeyPool> {
        let mut guard = self
            .pools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.len() >= MAX_POOLS {
            guard.clear();
        }
        guard
            .entry(identity.to_string())
            .or_insert_with(|| Arc::new(KeyPool::new(sources)))
            .clone()
    }

    /// 成败计数快照（admin stats 端点消费）。
    pub(crate) fn stats(&self) -> &WebSearchStats {
        &self.stats
    }
}

#[derive(Deserialize)]
pub struct FetchRequest {
    pub url: String,
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    /// 结果数（1..=20，缺省 5）
    #[serde(default)]
    pub n: Option<u8>,
    /// 指定单源（kind 名，大小写不敏感）；缺省按配置顺序逐源尝试
    #[serde(default)]
    pub provider: Option<String>,
}

/// `POST /v1/search {query, n?, provider?}`：按 `[web_search].providers`
/// 配置顺序逐源尝试至首个成功；`provider` 指定单源；全败 502 聚合各源
/// 错误摘要（脱敏：摘要不含 key 材料——key 只进请求头/体）。
pub async fn search(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<SearchRequest>,
) -> Result<Response, FluxgateErrorResponse> {
    let runtime = gate(&state).map_err(FluxgateErrorResponse)?;
    let query = body.query.trim().to_string();
    if query.is_empty() {
        return Err(GateError::Validation {
            field: "query".to_string(),
        }
        .into());
    }
    let n = body.n.unwrap_or(SEARCH_N_DEFAULT);
    if !(SEARCH_N_MIN..=SEARCH_N_MAX).contains(&n) {
        return Err(GateError::Validation {
            field: "n".to_string(),
        }
        .into());
    }

    let section = state.config.read().web_search.clone();
    #[cfg(feature = "vault")]
    let vault_handle = state.vendor_manager.vault();
    #[cfg(feature = "vault")]
    let vault_ref: Option<&VaultStore> = vault_handle.as_deref();
    #[cfg(not(feature = "vault"))]
    let vault_ref: Option<&VaultStore> = None;

    let metrics = state.vendor_manager.metrics();
    let selected = section.providers.iter().filter(|p| {
        body.provider
            .as_deref()
            .is_none_or(|want| p.kind.eq_ignore_ascii_case(want))
    });
    let mut summary: Vec<String> = Vec::new();
    for entry in selected {
        let kind = entry.kind.as_str();
        let provider = match build_provider(entry, runtime.client()) {
            Ok(p) => p,
            Err(e) => {
                summary.push(format!("{kind}: {}", e.user_message()));
                continue;
            }
        };
        let sources = key_sources_of(entry);
        let identity = format!(
            "{kind}\u{0}{}\u{0}{}",
            entry.base_url,
            sources
                .iter()
                .map(|s| match s {
                    KeySource::Env(name) => format!("env:{name}"),
                    KeySource::VaultRef(id) => format!("vault:{id}"),
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        let pool = runtime.pool_for(&identity, sources);
        let now = Instant::now();
        // 无 key 源（如 SearXNG）：单次直接调用，不走池轮转
        let attempts = pool.len();
        if attempts == 0 {
            match provider.search(&query, n, "").await {
                Ok(hits) => {
                    runtime.stats().record(kind, 0, true);
                    metrics.record_web_search_request(kind, true);
                    return Ok(search_success(hits, kind));
                }
                Err(e) => {
                    runtime.stats().record(kind, 0, false);
                    summary.push(format!("{kind}: {}", e.message()));
                }
            }
        }
        for _ in 0..attempts {
            let (idx, key) = match pool.acquire(now, vault_ref).await {
                Ok(pair) => pair,
                Err(e) => {
                    summary.push(format!("{kind}: {}", e.message()));
                    break;
                }
            };
            match provider.search(&query, n, &key).await {
                Ok(hits) => {
                    pool.report_success(idx);
                    runtime.stats().record(kind, idx, true);
                    metrics.record_web_search_request(kind, true);
                    return Ok(search_success(hits, kind));
                }
                Err(e) => {
                    pool.report_failure(idx, now);
                    runtime.stats().record(kind, idx, false);
                    summary.push(format!("{kind}: {}", e.message()));
                }
            }
        }
        metrics.record_web_search_request(kind, false);
    }
    if summary.is_empty() && section.providers.is_empty() {
        return Ok((
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": {
                    "message": t("search-no-providers"),
                    "type": "search_no_providers",
                }
            })),
        )
            .into_response());
    }
    metrics.record_web_search_request("search", false);
    let digest = summary.join("; ");
    Ok((
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": t_args("search-all-providers-failed", &[("summary", digest)]),
                "type": "search_all_providers_failed",
            }
        })),
    )
        .into_response())
}

/// 成功响应统一形态（OpenAI 风格 list）。
fn search_success(hits: Vec<Hit>, kind: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "object": "list",
            "provider": kind,
            "count": hits.len(),
            "data": hits,
        })),
    )
        .into_response()
}

/// `GET /admin/web-search/stats`：per (provider, key_idx) 成败计数。
/// 脱敏面：只回 key_idx 序号，零 key 材料（明文/env 名/vault id 不出面）。
pub async fn admin_stats(
    State(state): State<Arc<ApiState>>,
) -> Result<Response, FluxgateErrorResponse> {
    let runtime = gate(&state).map_err(FluxgateErrorResponse)?;
    let mut providers: Vec<serde_json::Value> = Vec::new();
    let mut by_provider: BTreeMap<String, Vec<(usize, u64, u64)>> = BTreeMap::new();
    for ((provider, key_idx), (ok, fail)) in runtime.stats().snapshot() {
        by_provider
            .entry(provider)
            .or_default()
            .push((key_idx, ok, fail));
    }
    for (provider, mut keys) in by_provider {
        keys.sort_by_key(|(idx, _, _)| *idx);
        providers.push(json!({
            "provider": provider,
            "keys": keys
                .into_iter()
                .map(|(idx, ok, fail)| json!({
                    "key_idx": idx,
                    "success": ok,
                    "failure": fail,
                }))
                .collect::<Vec<_>>(),
        }));
    }
    Ok((StatusCode::OK, Json(json!({ "providers": providers }))).into_response())
}

impl SearchError {
    /// 脱敏摘要（不含 key 材料：key 只进请求头/体，不进错误路径）。
    pub(crate) fn message(&self) -> String {
        match self {
            SearchError::Upstream { message } => message.clone(),
            SearchError::KeyUnavailable { message } => message.clone(),
        }
    }
}

/// 双门判定：enabled 运行时门 + 运行时装配门，未开一律 404 响应。
fn gate(state: &ApiState) -> Result<&WebSearchRuntime, Response> {
    if !state.config.read().web_search.enabled {
        return Err(web_search_disabled());
    }
    state.web_search.as_deref().ok_or_else(web_search_disabled)
}

fn web_search_disabled() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "message": t("web-search-disabled"),
                "type": "web_search_disabled",
            }
        })),
    )
        .into_response()
}

/// `POST /v1/fetch {url}`：netguard 守护抓取，返回正文文本。
///
/// 映射：2xx → 200 正文 JSON；3xx → 原状态码 + Location（禁跟随）；
/// 上游其余状态 → 502；Blocked → 400 `netguard_blocked`（仅段名 tag，不回显
/// 地址）；TooLarge → 413；UnsupportedMedia → 415；Timeout → 504；
/// URL 非法 → 400 validation。
pub async fn fetch(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<FetchRequest>,
) -> Result<Response, FluxgateErrorResponse> {
    let runtime = gate(&state).map_err(FluxgateErrorResponse)?;
    let url = body.url.trim().to_string();
    if url.is_empty() {
        return Err(GateError::Validation {
            field: "url".to_string(),
        }
        .into());
    }
    let metrics = state.vendor_manager.metrics();
    match runtime.fetcher().fetch(&url).await {
        Ok(page) if (300..400).contains(&page.status) => {
            let mut response = (
                StatusCode::from_u16(page.status).map_err(|_| GateError::Routing {
                    message: "bad upstream status".to_string(),
                })?,
                Json(json!({
                    "status": page.status,
                    "location": page.location,
                })),
            )
                .into_response();
            if let Some(loc) = &page.location {
                if let Ok(value) = loc.parse() {
                    response.headers_mut().insert(header::LOCATION, value);
                }
            }
            Ok(response)
        }
        Ok(page) if (200..300).contains(&page.status) => Ok((
            StatusCode::OK,
            Json(json!({
                "url": url,
                "status": page.status,
                "content_type": page.content_type,
                "text": page.text,
            })),
        )
            .into_response()),
        Ok(page) => Err(GateError::Protocol {
            source: format!("upstream status {}", page.status),
            inner: None,
        }
        .into()),
        Err(FetchError::Blocked { tag }) => {
            metrics.record_netguard_denied(tag);
            Ok((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "message": t_args("netguard-blocked", &[("tag", tag.to_string())]),
                        "type": "netguard_blocked",
                    }
                })),
            )
                .into_response())
        }
        Err(FetchError::InvalidUrl) => Err(GateError::Validation {
            field: "url".to_string(),
        }
        .into()),
        Err(FetchError::Timeout) => Err(GateError::Timeout {
            vendor: "fetch".to_string(),
        }
        .into()),
        Err(FetchError::TooLarge) => Ok((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "error": {
                    "message": t("fetch-body-too-large"),
                    "type": "fetch_body_too_large",
                }
            })),
        )
            .into_response()),
        Err(FetchError::UnsupportedMedia) => Ok((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({
                "error": {
                    "message": t("fetch-unsupported-media"),
                    "type": "fetch_unsupported_media",
                }
            })),
        )
            .into_response()),
        Err(FetchError::Transport { message }) => Err(GateError::Protocol {
            source: message,
            inner: None,
        }
        .into()),
    }
}

/// handler 双返回形态的桥：404 门响应（手造）与 GateError（IntoResponse 面）
/// 统一为 Response。
pub struct FluxgateErrorResponse(Response);

impl From<GateError> for FluxgateErrorResponse {
    fn from(e: GateError) -> Self {
        Self(crate::error::FluxgateError::from(e).into_response())
    }
}

impl From<crate::error::FluxgateError> for FluxgateErrorResponse {
    fn from(e: crate::error::FluxgateError) -> Self {
        Self(e.into_response())
    }
}

impl From<Response> for FluxgateErrorResponse {
    fn from(r: Response) -> Self {
        Self(r)
    }
}

impl axum::response::IntoResponse for FluxgateErrorResponse {
    fn into_response(self) -> Response {
        self.0
    }
}

// ---------- provider 抽象（R-search-001） ----------

/// 搜索源抽象：单次尝试（key 由 KeyPool 轮转供给），响应形状各源自适应。
#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, n: u8, api_key: &str) -> Result<Vec<Hit>, SearchError>;
}

/// provider 共享出站 client（15s 超时）。
pub(crate) fn provider_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(PROVIDER_TIMEOUT)
        .build()
        .expect("provider http client")
}

/// 各源官方 API base（配置 base_url 缺省回落；searxng 无缺省——实例能力
/// 由部署方保证，必须显式配置）
const DEFAULT_TAVILY_BASE: &str = "https://api.tavily.com";
const DEFAULT_BRAVE_BASE: &str = "https://api.search.brave.com";
const DEFAULT_EXA_BASE: &str = "https://api.exa.ai";

/// 按配置构建 provider 实例（kind 闭集外的值由加载校验拒绝，此处兜底拒绝）。
pub(crate) fn build_provider(
    cfg: &WebSearchProviderConfig,
    client: &reqwest::Client,
) -> Result<Arc<dyn SearchProvider>, GateError> {
    let base = |default: &str| {
        if cfg.base_url.trim().is_empty() {
            default.to_string()
        } else {
            cfg.base_url.trim_end_matches('/').to_string()
        }
    };
    Ok(match cfg.kind.as_str() {
        "tavily" => Arc::new(TavilyProvider::new(
            &base(DEFAULT_TAVILY_BASE),
            client.clone(),
        )),
        "brave" => Arc::new(BraveProvider::new(
            &base(DEFAULT_BRAVE_BASE),
            client.clone(),
        )),
        "exa" => Arc::new(ExaProvider::new(&base(DEFAULT_EXA_BASE), client.clone())),
        "searxng" => {
            if cfg.base_url.trim().is_empty() {
                return Err(GateError::Config {
                    message: t("config-web-search-missing-base-url"),
                });
            }
            Arc::new(SearXNGProvider::new(&base(""), client.clone()))
        }
        other => {
            return Err(GateError::Config {
                message: t_args(
                    "config-web-search-unknown-kind",
                    &[("kind", other.to_string())],
                ),
            })
        }
    })
}

/// 上游 JSON 取字段的最小容错路径：字段缺失/类型错一律归 Upstream（响应
/// 形状非法），不 panic。
fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}

fn parse_hits(v: &serde_json::Value, container: &[&str], snippet_key: &str) -> Vec<Hit> {
    let items = match container {
        [] => v,
        path => {
            let mut cur = v;
            for key in path {
                cur = cur.get(key).unwrap_or(&serde_json::Value::Null);
            }
            cur
        }
    };
    items
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    Some(Hit {
                        title: str_field(item, "title")?,
                        url: str_field(item, "url")?,
                        snippet: str_field(item, snippet_key).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tavily：POST {base}/search，api_key 在 body（query/max_results）。
pub struct TavilyProvider {
    base: String,
    client: reqwest::Client,
}

impl TavilyProvider {
    pub fn new(base: &str, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for TavilyProvider {
    async fn search(&self, query: &str, n: u8, api_key: &str) -> Result<Vec<Hit>, SearchError> {
        let resp = self
            .client
            .post(format!("{}/search", self.base))
            .json(&json!({
                "api_key": api_key,
                "query": query,
                "max_results": n,
            }))
            .send()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        if !resp.status().is_success() {
            return Err(SearchError::Upstream {
                message: format!("status {}", resp.status()),
            });
        }
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        Ok(parse_hits(&value, &[], "content"))
    }
}

/// Brave Search：GET {base}/res/web/search，X-Subscription-Token 头。
pub struct BraveProvider {
    base: String,
    client: reqwest::Client,
}

impl BraveProvider {
    pub fn new(base: &str, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for BraveProvider {
    async fn search(&self, query: &str, n: u8, api_key: &str) -> Result<Vec<Hit>, SearchError> {
        let mut url =
            reqwest::Url::parse(&format!("{}/res/web/search", self.base)).map_err(|e| {
                SearchError::Upstream {
                    message: e.to_string(),
                }
            })?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &n.to_string());
        let resp = self
            .client
            .get(url)
            .header("X-Subscription-Token", api_key)
            .send()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        if !resp.status().is_success() {
            return Err(SearchError::Upstream {
                message: format!("status {}", resp.status()),
            });
        }
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        Ok(parse_hits(&value, &["web"], "description"))
    }
}

/// Exa：POST {base}/search，x-api-key 头（query/numResults）。
pub struct ExaProvider {
    base: String,
    client: reqwest::Client,
}

impl ExaProvider {
    pub fn new(base: &str, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for ExaProvider {
    async fn search(&self, query: &str, n: u8, api_key: &str) -> Result<Vec<Hit>, SearchError> {
        let resp = self
            .client
            .post(format!("{}/search", self.base))
            .header("x-api-key", api_key)
            .json(&json!({
                "query": query,
                "numResults": n,
            }))
            .send()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        if !resp.status().is_success() {
            return Err(SearchError::Upstream {
                message: format!("status {}", resp.status()),
            });
        }
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        Ok(parse_hits(&value, &[], "text"))
    }
}

/// SearXNG：GET {base}/search?q&format=json（无鉴权头；实例禁 JSON API 时
/// 由部署方保证能力——design 技术债披露项）。
pub struct SearXNGProvider {
    base: String,
    client: reqwest::Client,
}

impl SearXNGProvider {
    pub fn new(base: &str, client: reqwest::Client) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for SearXNGProvider {
    async fn search(&self, query: &str, n: u8, _api_key: &str) -> Result<Vec<Hit>, SearchError> {
        let _ = n;
        let mut url = reqwest::Url::parse(&format!("{}/search", self.base)).map_err(|e| {
            SearchError::Upstream {
                message: e.to_string(),
            }
        })?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("format", "json");
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        if !resp.status().is_success() {
            return Err(SearchError::Upstream {
                message: format!("status {}", resp.status()),
            });
        }
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SearchError::Upstream {
                message: e.to_string(),
            })?;
        Ok(parse_hits(&value, &[], "content"))
    }
}

// ---------- key 池（R-search-002） ----------

/// key 来源：env 变量名或 vault 条目引用（明文不落配置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeySource {
    Env(String),
    VaultRef(String),
}

impl KeySource {
    pub(crate) fn env(name: &str) -> Self {
        Self::Env(name.to_string())
    }
}

/// vault 类型别名（vault feature 关闭时为不可达占位：VaultRef 源解析必败）。
#[cfg(feature = "vault")]
pub(crate) type VaultStore = dyn crate::vault::KeyStore;
#[cfg(not(feature = "vault"))]
pub(crate) struct VaultStore {
    _priv: (),
}

/// 单 key 状态：来源 + 近期失败时刻（冷却窗口内选择时跳过）。
struct KeyState {
    source: KeySource,
    last_fail: Option<Instant>,
}

/// provider 多 key 池：轮转指针 + 近 5 分钟失败 key 靠后（全失败才回头）。
/// 内部 std Mutex（临界区无 await：源列表快照后锁外解析，跨 await 不持锁）。
pub(crate) struct KeyPool {
    inner: Mutex<PoolInner>,
}

struct PoolInner {
    keys: Vec<KeyState>,
    cursor: usize,
}

impl KeyPool {
    pub(crate) fn new(sources: Vec<KeySource>) -> Self {
        Self {
            inner: Mutex::new(PoolInner {
                keys: sources
                    .into_iter()
                    .map(|source| KeyState {
                        source,
                        last_fail: None,
                    })
                    .collect(),
                cursor: 0,
            }),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys
            .len()
    }

    /// 轮转取 key：第一轮跳过冷却期内的 key，全被跳过则回头兜底（忽略
    /// 冷却）；key 解析失败（env 缺失 / vault 不可用）计入尝试并继续下一。
    /// 成功后推进轮转指针。并发获取允许同 key 撞车（轮转是尽力公平，非配额）。
    pub(crate) async fn acquire(
        &self,
        now: Instant,
        vault: Option<&VaultStore>,
    ) -> Result<(usize, String), SearchError> {
        let (sources, in_cooldown): (Vec<KeySource>, Vec<bool>) = {
            let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if guard.keys.is_empty() {
                return Err(SearchError::KeyUnavailable {
                    message: "empty key pool".to_string(),
                });
            }
            (
                guard.keys.iter().map(|k| k.source.clone()).collect(),
                guard
                    .keys
                    .iter()
                    .map(|k| {
                        k.last_fail
                            .is_some_and(|t| now.duration_since(t) < KEY_FAIL_COOLDOWN)
                    })
                    .collect(),
            )
        };
        let n = sources.len();
        let start = {
            let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let start = guard.cursor % n;
            guard.cursor = (start + 1) % n;
            start
        };
        let mut first_err: Option<SearchError> = None;
        for pass in 0..2 {
            for step in 0..n {
                let idx = (start + step) % n;
                if pass == 0 && in_cooldown[idx] {
                    continue;
                }
                match resolve_key(&sources[idx], vault).await {
                    Ok(key) => return Ok((idx, key)),
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
        }
        Err(first_err.unwrap_or(SearchError::KeyUnavailable {
            message: "no usable keys".to_string(),
        }))
    }

    pub(crate) fn report_success(&self, idx: usize) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(state) = guard.keys.get_mut(idx) {
            state.last_fail = None;
        }
    }

    pub(crate) fn report_failure(&self, idx: usize, now: Instant) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(state) = guard.keys.get_mut(idx) {
            state.last_fail = Some(now);
        }
    }
}

/// key 明文解析（env 读变量 / vault reveal 即用即弃）。
async fn resolve_key(
    source: &KeySource,
    vault: Option<&VaultStore>,
) -> Result<String, SearchError> {
    match source {
        KeySource::Env(name) => std::env::var(name).map_err(|_| SearchError::KeyUnavailable {
            message: format!("env {name} not set"),
        }),
        KeySource::VaultRef(id) => {
            #[cfg(feature = "vault")]
            match vault {
                Some(store) => {
                    use secrecy::ExposeSecret as _;
                    let secret =
                        store
                            .reveal(id)
                            .await
                            .map_err(|e| SearchError::KeyUnavailable {
                                message: e.to_string(),
                            })?;
                    Ok(secret.expose_secret().to_string())
                }
                None => Err(SearchError::KeyUnavailable {
                    message: "vault not wired".to_string(),
                }),
            }
            #[cfg(not(feature = "vault"))]
            {
                let _ = (id, vault);
                Err(SearchError::KeyUnavailable {
                    message: "vault feature disabled".to_string(),
                })
            }
        }
    }
}

/// 配置条目 → key 源列表（vault 引用优先于 env，envs 列表在前、主 env 收尾）。
pub(crate) fn key_sources_of(cfg: &WebSearchProviderConfig) -> Vec<KeySource> {
    let mut sources = Vec::new();
    if !cfg.api_key_vault_ref.trim().is_empty() {
        sources.push(KeySource::VaultRef(
            cfg.api_key_vault_ref.trim().to_string(),
        ));
    }
    for name in &cfg.api_key_envs {
        if !name.trim().is_empty() {
            sources.push(KeySource::Env(name.trim().to_string()));
        }
    }
    if !cfg.api_key_env.trim().is_empty() {
        sources.push(KeySource::Env(cfg.api_key_env.trim().to_string()));
    }
    sources
}

/// per (provider, key_idx) 成败计数（admin stats 消费；零 key 材料落面）。
#[derive(Default)]
pub(crate) struct WebSearchStats {
    inner: Mutex<BTreeMap<(String, usize), (u64, u64)>>,
}

impl WebSearchStats {
    pub(crate) fn record(&self, provider: &str, key_idx: usize, success: bool) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let entry = guard
            .entry((provider.to_string(), key_idx))
            .or_insert((0, 0));
        if success {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }

    /// 快照：(provider, key_idx) → (成功数, 失败数)，按键排序稳定。
    pub(crate) fn snapshot(&self) -> BTreeMap<(String, usize), (u64, u64)> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(all(test, feature = "web-search"))]
mod provider_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn test_client() -> reqwest::Client {
        provider_client()
    }

    /// R-search-001：四 provider 请求形态与 Hit 解析（mock 端点断言）。
    #[tokio::test]
    async fn tavily_brave_exa_searxng_request_shapes_and_parse() {
        let mut server = mockito::Server::new_async().await;
        let want = vec![Hit {
            title: "T".into(),
            url: "https://a.example/1".into(),
            snippet: "C".into(),
        }];

        // Tavily：POST {base}/search，api_key 在 body，max_results=n，content→snippet
        let m = server
            .mock("POST", "/tavily/search")
            .match_header("Content-Type", "application/json")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "api_key": "tv-key",
                "query": "rust ssrf",
                "max_results": 2
            })))
            .with_header("Content-Type", "application/json")
            .with_body(r#"{"results":[{"title":"T","url":"https://a.example/1","content":"C"}]}"#)
            .create_async()
            .await;
        let p = TavilyProvider::new(&format!("{}/tavily", server.url()), test_client());
        let hits = p.search("rust ssrf", 2, "tv-key").await.unwrap();
        assert_eq!(hits, want, "tavily 解析形态");
        m.assert();

        // Brave：GET {base}/res/web/search?q&count，X-Subscription-Token 头，description→snippet
        let m = server
            .mock("GET", "/brave/res/web/search")
            .match_query(mockito::Matcher::AllOf(vec![
                    mockito::Matcher::UrlEncoded("q".into(), "rust ssrf".into()),
                    mockito::Matcher::UrlEncoded("count".into(), "2".into()),
                ]))
            .match_header("X-Subscription-Token", "br-key")
            .with_header("Content-Type", "application/json")
            .with_body(r#"{"web":{"results":[{"title":"T","url":"https://a.example/1","description":"C"}]}}"#)
            .create_async()
            .await;
        let p = BraveProvider::new(&format!("{}/brave", server.url()), test_client());
        let hits = p.search("rust ssrf", 2, "br-key").await.unwrap();
        assert_eq!(hits, want, "brave 解析形态");
        m.assert();

        // Exa：POST {base}/search JSON，x-api-key 头，numResults=n，text→snippet
        let m = server
            .mock("POST", "/exa/search")
            .match_header("x-api-key", "ex-key")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "query": "rust ssrf",
                "numResults": 2
            })))
            .with_header("Content-Type", "application/json")
            .with_body(r#"{"results":[{"title":"T","url":"https://a.example/1","text":"C"}]}"#)
            .create_async()
            .await;
        let p = ExaProvider::new(&format!("{}/exa", server.url()), test_client());
        let hits = p.search("rust ssrf", 2, "ex-key").await.unwrap();
        assert_eq!(hits, want, "exa 解析形态");
        m.assert();

        // SearXNG：GET {base}/search?q&format=json，无鉴权头，content→snippet
        let m = server
            .mock("GET", "/searxng/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "rust ssrf".into()),
                mockito::Matcher::UrlEncoded("format".into(), "json".into()),
            ]))
            .with_header("Content-Type", "application/json")
            .with_body(r#"{"results":[{"title":"T","url":"https://a.example/1","content":"C"}]}"#)
            .create_async()
            .await;
        let p = SearXNGProvider::new(&format!("{}/searxng", server.url()), test_client());
        let hits = p.search("rust ssrf", 2, "").await.unwrap();
        assert_eq!(hits, want, "searxng 解析形态");
        m.assert();

        // 非 2xx → Upstream 错误
        let m = server
            .mock("POST", "/tavily/search")
            .with_status(503)
            .create_async()
            .await;
        let p = TavilyProvider::new(&format!("{}/tavily", server.url()), test_client());
        let err = p.search("q", 1, "k").await.unwrap_err();
        assert!(matches!(err, SearchError::Upstream { .. }), "{err:?}");
        m.assert();
    }

    /// R-search-002：轮转推进、近 5 分钟失败 key 跳过、全失败回头兜底、stats 计数。
    #[tokio::test]
    async fn rotation_advances_and_recently_failed_key_skipped() {
        std::env::set_var("WEB_SEARCH_POOL_KEY_A", "ka");
        std::env::set_var("WEB_SEARCH_POOL_KEY_B", "kb");
        let pool = KeyPool::new(vec![
            KeySource::env("WEB_SEARCH_POOL_KEY_A"),
            KeySource::env("WEB_SEARCH_POOL_KEY_B"),
        ]);
        let now = std::time::Instant::now();

        // 轮转推进：两次 acquire 落在不同 key
        let (i, k) = pool.acquire(now, None).await.unwrap();
        assert_eq!((i, k.as_str()), (0, "ka"));
        let (i, k) = pool.acquire(now, None).await.unwrap();
        assert_eq!((i, k.as_str()), (1, "kb"));

        // key0 失败 → 冷却期内选择跳过 key0
        pool.report_failure(0, now);
        let (i, k) = pool.acquire(now, None).await.unwrap();
        assert_eq!((i, k.as_str()), (1, "kb"), "冷却期内的 key 必须被跳过");

        // 全失败 → 回头兜底可用（忽略冷却；从轮转指针起首个可解析 key）
        pool.report_failure(1, now);
        let (i, k) = pool.acquire(now, None).await.unwrap();
        assert_eq!((i, k.as_str()), (1, "kb"), "全失败必须回头兜底（指针位起）");

        // 冷却期过后恢复正常轮转
        pool.report_success(0);
        pool.report_success(1);
        let later = now + KEY_FAIL_COOLDOWN + Duration::from_secs(1);
        let (i, k) = pool.acquire(later, None).await.unwrap();
        assert_eq!((i, k.as_str()), (0, "ka"));

        // 失败计数入 stats
        let stats = WebSearchStats::default();
        stats.record("tavily", 0, true);
        stats.record("tavily", 0, false);
        stats.record("brave", 3, false);
        let mut want = BTreeMap::new();
        want.insert(("tavily".to_string(), 0usize), (1u64, 1u64));
        want.insert(("brave".to_string(), 3usize), (0u64, 1u64));
        assert_eq!(stats.snapshot(), want);
    }
}
