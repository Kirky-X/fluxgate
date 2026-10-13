// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod loader;
pub mod manager;
pub mod model_catalog;

pub use loader::{parse_config_content, validate_config};
pub use manager::ConfigManager;
pub use model_catalog::{
    capabilities_for, flavor_default_capabilities, ModelCapabilities, ModelCatalogEntry,
    PricingCurrency,
};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "0.0.0.0".into()
}

fn default_port() -> u16 {
    8080
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
    /// 完整数据库 URL（postgres://、mysql://、sqlite://…），非空时优先于
    /// path；空值 = 旧语义，path 按 sqlite 文件库回退
    #[serde(default)]
    pub url: String,
}

fn default_db_path() -> String {
    "./data/llm_proxy.db".into()
}

fn default_pool_size() -> u32 {
    10
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct VendorConfig {
    pub id: String,
    pub name: String,
    pub type_: String,
    pub api_key_env: String,
    /// 多 key 池的显式 env 名列表，与 api_key_env 合并去重（本列表在前）；
    /// 缺名 env 跳过并告警，池上限见 key_pool::MAX_KEY_POOL。
    /// 只存环境变量名，绝不存 key 原文
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_key_envs: Vec<String>,
    /// 出站自定义头（静态值，热生效）：请求构造处统一注入，
    /// 与网关已设置的头（鉴权与协议头）冲突时跳过该项并记 warn
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_headers: HashMap<String, String>,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_vendor_enabled")]
    pub enabled: bool,
    /// fallback 优先级：数值越小优先级越高（1 = primary）
    #[serde(default = "default_vendor_priority")]
    pub priority: u8,
    #[serde(default = "Vec::new")]
    pub models: Vec<ModelConfig>,
    pub rate_limit: RateLimitConfig,
    pub concurrency: ConcurrencyConfig,
    /// 出站 HTTP 总超时（毫秒），自 send 起算至响应体读完；0 = 不限时
    #[serde(default = "default_vendor_timeout_ms")]
    pub timeout_ms: u64,
    /// 流式 TTFT（Time-To-First-Token）超时（毫秒），包住 send 至上游
    /// 响应头返回；0 = 不限时
    #[serde(default = "default_stream_ttft_ms")]
    pub stream_ttft_ms: u64,
    /// 流式 chunk 间 idle 超时（毫秒），超时以 Timeout 终止流；0 = 不限时
    #[serde(default = "default_stream_idle_ms")]
    pub stream_idle_ms: u64,
}

fn default_vendor_priority() -> u8 {
    1
}

fn default_vendor_timeout_ms() -> u64 {
    60_000
}

fn default_stream_ttft_ms() -> u64 {
    60_000
}

fn default_stream_idle_ms() -> u64 {
    30_000
}

fn default_api_base() -> String {
    String::new()
}

fn default_vendor_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ModelConfig {
    pub model_id: String,
    #[serde(default)]
    pub abstract_id: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default = "default_model_enabled")]
    pub enabled: bool,
}

fn default_weight() -> f64 {
    1.0
}

fn default_model_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum RateLimitConfig {
    #[serde(rename = "time_window")]
    TimeWindow { start: String, end: String },
    #[serde(rename = "interval")]
    Interval {
        hours: u64,
        /// 滑动窗口开关：默认 false = 固定窗口；true = 分片滑动窗口
        ///（消除固定窗口翻越点的瞬时 2× 配额双花）
        #[serde(default)]
        sliding: bool,
    },
    #[serde(rename = "token")]
    Token {
        max_tokens: u64,
        /// Token 配额窗口时长（小时），缺省 24h
        #[serde(default = "default_token_window_hours")]
        window_hours: Option<u64>,
        /// 滑动窗口开关（语义同 Interval.sliding）。
        #[serde(default)]
        sliding: bool,
    },
}

fn default_token_window_hours() -> Option<u64> {
    Some(24)
}

#[derive(Default, Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ConcurrencyConfig {
    #[serde(default = "default_rps")]
    pub requests_per_second: u32,
    /// 最大并发在途请求数（含流式）；缺省 None = 队列容量（100）
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,
    /// token 加权并发票据开关：开启时按预估 token 量计费
    ///（agentic 长会话不再与短请求等价占票）；构造期消费，restart-required
    #[serde(default)]
    pub token_weighted: bool,
    /// 单请求票据下限（默认 1）
    #[serde(default = "default_token_ticket_min")]
    pub token_ticket_min: u64,
    /// 单请求票据上限（逃逸阀，防预算被单个请求耗尽）
    #[serde(default = "default_token_ticket_max")]
    pub token_ticket_max: u64,
    /// max_tokens 缺省兜底（请求未声明时按该值计费）
    #[serde(default = "default_token_ticket_default")]
    pub token_ticket_default: u64,
}

fn default_token_ticket_min() -> u64 {
    1
}

fn default_token_ticket_max() -> u64 {
    16384
}

fn default_token_ticket_default() -> u64 {
    4096
}

fn default_rps() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GlobalModelConfig {
    #[serde(default = "default_global_enabled")]
    pub enabled: bool,
    #[serde(default = "default_strategy")]
    pub strategy: String,
}

fn default_global_enabled() -> bool {
    true
}

fn default_strategy() -> String {
    "weighted".into()
}

/// API key 档位（拥塞准入优先级基准）；serde 严格枚举，未知档位使配置加载失败
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum AuthTier {
    #[serde(rename = "dedicated")]
    Dedicated,
    #[serde(rename = "guaranteed")]
    Guaranteed,
    /// 缺省档（未在 `[auth]`.keys 声明的 key 全部落此档——行为不变）
    #[default]
    #[serde(rename = "elastic")]
    Elastic,
    #[serde(rename = "spot")]
    Spot,
}

/// [[auth.keys]] 单条：key 摘要（SHA256 hex，明文不落盘）→ 档位 + 配额；
/// 未在 keys 声明的 key 缺省 [`AuthTier::Elastic`]
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AuthKeyEntry {
    pub hash: String,
    #[serde(default)]
    pub tier: AuthTier,
    /// token 窗口限额（input+output 合计；0 = 不限）。
    #[serde(default)]
    pub token_quota: u64,
    /// 成本窗口限额（millicents 微美分整数，1 单位 = 1e-5 USD；0 = 不限）。
    #[serde(default)]
    pub cost_quota_millicents: u64,
}

/// `[auth]` 段：api_keys 存 SHA256 hex 摘要（认证时对请求 key 现场算摘要
/// 做常量时间比较，明文密钥不落盘）；keys 子表声明 key 档位，
/// 缺省全 elastic 且准入不启用。热读：认证与准入每请求现读
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AuthSection {
    #[serde(default = "default_auth_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub header: AuthHeaderSection,
    /// key 档位表（可选；缺省全 elastic、准入不启用）
    #[serde(default)]
    pub keys: Vec<AuthKeyEntry>,
}

fn default_auth_enabled() -> bool {
    true
}

/// [auth.header] 子段：请求头名与前缀（保真加载防回写抹段）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AuthHeaderSection {
    #[serde(default = "default_auth_header_name")]
    pub name: String,
    #[serde(default = "default_auth_header_prefix")]
    pub prefix: String,
}

fn default_auth_header_name() -> String {
    "Authorization".into()
}

fn default_auth_header_prefix() -> String {
    "Bearer".into()
}

impl Default for AuthHeaderSection {
    fn default() -> Self {
        Self {
            name: default_auth_header_name(),
            prefix: default_auth_header_prefix(),
        }
    }
}

/// `[cors]` 段 — CORS 配置（serde 全 default，未配置段时 Option 为 None）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CorsSection {
    #[serde(default = "default_cors_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub allowed_methods: Vec<String>,
    #[serde(default)]
    pub allowed_headers: Vec<String>,
    #[serde(default)]
    pub allow_credentials: bool,
    #[serde(default)]
    pub max_age: u64,
}

fn default_cors_enabled() -> bool {
    true
}

impl Default for CorsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_origins: vec!["*".to_string()],
            allowed_methods: vec!["GET".to_string(), "POST".to_string(), "OPTIONS".to_string()],
            allowed_headers: vec!["*".to_string()],
            allow_credentials: false,
            max_age: 86400,
        }
    }
}

/// `[retry]` 段：出站重试策略，热读（POST /admin/config 更新后立即生效）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RetrySection {
    /// 单 vendor 内最大重试次数（不含首次调用）
    #[serde(default = "default_retry_max_retries")]
    pub max_retries: u32,
    /// 触发重试的 HTTP 状态码集合（429 走指数退避，5xx 走固定间隔）
    #[serde(default = "default_retryable_status_codes")]
    pub retryable_status_codes: Vec<u16>,
    /// 首次重试前延迟（毫秒）
    #[serde(default = "default_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    /// 退避乘数
    #[serde(default = "default_retry_factor")]
    pub factor: f64,
    /// 单次等待上限（毫秒）
    #[serde(default = "default_retry_max_delay_ms")]
    pub max_delay_ms: u64,
    /// 退避抖动比例（0.0–1.0）
    #[serde(default = "default_retry_jitter")]
    pub jitter: f64,
    /// 同步惊群展宽比例：延迟 = base + U(0, spread_jitter) × base
    ///（消除多实例同刻重试）；0.0 = 回滚阀（Retry-After 原样、5xx 固定 1s）
    #[serde(default = "default_retry_spread_jitter")]
    pub spread_jitter: f64,
    /// 跨 vendor 总尝试预算（换路 fallback 的 attempts 上限，防
    /// vendor 数 × 重试数爆炸）
    #[serde(default = "default_retry_total_attempts")]
    pub max_total_attempts: u32,
    /// 单请求重试总时间预算（秒）：默认 0 = 不限时；>0 时重试循环每次
    /// 等待前检查预算，超限返回最后错误
    #[serde(default)]
    pub retry_budget_secs: u64,
    /// 重试放大守门器：开启时失败率逼近临界（f > 0.8·f_c）则本请求
    /// max_total_attempts 收紧到 1 并告警；r·f > 0.95 发散告警。
    /// 默认 false 只靠 metrics 计数
    #[serde(default)]
    pub amplification_gate: bool,
}

fn default_retry_max_retries() -> u32 {
    3
}

fn default_retryable_status_codes() -> Vec<u16> {
    vec![429, 500, 502, 503]
}

fn default_retry_initial_delay_ms() -> u64 {
    1000
}

fn default_retry_factor() -> f64 {
    2.0
}

fn default_retry_max_delay_ms() -> u64 {
    16000
}

fn default_retry_jitter() -> f64 {
    0.2
}

fn default_retry_spread_jitter() -> f64 {
    1.0
}

fn default_retry_total_attempts() -> u32 {
    8
}

/// `[routing]` 段：路由权重系数与选择算法，热读（POST /admin/config 立即生效）。
/// 权重 = 五因子加权和，最终权重经温度平滑 w^(1/T)；默认系数与历史
/// 3D 权重一致（0.4/0.3/0.3，新维度默认 0），显式开启才生效
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RoutingSection {
    /// 第 1 维：model.weight（配置权重，通常 0.0-2.0）系数
    #[serde(default = "default_w_model")]
    pub w_model: f64,
    /// 第 2 维：queue_factor（队列空闲度，`[0,1]`）系数
    #[serde(default = "default_w_queue")]
    pub w_queue: f64,
    /// 第 3 维：window_factor（Token 预算/窗口，`[0,1]`）系数
    #[serde(default = "default_w_window")]
    pub w_window: f64,
    /// 第 4 维：EWMA 延迟因子系数（latency_ref/(latency_ref+ewma_ms)）。
    /// 默认 0 = 关闭
    #[serde(default)]
    pub w_latency: f64,
    /// 第 4 维：EWMA 错误率因子系数（1 − ewma_error_rate）。默认 0 = 关闭
    #[serde(default)]
    pub w_error: f64,
    /// latency_factor 参考延迟（ms）：vendor EWMA 等于该值时因子 0.5
    #[serde(default = "default_latency_ref_ms")]
    pub latency_ref_ms: f64,
    /// 温度平滑指数：最终权重 w^(1/T)；T=1（默认）不平滑，T>1 压平，
    /// T<1 放大。必须 > 0，非法值按不平滑处理
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// 选择算法："weighted"（默认轮盘赌）| "power_of_two"（Po2C 实验：
    /// 均匀采样 2 候选按 EWMA 延迟择优）| "linucb"（UCB argmax bandit 档，
    /// 需接线 BanditState 且特征可得，否则回退 weighted）。未知值按 "weighted"
    #[serde(default = "default_selection")]
    pub selection: String,
    /// 负载维（queue_factor）输入档位单选："queue"（默认，worker 队列
    /// 深度）/ "in_flight"（在途数）/ "in_flight_relative"（相对化
    /// (x−fleet_mean)/max(1,fleet_mean) 映射 0..100）。负载维单选不叠加
    ///（叠加会双计同一请求）；未知值按 "queue"
    #[serde(default = "default_load_signal")]
    pub load_signal: String,
    /// Peak-EWMA 时间常数 τ（ms）：α_dyn = 1−exp(−Δt/τ)；0 = 无平滑（慎用）
    #[serde(default = "default_ewma_tau_ms")]
    pub ewma_tau_ms: u64,
    /// min+buffer 候选集缓冲：>0 时先取 ewma_e2e ≤ 最低×(1+buffer) 的
    /// 候选集内均匀随机（防打爆最快者）；默认 0.0 = 关闭
    #[serde(default)]
    pub latency_buffer: f64,
    /// 请求级权重开关（X-Weight 头）：默认 false 忽略该头；开启且携带
    /// 合法 X-Weight（有限正数）时覆盖 model.weight。热读，下一请求生效
    #[serde(default)]
    pub allow_request_weight: bool,
    /// 全冷却 fail-open 安全网：默认 false 保持 Validation 错误；开启时
    /// 全候选被 CB/Throttled 剔除则回落「忽略状态但 enabled」候选集。
    /// 仅建议有兜底容量的部署开启
    #[serde(default)]
    pub fail_open_when_all_cooling: bool,
    /// Retry-After 提示：默认 false 原错误冒泡；开启且换路耗尽且全为
    /// Throttled 时返回 AllVendorsThrottled（429 + Retry-After 头）
    #[serde(default)]
    pub retry_after_hint: bool,
    /// 会话亲和模式："hard"（默认，命中即返回）/ "soft"（命中权重
    /// ×(1+affinity_boost_factor) 参与竞争）；未知值按 "hard"
    #[serde(default = "default_affinity_mode")]
    pub affinity_mode: String,
    /// soft 模式命中权重乘子基准：权重乘 (1 + 值)，默认 1.0；
    /// 仅 soft 模式生效，负值按 0（防负权重）
    #[serde(default = "default_affinity_boost_factor")]
    pub affinity_boost_factor: f64,
    /// 亲和键租户隔离：默认 false = legacy 键；开启时键改为
    /// affinity:{api_key_hash}:{session_key}（租户劫持修复，建议开启）。
    /// 注意键格式变更使存量绑定失效（TTL 1h 内自然过渡）
    #[serde(default)]
    pub affinity_tenant_scoped: bool,
    /// SLA 虚拟队列打分强度 V：默认 0.0 = 关闭；>0 时权重末尾乘
    /// 1 / (1 + V·Ĉ + max(0, Q·(α − ŝ)))（Ĉ = `[pricing]` 每 1k 单价，缺价记
    /// 候选集中位数；ŝ = 1 − EWMA 错误率）。按租户分桶，非有限/负值视为关闭
    #[serde(default)]
    pub sla_v: f64,
    /// 价格因子（默认开启）：单价 p = (input+output)/2 每 1M tokens，
    /// 乘性修正 weight ×= min_p / p（低价候选胜出）；未登记 / 0 价候选
    /// 因子 1.0（中性豁免）。关闭时整体跳过。热生效
    #[serde(default = "default_price_factor_enabled")]
    pub price_factor_enabled: bool,
    /// 目标服务水平 α（`[0,1]`，默认 0.95）：α > ŝ 欠账累积、α < ŝ 衰减归 0；
    /// 非有限/越界钳 `[0,1]`
    #[serde(default = "default_sla_alpha")]
    pub sla_alpha: f64,
    /// LPS 延迟前瞻软约束强度 δ：默认 0.0 = 关闭；>0 时权重末尾乘
    /// 1/(1 + δ·Ŵ/Ŵ_ref)（Ŵ_ref = 候选集内 Ŵ 中位数）；负值/非有限视为关闭
    #[serde(default)]
    pub lps_delta: f64,
    /// LPS 硬约束门控：默认 false 不筛选；开启后按 Ŵ≤deadline 筛可行集
    ///（deadline = x-deadline-ms 头），空集改选 Ŵ 最小者
    #[serde(default)]
    pub lps_deadline_gate: bool,
    /// 默认截止（毫秒）：lps_deadline_gate 开启且请求未带 x-deadline-ms 头时使用
    #[serde(default = "default_deadline_ms")]
    pub default_deadline_ms: f64,
    /// LinUCB 探索参数 α：仅 selection="linucb" 时被咨询；0/负值 = 纯贪心，
    /// 非有限按 0。臂矩阵正则初值 = max(α²,1)·I。默认 1.0
    #[serde(default = "default_ucb_alpha")]
    pub ucb_alpha: f64,
}

fn default_price_factor_enabled() -> bool {
    true
}

fn default_affinity_mode() -> String {
    "hard".to_string()
}

fn default_affinity_boost_factor() -> f64 {
    1.0
}

fn default_sla_alpha() -> f64 {
    0.95
}

fn default_deadline_ms() -> f64 {
    10000.0
}

fn default_ucb_alpha() -> f64 {
    1.0
}

fn default_w_model() -> f64 {
    0.4
}

fn default_w_queue() -> f64 {
    0.3
}

fn default_w_window() -> f64 {
    0.3
}

fn default_latency_ref_ms() -> f64 {
    1000.0
}

fn default_temperature() -> f64 {
    1.0
}

fn default_selection() -> String {
    "weighted".into()
}

fn default_load_signal() -> String {
    "queue".into()
}

fn default_ewma_tau_ms() -> u64 {
    10_000
}

impl Default for RoutingSection {
    fn default() -> Self {
        Self {
            w_model: default_w_model(),
            w_queue: default_w_queue(),
            w_window: default_w_window(),
            w_latency: 0.0,
            w_error: 0.0,
            latency_ref_ms: default_latency_ref_ms(),
            temperature: default_temperature(),
            selection: default_selection(),
            load_signal: default_load_signal(),
            ewma_tau_ms: default_ewma_tau_ms(),
            latency_buffer: 0.0,
            allow_request_weight: false,
            fail_open_when_all_cooling: false,
            retry_after_hint: false,
            affinity_mode: default_affinity_mode(),
            affinity_boost_factor: default_affinity_boost_factor(),
            affinity_tenant_scoped: false,
            sla_v: 0.0,
            price_factor_enabled: default_price_factor_enabled(),
            sla_alpha: default_sla_alpha(),
            ucb_alpha: default_ucb_alpha(),
            lps_delta: 0.0,
            lps_deadline_gate: false,
            default_deadline_ms: default_deadline_ms(),
        }
    }
}

impl RoutingSection {
    /// 命名预设：三套开箱即用的系数组合。
    /// - `quality`：偏模型质量（w_model 主导）
    /// - `latency`：偏近期延迟（开启 EWMA 延迟维）
    /// - `cost`：偏窗口预算（window_factor 主导，Token 余额 ≈ 成本预算代理）
    ///
    /// 未知名返回 None。
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "quality" => Some(Self {
                w_model: 0.7,
                w_queue: 0.2,
                w_window: 0.1,
                ..Self::default()
            }),
            "latency" => Some(Self {
                w_model: 0.2,
                w_queue: 0.2,
                w_window: 0.2,
                w_latency: 0.4,
                latency_ref_ms: 500.0,
                ..Self::default()
            }),
            "cost" => Some(Self {
                w_model: 0.2,
                w_queue: 0.2,
                w_window: 0.6,
                ..Self::default()
            }),
            _ => None,
        }
    }
}

impl Default for RetrySection {
    fn default() -> Self {
        Self {
            max_retries: default_retry_max_retries(),
            retryable_status_codes: default_retryable_status_codes(),
            initial_delay_ms: default_retry_initial_delay_ms(),
            factor: default_retry_factor(),
            max_delay_ms: default_retry_max_delay_ms(),
            jitter: default_retry_jitter(),
            spread_jitter: default_retry_spread_jitter(),
            max_total_attempts: default_retry_total_attempts(),
            retry_budget_secs: 0,
            amplification_gate: false,
        }
    }
}

/// `[cache]` 段：精确响应缓存（oxcache memory 后端）。键含调用方 key 哈希
/// 做租户隔离；仅确定性请求查/写（stream=false 且无 tools 且 temperature=0）。
/// enabled 默认 false（TTL 内响应陈旧是产品语义，运维显式打开）。热读
/// [[cache.categories]] 单条：按模型前缀的 TTL 类别乘数，无命中乘数 1.0
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CacheCategory {
    pub model_id_prefix: String,
    pub multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CacheSection {
    /// 是否启用精确响应缓存（默认关闭）
    #[serde(default)]
    pub enabled: bool,
    /// 响应缓存 TTL（秒），默认 300
    #[serde(default = "default_cache_ttl_secs")]
    pub ttl_secs: u64,
    /// TTL 负载联动开关（默认 false = 纯静态 TTL；类别乘数表独立生效）：
    /// 开启时 TTL = ttl_secs × 类别乘数 × (1 + λ·(β_max−1))；λ 以
    /// ewma_e2e_ms 均值替代论文的分位口径（高估方向安全）。热读
    #[serde(default)]
    pub ttl_load_linked: bool,
    /// 延迟项权重（默认 0 = 不参与；与 w_Q 同为 0 时 λ 恒 0、TTL=base）
    #[serde(default = "default_cache_w_l")]
    pub w_l: f64,
    /// 流量份额项权重（默认 0 = 不参与）
    #[serde(default = "default_cache_w_q")]
    pub w_q: f64,
    /// 负载放大上限（λ=1 时 TTL = base×β_max）。默认 1.5（示例值）；
    /// 配置 <1 时按 1 处理（只放大不缩短——缩短会放大回源压力）。
    #[serde(default = "default_cache_beta_max")]
    pub beta_max: f64,
    /// 延迟目标（毫秒，ewma_e2e_ms 口径）。L_p 达到该值时延迟项满权重。
    /// 默认 1000（示例值，按 /admin/metrics 的 ewma_e2e_ms 分布调整）。
    #[serde(default = "default_cache_l_target")]
    pub l_target: f64,
    /// 流量份额目标（0..1）。Q 达到该值时份额项满权重。默认 0.5。
    #[serde(default = "default_cache_q_target")]
    pub q_target: f64,
    /// 类别乘数表（缺省空 = 全部 1.0；与负载联动开关独立生效）
    #[serde(default)]
    pub categories: Vec<CacheCategory>,
    /// TTL 抖动系数（默认 0 = 关闭）：>0 时写入 TTL 按 ttl×(1±factor)
    /// 对称抖动，同批键过期错峰
    #[serde(default)]
    pub ttl_jitter: f64,
    /// 空响应负缓存窗口（秒，默认 60）。store() 收到 Null 响应时写
    /// 哨兵而非值，窗口内 check() 直接 Hit(Null) 不再穿透上游。
    #[serde(default = "default_cache_null_cache_ttl_secs")]
    pub null_cache_ttl_secs: u64,
}

fn default_cache_ttl_secs() -> u64 {
    300
}

fn default_cache_w_l() -> f64 {
    0.0
}

fn default_cache_w_q() -> f64 {
    0.0
}

fn default_cache_beta_max() -> f64 {
    1.5
}

fn default_cache_l_target() -> f64 {
    1000.0
}

fn default_cache_q_target() -> f64 {
    0.5
}

fn default_cache_null_cache_ttl_secs() -> u64 {
    60
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_secs: default_cache_ttl_secs(),
            ttl_load_linked: false,
            w_l: default_cache_w_l(),
            w_q: default_cache_w_q(),
            beta_max: default_cache_beta_max(),
            l_target: default_cache_l_target(),
            q_target: default_cache_q_target(),
            categories: Vec::new(),
            ttl_jitter: 0.0,
            null_cache_ttl_secs: default_cache_null_cache_ttl_secs(),
        }
    }
}

/// `[resilience]` 段：韧性覆盖面。硬约束：必须定义为 Config 的 Section
/// 字段——persist_and_swap 全量写回 config.toml，未进 Config 的段会在
/// 首次热更时从磁盘消失。热读；Default 手写：开关缺省 false，阈值给非零缺省
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ResilienceSection {
    /// 熔断/重试/换路覆盖到透传与流式路径（默认 false = 三条裸路径
    /// 无 CB 不换路）。开启后：透传非流式由 per-vendor CB 包裹；流式路径
    /// 首字节前失败换 vendor 重派，流中错误以 SSE error 终止；CB Open 直接拒绝
    #[serde(default)]
    pub cover_all_paths: bool,
    /// 失败率软降权开关（默认 false）：开启时样本达标且失败率超阈的
    /// vendor 权重 ×failure_rate_penalty；只打折不剔除
    #[serde(default)]
    pub failure_rate_gate: bool,
    /// 触发软降权的最小窗口样本数（total，60s 滑窗）。
    /// 样本不足不惩罚（防小样本误杀）。
    #[serde(default = "default_failure_rate_min_samples")]
    pub failure_rate_min_samples: u64,
    /// 触发软降权的失败率阈值（(0,1]，fail_pct > 阈值才降权）。
    #[serde(default = "default_failure_rate_threshold")]
    pub failure_rate_threshold: f64,
    /// 软降权乘子（权重 ×penalty）。0.1 = LiteLLM 数值对照。
    #[serde(default = "default_failure_rate_penalty")]
    pub failure_rate_penalty: f64,
    /// CB Open→HalfOpen 超时窗口（毫秒），默认 30000；测试可注入短窗口
    #[serde(default = "default_circuit_open_timeout_ms")]
    pub circuit_open_timeout_ms: u64,
    /// 回退链可观测响应头：默认 false；开启时成功响应附
    /// x-nexusflow-vendor / model / attempts / fallbacks。热更段
    #[serde(default)]
    pub expose_fallback_headers: bool,
}

fn default_failure_rate_min_samples() -> u64 {
    5
}

fn default_failure_rate_threshold() -> f64 {
    0.5
}

fn default_failure_rate_penalty() -> f64 {
    0.1
}

fn default_circuit_open_timeout_ms() -> u64 {
    30_000
}

impl Default for ResilienceSection {
    fn default() -> Self {
        Self {
            cover_all_paths: false,
            failure_rate_gate: false,
            failure_rate_min_samples: default_failure_rate_min_samples(),
            failure_rate_threshold: default_failure_rate_threshold(),
            failure_rate_penalty: default_failure_rate_penalty(),
            circuit_open_timeout_ms: default_circuit_open_timeout_ms(),
            expose_fallback_headers: false,
        }
    }
}

/// `[admission]` 段：拥塞感知优先级准入。并发占用率（Σ in_flight / Σ 上限）
/// 达 congestion_threshold 时，低于「当前在途最小档」优先级的新请求直接
/// 429 + Retry-After。Section 字段落地防热更抹段；热读
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AdmissionSection {
    /// 默认 false = 完全关闭（判定旁路、EWMA 不记账、计数不增减）
    #[serde(default)]
    pub enabled: bool,
    /// 拥塞阈值 — Σ in_flight / Σ 并发上限 ≥ 该值视为拥塞。默认 0.9。
    #[serde(default = "default_congestion_threshold")]
    pub congestion_threshold: f64,
    /// 优先级公式系数（论文式 1 典型值 2.0/1.0/4.0）。α_slo 预留
    ///（ℓ* 恒 0，本条不生效）。
    #[serde(default = "default_alpha_slo")]
    pub alpha_slo: f64,
    /// burst EWMA（到达速率）对优先级的压制系数。默认 1.0。
    #[serde(default = "default_alpha_burst")]
    pub alpha_burst: f64,
    /// debt EWMA（被 shed 频度）对优先级的压制系数。默认 4.0。
    #[serde(default = "default_alpha_debt")]
    pub alpha_debt: f64,
}

fn default_congestion_threshold() -> f64 {
    0.9
}

fn default_alpha_slo() -> f64 {
    2.0
}

fn default_alpha_burst() -> f64 {
    1.0
}

fn default_alpha_debt() -> f64 {
    4.0
}

impl Default for AdmissionSection {
    fn default() -> Self {
        Self {
            enabled: false,
            congestion_threshold: default_congestion_threshold(),
            alpha_slo: default_alpha_slo(),
            alpha_burst: default_alpha_burst(),
            alpha_debt: default_alpha_debt(),
        }
    }
}

/// `[rate_limit]` 段：入站限流规则。path=None 的全局规则注入 limiteron
/// Governor（规则级单例桶）；path=Some(前缀) 的端点规则在中间件侧按
/// (rule, 租户) 分桶。restart-required（Router 构造期消费）；
/// rules 为空（缺省）= limiteron 内置默认兜底
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
pub struct RateLimitSection {
    #[serde(default)]
    pub rules: Vec<RateLimitRule>,
}

/// 单条入站限流规则（[[rate_limit.rules]]）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RateLimitRule {
    /// 规则标识（观测/日志维度）
    pub id: String,
    /// 端点路径前缀（前缀匹配，如 "/v1/chat/completions"）。
    /// 缺省 = 全局规则（注入 limiteron Governor）
    #[serde(default)]
    pub path: Option<String>,
    /// 按租户（API key SHA256 摘要）分桶，默认 true；无 key 请求归入
    /// 单一 `anonymous` 租户（此时退化为单一全局桶）
    #[serde(default = "default_rate_limit_per_tenant")]
    pub per_tenant: bool,
    /// 滑动窗口时长（秒）
    pub window_secs: u64,
    /// 窗口内最大请求数
    pub max_requests: u64,
}

fn default_rate_limit_per_tenant() -> bool {
    true
}

/// `[grpc]` 段 — gRPC 服务器配置（sdforge grpc 门面，`feature = "grpc"`）。
/// 建站期消费（`GrpcServerConfig` 固化于服务器构造，与 `[server]`/`[cors]`
/// 同语义），更新后需重启进程生效。
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
pub struct GrpcConfig {
    /// 显式启用（None 段或 false = 不监听 gRPC 端口，行为与历史逐位一致）
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_grpc_host")]
    pub host: String,
    #[serde(default = "default_grpc_port")]
    pub port: u16,
    /// per-call 超时秒数（透传 sdforge `GrpcServerConfig.timeout_seconds`；
    /// 0 = 不限）
    #[serde(default = "default_grpc_timeout_seconds")]
    pub timeout_seconds: u64,
    /// 每连接并发上限（透传 `max_connections`；0 = tonic 默认）
    #[serde(default = "default_grpc_max_connections")]
    pub max_connections: usize,
    /// HTTP/2 keepalive 探测间隔秒数（None = tonic 默认）
    #[serde(default)]
    pub http2_keepalive_interval_secs: Option<u64>,
    /// HTTP/2 keepalive 探测超时秒数（None = tonic 默认）
    #[serde(default)]
    pub http2_keepalive_timeout_secs: Option<u64>,
}

fn default_grpc_host() -> String {
    "0.0.0.0".into()
}

fn default_grpc_port() -> u16 {
    50051
}

fn default_grpc_timeout_seconds() -> u64 {
    30
}

fn default_grpc_max_connections() -> usize {
    1000
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub vendors: Vec<VendorConfig>,
    pub global_model: Option<GlobalModelConfig>,
    /// 价格表 — 路由 SLA 因子与未登记目录模型的配额回退档（计价正源是
    /// `[[model_catalog]]` 四档单价）。默认空 vec
    #[serde(default)]
    pub pricing: Vec<PricingConfig>,
    /// [[model_catalog]] 段：per-model 能力/定价目录。缺省空 vec（未登记
    /// 模型走 per-flavor 保守默认）；定价单位每 1M tokens；context_window
    /// 必须 > 0 且 model_id 去重（validate_config）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_catalog: Vec<ModelCatalogEntry>,
    /// `[auth]` 段 — API Key 认证（摘要存储）。None = 配置中无该段，认证禁用
    #[serde(default)]
    pub auth: Option<AuthSection>,
    /// `[cors]` 段。None = 配置中无该段
    #[serde(default)]
    pub cors: Option<CorsSection>,
    /// `[retry]` 段 — 出站重试策略。缺段时用 Default（429/5xx 重试 3 次 + jitter）
    #[serde(default)]
    pub retry: RetrySection,
    /// `[routing]` 段 — 路由权重系数与选择算法。缺段时用 Default
    /// （0.4/0.3/0.3 + 新维度全关），与历史 3D 权重行为一致
    #[serde(default)]
    pub routing: RoutingSection,
    /// `[cache]` 段 — 精确响应缓存。缺段时 enabled=false（关闭）
    #[serde(default)]
    pub cache: CacheSection,
    /// `[rate_limit]` 段 — 入站限流。缺段/空 rules = limiteron
    /// Governor 内置默认兜底（与历史行为逐位一致）。restart-required。
    #[serde(default)]
    pub rate_limit: RateLimitSection,
    /// `[resilience]` 段 — 韧性覆盖面。缺段时 cover_all_paths=false
    ///（三条裸路径行为与历史逐位一致）。Section 字段落地防热更抹段。
    #[serde(default)]
    pub resilience: ResilienceSection,
    /// `[admission]` 段 — 拥塞感知优先级准入。缺段时 enabled=false
    ///（完全旁路，行为与历史逐位一致）。
    #[serde(default)]
    pub admission: AdmissionSection,
    /// `[health]` 段 — 主动健康检查探针（WP7）。缺段时 mode=off（零后台
    /// 任务、行为与历史逐位一致）。Section 字段落地防热更抹段。
    #[serde(default)]
    pub health: HealthSection,
    /// `[quota]` 段 — 成本与 token 维度配额（WP8）。缺段时 enabled=false
    ///（零判限、行为与历史逐位一致）。Section 字段落地防热更抹段。
    #[serde(default)]
    pub quota: QuotaSection,
    /// `[grpc]` 段 — gRPC 服务器（`feature = "grpc"`）。None = 配置中无该段，
    /// gRPC 服务器禁用（不监听 gRPC 端口，行为与历史逐位一致）
    #[serde(default)]
    pub grpc: Option<GrpcConfig>,
    /// `[docs]` 段 — OpenAPI/Swagger UI 出口（G11）。缺段时关闭。
    #[serde(default)]
    pub docs: DocsSection,
}

/// `[docs]` 段：OpenAPI 文档面开关。默认 false 时 /api-docs 与 Swagger UI
/// 不存在（404）；开启后可达（免认证免限流——开关本身即访问门）
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
pub struct DocsSection {
    /// OpenAPI JSON + Swagger UI 是否挂载（默认 false）。
    #[serde(default)]
    pub enabled: bool,
}

/// `[health]` 段：主动健康检查探针。
/// - mode: off（默认，零后台任务）| suspect_only（仅探测冷却/黑名单/
///   不可用态）| all（全量周期探测）；未知值按 off 并 warn
/// - interval_secs: 探测周期（默认 300）；timeout_ms: 单次探测超时（默认 5000）
/// - probe_strategy: free（默认，GET 上游模型列表，成功仅清 vendor 级冷却）|
///   minimal_call（max_tokens=1 计费验证，成功额外清 key 黑名单与模型级熔断）；
///   未知值按 free 并 warn
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct HealthSection {
    #[serde(default = "default_health_mode")]
    pub mode: String,
    #[serde(default = "default_health_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_probe_strategy")]
    pub probe_strategy: String,
}

fn default_health_mode() -> String {
    "off".into()
}

fn default_health_interval_secs() -> u64 {
    300
}

fn default_health_timeout_ms() -> u64 {
    5000
}

fn default_probe_strategy() -> String {
    "free".into()
}

impl Default for HealthSection {
    fn default() -> Self {
        Self {
            mode: default_health_mode(),
            interval_secs: default_health_interval_secs(),
            timeout_ms: default_health_timeout_ms(),
            probe_strategy: default_probe_strategy(),
        }
    }
}

/// `[quota]` 段：成本与 token 维度配额（预扣-回补）。enabled 默认 false
///（零判限）；window_secs 固定窗口（默认 3600，0 非法）；est_chars_per_token
/// 输入估算常数（默认 4）。per-key 限额放 [[auth.keys]]（0 = 不限）；
/// 窗口账本为进程内存（单实例、重启清零）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct QuotaSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_quota_window_secs")]
    pub window_secs: u64,
    #[serde(default = "default_est_chars_per_token")]
    pub est_chars_per_token: u64,
}

fn default_quota_window_secs() -> u64 {
    3600
}

fn default_est_chars_per_token() -> u64 {
    4
}

impl Default for QuotaSection {
    fn default() -> Self {
        Self {
            enabled: false,
            window_secs: default_quota_window_secs(),
            est_chars_per_token: default_est_chars_per_token(),
        }
    }
}

/// 单条定价配置 — vendor + model 维度的输入/输出 token 单价（每 1k tokens，USD）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PricingConfig {
    pub vendor_id: String,
    pub model_id: String,
    pub input_per_1k_tokens: f64,
    pub output_per_1k_tokens: f64,
}
