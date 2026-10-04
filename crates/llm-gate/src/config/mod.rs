// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use serde::{Deserialize, Serialize};

pub mod loader;
pub mod manager;

pub use loader::validate_config;
pub use manager::ConfigManager;

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
    /// 多 key 池的显式 env 名列表（T005/N5）。语义对齐 agentstem
    /// provider_chain.rs:226-236：显式列表优先于池 env——与 `api_key_env`
    /// （向后兼容单 env 名，旧配置逐位可用）合并去重定序，本列表在前。
    /// 池解析逐名读环境变量得实际 key 串，缺名的 env 跳过并显性告警；
    /// 池上限截断见 `vendor::key_pool::MAX_KEY_POOL`。
    /// 本字段只存环境变量名，绝不存 key 原文（admin 面掩码约定）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_key_envs: Vec<String>,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_vendor_enabled")]
    pub enabled: bool,
    /// fallback 优先级（recId=7）：数值越小优先级越高（1 = primary）。
    /// 换路 fallback 由该字段构建 FallbackChain。
    #[serde(default = "default_vendor_priority")]
    pub priority: u8,
    #[serde(default = "Vec::new")]
    pub models: Vec<ModelConfig>,
    pub rate_limit: RateLimitConfig,
    pub concurrency: ConcurrencyConfig,
    /// 出站 HTTP per-attempt 总超时（毫秒）— 从 `send` 发起起算至响应体
    /// 读完（reqwest 请求级超时语义），不含本地排队等待。recId=4（W2）：
    /// 此前出站 Client 零超时，可无限等待。
    /// **0 = 不限时**（逃生阀：u64 零值若被当作 `Duration::from_millis(0)`
    /// 语义是「立即超时」，故实现必须显式跳过 timeout 包裹）。
    #[serde(default = "default_vendor_timeout_ms")]
    pub timeout_ms: u64,
    /// 流式 TTFT（Time-To-First-Token）超时（毫秒）— 包住 `send` 发起至
    /// 上游响应头返回的段落；首包（头）返回后愿意继续等（Cloudflare
    /// 口径）。recId=4（W2）：此前流式 send 无超时。
    /// **0 = 不限时**（逃生阀，同上）。
    #[serde(default = "default_stream_ttft_ms")]
    pub stream_ttft_ms: u64,
    /// 流式 chunk 间 idle 超时（毫秒）— 相邻上游字节块的最大间隔，超时
    /// 以 `Timeout` 终止流。recId=4（W2）：此前流式无 idle 超时，上游
    /// 挂起可无限占用并发 permit。
    /// **0 = 不限时**（逃生阀：防慢生成流被误杀的部署可关闭）。
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
        /// recId=11：滑动窗口开关。**默认 false = FixedWindowLimiter
        ///（现状，行为逐位一致）**；true = limiteron
        /// ShardedSlidingWindowLimiter（消除固定窗口翻越点的瞬时 2×
        /// 配额双花；reset_secs 以窗口全长近似上界，window_end 精度略降
        /// ——window_factor 仅 is_ending_soon(<300s) 时惩罚，影响可忽略）。
        /// 明确不用 SlidingWindowLimiter（deprecated 且硬编码系统时钟）。
        #[serde(default)]
        sliding: bool,
    },
    #[serde(rename = "token")]
    Token {
        max_tokens: u64,
        /// Token 配额窗口时长（小时）。recId=5：窗口由
        /// limiteron::limiters::FixedWindowLimiter 惰性重置（旧实现
        /// should_reset 只匹配 Interval 变体，Token 窗口永不重置）。
        /// 缺省 24h，兼容旧配置。
        #[serde(default = "default_token_window_hours")]
        window_hours: Option<u64>,
        /// recId=11：滑动窗口开关（语义同 Interval.sliding）。
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
    /// 最大并发在途请求数（含流式）。recId=5：流式 permit 生命周期修复后
    /// permit 真实持有至流结束，该上限对流式生效。缺省 None = 使用队列
    /// 容量（100）。
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,
    /// recId=18：token 加权并发票据开关。**默认 false = 单票据**
    ///（行为与现状逐位一致）。开启时并发票据按预估 token 量计费
    ///（cost = clamp(messages 序列化字节/4 + max_tokens, min, max)，
    /// agentic 长会话不再与短请求等价占票）；#6 的 in_flight 信号
    /// 自动变为 token 加权口径（同一计数点、不同加权维度）。注意：
    /// 本段为 vendor 实例构造期消费（restart-required，与
    /// max_concurrent_requests 同路径）。票据饥饿（长会话占满预算）
    /// 是机制内生风险，token_ticket_max 封顶为逃逸阀。
    #[serde(default)]
    pub token_weighted: bool,
    /// recId=18：单请求票据下限（默认 1）。
    #[serde(default = "default_token_ticket_min")]
    pub token_ticket_min: u64,
    /// recId=18：单请求票据上限（逃逸阀，默认 16384 示例值——超长会话
    /// 也至多占用该票据量，防预算被单个请求耗尽）。
    #[serde(default = "default_token_ticket_max")]
    pub token_ticket_max: u64,
    /// recId=18：max_tokens 缺省兜底（容量规划前提——请求未声明
    /// max_tokens 时按该值计费）。默认 4096。
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

/// recId=16：API key 档位 — 拥塞准入的优先级基准
///（dedicated/guaranteed/elastic/spot）。serde 严格枚举：未知档位字符串
/// 使配置加载失败（显式配置错误早暴露；与 load_signal 的运行时单选档
/// 不同，本值无运行时回退路径语义）。
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum AuthTier {
    #[serde(rename = "dedicated")]
    Dedicated,
    #[serde(rename = "guaranteed")]
    Guaranteed,
    /// 缺省档（未在 [auth].keys 声明的 key 全部落此档——行为不变）
    #[default]
    #[serde(rename = "elastic")]
    Elastic,
    #[serde(rename = "spot")]
    Spot,
}

/// recId=16：[[auth.keys]] 单条 — key 摘要 → 档位。hash 与 api_keys 同
/// 形态（SHA256 hex，明文不落盘）。未在 keys 声明的 key（含 api_keys
/// 列表全部）缺省 [`AuthTier::Elastic`]（行为不变）。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AuthKeyEntry {
    pub hash: String,
    #[serde(default)]
    pub tier: AuthTier,
}

/// [auth] 段 — API Key 认证配置。
///
/// `api_keys` 存 SHA256 hex 摘要（与 config.toml 注释一致）：
/// `echo -n "your-api-key" | sha256sum`，认证时对请求 key 现场算摘要做
/// 常量时间比较，明文密钥不落盘、不进内存。
///
/// recId=16：可选 `keys` 子表声明 key 档位（拥塞准入的优先级基准）；
/// 缺省（空 vec）→ 全部 key 视为 elastic 且准入不启用，行为与现状
/// 逐位一致。热读：认证与准入每请求从热配置现读。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AuthSection {
    #[serde(default = "default_auth_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub header: AuthHeaderSection,
    /// recId=16：key 档位表（可选；缺省全 elastic、准入不启用）
    #[serde(default)]
    pub keys: Vec<AuthKeyEntry>,
}

fn default_auth_enabled() -> bool {
    true
}

/// [auth.header] 子段 — 请求头名与前缀（保留段结构；当前中间件
/// 按 Authorization/Bearer 提取，此处先保真加载防止回写抹段）
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

/// [cors] 段 — CORS 配置（serde 全 default，未配置段时 Option 为 None）
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

/// [retry] 段 — 出站重试策略（recId=7）。
///
/// 热读路径：worker 每请求从 `ConfigManager::read().retry` 现读，
/// POST /admin/config 全量更新后立即生效（recId=13 热更新地基）。
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
    /// 同步惊群展宽比例（recId=5）：Retry-After 与 5xx 固定间隔分支的
    /// 延迟 = base + U(0, spread_jitter) × base（base 为 Retry-After 值
    /// 或 1s）。1.0 = 全范围展宽 [base, 2×base)，消除多实例同刻重试；
    /// 0.0 = 完全恢复旧行为（Retry-After 原样返回、5xx 固定 1s）——回滚阀。
    #[serde(default = "default_retry_spread_jitter")]
    pub spread_jitter: f64,
    /// 跨 vendor 总尝试预算（换路 fallback 的 attempts 上限，防
    /// vendor 数 × 重试数爆炸）
    #[serde(default = "default_retry_total_attempts")]
    pub max_total_attempts: u32,
    /// recId=9（W5）：单请求重试总时间预算（秒）。**默认 0 = 不限时**
    ///（现状）；>0 时重试循环与跨 vendor fallback 循环在每次等待前检查
    /// 「锚点耗时 + 下次延迟」是否超预算，超限返回最后错误（Portkey
    /// 递减式预算对照——防重试等待把单请求拖到分钟级）。锚点统一
    /// tokio::time::Instant（std Instant 不随 paused clock 推进）。
    #[serde(default)]
    pub retry_budget_secs: u64,
    /// recId=9（W5）：重试放大守门器（λ_eff/f_c 亚稳态防护）。**默认
    /// false = 只靠 metrics 计数、行为与现状逐位一致**。开启时在换路
    /// fallback 入口评估：f > 0.8·f_c（失败率逼近临界失败率）→ 本请求
    /// max_total_attempts 临时收紧到 1 + tracing::warn（误触发退化为单
    /// 尝试，fail-safe 方向正确）；r·f > 0.95 → 发散告警（只 warn）。
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

/// [routing] 段 — 路由权重系数与选择算法（recId=10）。
///
/// 热读路径：`Router` 每次路由从热配置现读（confers::dynamic::DynamicField
/// update 后 `read()` 立即可见），POST /admin/config 更新本段后**立即生效，
/// 无需重启**。
///
/// 权重公式（recId=10③）：
/// `weight = w_model×model.weight + w_queue×queue_factor + w_window×window_factor
///        + w_latency×latency_factor + w_error×error_factor`，最终权重再经
/// 温度平滑 `w^(1/T)`（herding 缓解）。
///
/// 硬约束：默认值与历史 3D 权重（0.4/0.3/0.3）完全一致，且新维度
/// （w_latency/w_error）默认 0 —— 线上路由行为不变，显式开启才生效。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RoutingSection {
    /// 第 1 维：model.weight（配置权重，通常 0.0-2.0）系数
    #[serde(default = "default_w_model")]
    pub w_model: f64,
    /// 第 2 维：queue_factor（队列空闲度，[0,1]）系数
    #[serde(default = "default_w_queue")]
    pub w_queue: f64,
    /// 第 3 维：window_factor（Token 预算/窗口，[0,1]）系数
    #[serde(default = "default_w_window")]
    pub w_window: f64,
    /// 第 4 维（recId=10③）：EWMA 延迟因子系数。默认 0 = 关闭；开启后
    /// latency_factor = latency_ref_ms / (latency_ref_ms + vendor_ewma_ms)，
    /// 有界 (0,1)，快厂商得分高。
    #[serde(default)]
    pub w_latency: f64,
    /// 第 4 维（recId=10③）：EWMA 错误率因子系数。默认 0 = 关闭；开启后
    /// error_factor = 1 - ewma_error_rate，有界 [0,1]。
    #[serde(default)]
    pub w_error: f64,
    /// latency_factor 的参考延迟（ms）：vendor EWMA 等于该值时因子 0.5。
    /// 默认 1000ms，可按全局 P50 延迟热调整。
    #[serde(default = "default_latency_ref_ms")]
    pub latency_ref_ms: f64,
    /// 温度平滑指数（recId=10④，herding 缓解）：最终权重 w^(1/T)。
    /// T=1（默认）不平滑；T>1 压平差距（趋向均匀探索）；T<1 放大差距。
    /// 必须 > 0，非法值按不平滑处理。
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// 选择算法："weighted"（默认轮盘赌）| "power_of_two"（Po2C 实验：
    /// 均匀采样 2 候选、按 EWMA 延迟择优）| "linucb"（recId=15：LinUCB
    /// bandit 档——候选集内按 UCB=⟨x,θ̂⟩+α√(xᵀA⁻¹x) argmax，per
    /// (abstract_model, vendor_id) 臂，成败 0/1 代理奖励；需 Router 接线
    /// BanditState 且请求特征可得，未接线/特征缺失时回退 weighted）。默认
    /// weighted，linucb 不被咨询、无状态更新；Po2C 为实验开关（Astrolabe
    /// 数字属 vLLM 同模型场景，等熵监控有数据后再评估）。未知值一律按
    /// "weighted"（默认与回滚同路径）。
    #[serde(default = "default_selection")]
    pub selection: String,
    /// recId=6：负载维（queue_factor）输入档位，单选开关：
    /// - `"queue"`（默认）— worker 队列深度（历史行为，线上路径不变）；
    /// - `"in_flight"` — 在途请求数（并发 permit 占用，含长流，recId=6 主信号）；
    /// - `"in_flight_relative"` — 在途相对化 (x−fleet_mean)/max(1,fleet_mean)
    ///   （production-stack 公式），线性映射到 queue_factor 的 0..100 输入域。
    ///
    /// 决策记录：排队深度**不**与在途数叠加为次级加权项——负载维输入是
    /// 单选（叠加会双计同一请求且系数不可分）；选 in-flight 档时队列深度
    /// 退出该维。未知值一律按 `"queue"`（默认与回滚同路径）。
    #[serde(default = "default_load_signal")]
    pub load_signal: String,
    /// recId=8：Peak-EWMA 时间常数 τ（ms）— α_dyn = 1−exp(−Δt/τ)（Kong
    /// latency.lua DECAY_TIME=10s 同值）。久未采样 α→1 快速反映恢复，高频
    /// 采样平滑抗噪。经 VendorManager 落账前热同步到 metrics。0 = 极限
    /// 「完全采新样本」（无平滑），运维慎用。
    #[serde(default = "default_ewma_tau_ms")]
    pub ewma_tau_ms: u64,
    /// recId=8：min+buffer 候选集缓冲（LiteLLM lowest_latency_buffer 语义）—
    /// 大于 0 时选择先取 ewma_e2e ≤ 最低×(1+buffer) 的候选集、集合内均匀随机
    ///（防打爆最快者）。**默认 0.0 = 关闭**（轮盘赌/Po2C 与历史逐位一致；
    /// config_toml_example_test 结构性断言）。
    ///
    /// 顺序依赖：延迟道已按 e2e 口径分道（recId=8），w_latency 调非零应
    /// 在分道落地之后评估（config.toml 注释同步注明）。
    #[serde(default)]
    pub latency_buffer: f64,
    /// recId=19(a)：请求级权重开关（X-Weight 头，LiteLLM simple_shuffle
    /// _router_weights 对照）。**默认 false = 头被忽略**（路由行为与现状
    /// 逐位一致）。开启且请求携带合法 X-Weight（有限正数）时覆盖
    /// model.weight 参与权重计算；门控在 Router 侧热读判定（本开关
    /// POST /admin/config 更新后下一请求生效）。
    #[serde(default)]
    pub allow_request_weight: bool,
    /// recId=19(b)：全冷却 fail-open 安全网。**默认 false = 全候选被剔除
    /// 时保持 Validation 错误（现状）**。开启时全部候选因 CB Open /
    /// Throttled 被路由层剔除的情况下，回落「忽略状态但 enabled」的候选
    /// 集加权选择。现实约束：NexusFlow 无主动健康检查驱动的冷却，全部
    /// 为真实失败冷却——本开关覆盖的是 CB Open 超时未过窗口 / 限流窗口
    /// 未重置，语义弱于 LiteLLM 的 health-check 限定；仅建议有兜底容量
    /// 的部署开启（把已冷却 vendor 重新纳入流量是有意冒险）。
    #[serde(default)]
    pub fail_open_when_all_cooling: bool,
    /// recId=19(c)：Retry-After 提示。**默认 false = 耗尽时原错误冒泡
    /// （503 语义不变）**。开启且换路 fallback 耗尽时全部排除原因为
    /// Throttled → 返回 AllVendorsThrottled（429 + Retry-After 头，
    /// reset 秒数取各 vendor 窗口重置剩余的 min）。首次路由即全
    /// Throttled 的入口场景（handler 首路由 ? 早退）本轮不覆盖。
    #[serde(default)]
    pub retry_after_hint: bool,
    /// recId=10(1)：会话亲和模式 — `"hard"`（默认，现状：命中即返回，
    /// 绑定 vendor 在合格集合内时必选）；`"soft"`：命中软化为评分项——
    /// 命中 vendor 权重 ×(1 + affinity_boost_factor) 后与其余候选照常
    /// 竞争。未知值一律按 `"hard"`（默认与回滚同路径）。
    #[serde(default = "default_affinity_mode")]
    pub affinity_mode: String,
    /// recId=10(1)：soft 模式下命中 vendor 的权重乘子基准 — 权重乘
    /// (1 + affinity_boost_factor)。默认 1.0（×2 boost）；仅在
    /// affinity_mode = "soft" 时生效，负值按 0 处理（防负权重）。
    /// production-stack score = 亲和收益 − β·相对负载的形态以乘子近似
    ///（β=0 语义修正见调研报告）。热生效：POST /admin/config 下一请求
    /// 即用新值。
    #[serde(default = "default_affinity_boost_factor")]
    pub affinity_boost_factor: f64,
    /// recId=10(2)：亲和键租户隔离。**默认 false = legacy 键
    /// `affinity:{session_key}`（现状，行为与键格式逐位一致）**。开启时
    /// 键改为 `affinity:{api_key_hash}:{session_key}`（hash 与响应缓存
    /// cache_api_key_hash 同源）——实为租户劫持修复（两租户同 session id
    /// 不再互相劫持），建议上线开启；注意键格式变更换代使存量绑定全部
    /// 失效（一次性，TTL 1h 内自然过渡）。API 层 bind/rebind/lookup
    /// 与路由侧 lookup 同源判定。
    #[serde(default)]
    pub affinity_tenant_scoped: bool,
    /// recId=14：SLA/满意度虚拟队列打分项强度 V（SLARouter 零模型版）。
    /// **默认 0.0 = 关闭**：惩罚因子不参与权重、虚拟队列 Q 不更新（路由
    /// 行为与现状逐位一致）。>0 时每候选权重末尾乘惩罚因子
    /// `1 / (1 + V·Ĉ_m + max(0, Q·(α − ŝ_m)))`，其中 ŝ_m = 1 − EWMA 错误率
    ///（`vendor_ewma`，无观测数据 = 1.0 冷启动乐观）；Ĉ_m = [pricing] 的
    /// (input+output)/2 每 1k 单价，缺价候选记「候选集内已知单价的中位数」
    ///（保守不奖励——记 0 会让无定价 vendor 获得最小惩罚、乘法式下权重
    /// 最高，与成本感知目的相反），候选集全无定价时成本项整体置中性 0
    ///（退化为纯 Q 项）。非有限/负值同样视为关闭（>0 门控）。虚拟队列
    /// 按租户（API key 哈希）分桶，不受 `affinity_tenant_scoped` 约束
    ///（那只是亲和键格式开关）。
    #[serde(default)]
    pub sla_v: f64,
    /// recId=14：目标服务水平 α（[0,1]）。虚拟队列对偶更新
    /// `Q ← max(0, Q + α − ŝ_selected)`（决策时更新，SLARouter Eq 4/5 的
    /// Lyapunov 语义）：α < ŝ（满意度达标）→ Q 衰减归 0；α > ŝ（服务
    /// 欠账）→ Q 累积推高惩罚。默认 0.95；非有限/越界值钳到 [0,1]
    ///（NaN 按 0 处理 = Q 恒 0，退化为纯成本项——fail-safe 方向）。
    #[serde(default = "default_sla_alpha")]
    pub sla_alpha: f64,
    /// recId=17：LPS 排队论延迟前瞻的软约束强度 δ。**默认 0.0 = 关闭**：
    /// Ŵ 不参与权重（行为与现状逐位一致）。>0 时权重末尾乘
    /// `1/(1 + δ·Ŵ/Ŵ_ref)`（乘法软约束——对报告线性 −δ·Ŵ 的有意偏差：
    /// 线性负项会触发 compute_weight 非正归零保护造成权重归零剔除，
    /// 见 router.rs lps 注释）。Ŵ_ref = 当次候选集内 Ŵ 中位数（相对
    /// 归一，零新增配置；全候选发散时中位数=CAP → 因子均匀不扭曲选择）。
    /// 负值/非有限视为关闭。
    #[serde(default)]
    pub lps_delta: f64,
    /// recId=17：LPS 硬约束门控。**默认 false = 不消费 deadline、不做
    /// 可行集筛选**（行为与现状逐位一致）。开启后请求按 `Ŵ≤deadline`
    /// 筛可行集（deadline = x-deadline-ms 头，缺省用
    /// [`Self::default_deadline_ms`]），可行集为空时改选 Ŵ 最小者
    ///（JITServe 软信号语义）。亲和 hard 命中优先于本筛选（现状语义）。
    #[serde(default)]
    pub lps_deadline_gate: bool,
    /// recId=17：默认截止（毫秒）——lps_deadline_gate 开启且请求未携带
    /// x-deadline-ms 头时使用。默认 10000（示例值，按业务 P99/滑动观察
    /// /admin/metrics 的 ewma_e2e_ms 调整）。
    #[serde(default = "default_deadline_ms")]
    pub default_deadline_ms: f64,
    /// recId=15：LinUCB 探索参数 α — `ucb(x,α) = ⟨x,θ̂⟩ + α√(xᵀA⁻¹x)` 的
    /// 探索半径。**仅 `[routing].selection = "linucb"` 时被咨询**（默认
    /// selection="weighted"，本参数不参与任何路径——关闭由 selection 档位
    /// 承担）；0/负值 = 关闭探索（纯贪心 θ̂ argmax），非有限按 0。臂矩阵
    /// 正则初值 = max(α²,1)·I（构造期定死，α 热更只影响探索半径）。默认 1.0。
    #[serde(default = "default_ucb_alpha")]
    pub ucb_alpha: f64,
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
            sla_alpha: default_sla_alpha(),
            ucb_alpha: default_ucb_alpha(),
            lps_delta: 0.0,
            lps_deadline_gate: false,
            default_deadline_ms: default_deadline_ms(),
        }
    }
}

impl RoutingSection {
    /// 命名预设（recId=10①）：三套开箱即用的系数组合。
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

/// [cache] 段 — 精确响应缓存（recId=12）。
///
/// 存储：oxcache `CacheBackend`（memory 后端），绝不手写 map。键形
/// `resp:{api_key_sha256}:{vendor_id}:{model_id}:{sha256(canonical_body)}`
/// （调用方 API key 哈希入键做租户隔离）。仅确定性请求查/写：
/// `stream==false && tools 为空 && temperature==0 或缺省`。
///
/// 热读路径：`ResponseCache` 每次操作从 `ConfigManager::read().cache`
/// 现读，POST /admin/config 更新后立即生效（无需重启）。
///
/// `enabled` 默认 **false**：TTL 内响应陈旧是产品语义，上线由运维显式
/// 打开（config.toml 注释同步注明）。
/// recId=12：[[cache.categories]] 单条 — 按模型前缀的 TTL 类别乘数
///（(endpoint, model) 划分的零开销近似：缓存键已含 model 维度，前缀
/// 匹配即可）。无命中的 model 乘数 1.0。
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
    /// recId=12：TTL 负载联动开关。**默认 false = 纯静态 TTL**（与现状
    /// 逐位一致；类别乘数表独立生效）。开启时实际 TTL =
    /// `ttl_secs × 类别乘数 × (1 + λ·(β_max−1))`，其中
    /// λ = min(1, (L_p/L_target)·w_L + (Q/Q_target)·w_Q)。**口径降级
    /// 声明**：论文 L_p 是延迟分位（P95），本实现以 ewma_e2e_ms 均值
    /// 替代（不引入分位估计器；高估方向比低估安全——陈旧容忍度随负载
    /// 放大符合论文意图）。热读：每请求从热配置现读。
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
    /// recId=12：类别乘数表（缺省空 = 全部 1.0；与负载联动开关独立生效）
    #[serde(default)]
    pub categories: Vec<CacheCategory>,
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
        }
    }
}

/// [resilience] 段 — 韧性覆盖面（recId=2 / recId=7）。
///
/// 硬约束：必须定义为 Config 结构体的 Section 字段 —— persist_and_swap 以
/// `toml::to_string_pretty` 全量写回 config.toml（manager.rs 实证），未进
/// Config 的段会在首次 POST /admin/config 热更时从磁盘消失。
///
/// 热读路径：执行路径每请求从 `ConfigManager::read().resilience` 现读，
/// POST /admin/config 更新后下一次请求即生效。
/// Default 手写实现：cover_all_paths / failure_rate_gate 缺省 false
///（结构上锁定默认路径行为不变），阈值参数给可用的非零缺省。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ResilienceSection {
    /// 熔断/重试/换路覆盖到透传与流式路径（recId=2）。**默认 false**：
    /// 关闭时透传非流式（无 CB）、透传流式、转换流式三条裸路径与历史
    /// 行为逐位一致（只有转换非流式有换路 fallback，recId=7⑤ 先例）。
    /// 开启后：
    /// - 透传非流式由 per-vendor CircuitBreaker 包裹，失败进入换路循环；
    /// - 两条流式路径 send+响应头校验段由 CB 包裹，首字节（响应头）前
    ///   失败换 vendor 重派，流中错误仍以 SSE error 事件终止不换路；
    /// - CB Open 状态会直接拒绝透传/流式请求（触发换路）——运维影响面
    ///   见 config.toml [resilience] 注释。
    #[serde(default)]
    pub cover_all_paths: bool,
    /// recId=7（W5）：失败率软降权开关。**默认 false**——关闭时路由权重
    /// 与现状逐位一致（窗口计数在 metrics 常开，纯观测）。开启时窗口
    /// 样本达标且失败率超阈的 vendor 权重 ×`failure_rate_penalty`
    ///（LiteLLM 数值对照 0.1）：软降权只打折不剔除——路由层 ==Available
    /// 过滤不动，仅作用于仍 Available 的劣化 vendor。
    #[serde(default)]
    pub failure_rate_gate: bool,
    /// recId=7（W5）：触发软降权的最小窗口样本数（total，60s 滑窗）。
    /// 样本不足不惩罚（防小样本误杀）。
    #[serde(default = "default_failure_rate_min_samples")]
    pub failure_rate_min_samples: u64,
    /// recId=7（W5）：触发软降权的失败率阈值（(0,1]，fail_pct > 阈值才降权）。
    #[serde(default = "default_failure_rate_threshold")]
    pub failure_rate_threshold: f64,
    /// recId=7（W5）：软降权乘子（权重 ×penalty）。0.1 = LiteLLM 数值对照。
    #[serde(default = "default_failure_rate_penalty")]
    pub failure_rate_penalty: f64,
    /// CB Open→HalfOpen 的超时窗口（毫秒）。**默认 30000**：与既有硬编码
    /// 行为逐位一致。调小加速故障恢复探测、调大避免对持续故障上游的
    /// 探针风暴；测试经此注入短窗口以替代真实秒级睡眠。
    #[serde(default = "default_circuit_open_timeout_ms")]
    pub circuit_open_timeout_ms: u64,
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
        }
    }
}

/// [admission] 段 — 拥塞感知优先级准入（recId=16）。
///
/// 网关侧并发占用率（Σ in_flight / Σ 上限，复用 #6 的
/// ConcurrencyLimiter::in_flight）达到 [`Self::congestion_threshold`]
/// 时，新请求按档位优先级与「当前在途最小档」比较：低于者直接
/// 429 + Retry-After（不排队）。优先级 = w_tier·(1+α_slo·ℓ*)⁻¹·
/// (1+α_burst·b)⁻¹·(1+α_debt·d)⁻¹（ℓ* SLO 缺口维度本条无数据源恒 0，
/// 该项恒 1；b/d 为 per-key burst/debt EWMA）。
///
/// 硬约束：必须定义为 Config 结构体的 Section 字段 —— persist_and_swap
/// 全量写回 config.toml（manager.rs 实证），未进 Config 的段会在首次
/// POST /admin/config 热更时从磁盘消失。热读：准入每请求现读，
/// enabled/阈值/系数热更立即生效。
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AdmissionSection {
    /// **默认 false = 完全关闭**：准入判定旁路、EWMA 不记账、per-tier
    /// 在途计数不增减（行为与现状逐位一致；config_toml_example_test
    /// 结构性断言）。
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

/// [rate_limit] 段 — 入站限流规则（recId=3）。
///
/// 缺陷背景：该段此前被 serde 静默丢弃（Config 无此字段），
/// rate_limit_middleware 一直跑 limiteron Governor 的库内置默认兜底规则，
/// 配置文件中的限流参数从未生效。
///
/// 消费方式（recId=3 方案 a/b）：
/// - `path = None` 的全局规则 → 转为 limiteron `Rule`（`Matcher::User` +
///   SlidingWindow）于 Router 构造期注入 Governor；limiteron 决策链为规则级
///   单例桶（`DecisionChain::check` 无 context 参数），该层不分租户；
/// - `path = Some(前缀)` 的端点规则 → 中间件侧端点表（limiteron 原语
///   `ShardedSlidingWindowLimiter` 直接实例化），按 `(rule, 租户)` 分桶
///   （limiteron Matcher 六变体无 path 维度，端点自筛在中间件完成）。
///
/// 热更：**restart-required** —— 本段在 Router 构造期消费（与 [server]/
/// [cors] 先例一致），POST /admin/config 更新不热生效；热下发列为二期。
///
/// `rules` 为空（缺省）= limiteron Governor 内置默认兜底 + 无端点表，
/// 行为与历史版本逐位一致。
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
    /// 按租户（调用方 API key 的 SHA256 摘要）分桶。默认 true。
    /// 无 key 的请求统一归入单一 `anonymous` 租户（沿用
    /// response_cache::cache_api_key_hash 先例）——此时退化为单一全局桶。
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

/// [grpc] 段 — gRPC 服务器配置（sdforge grpc 门面，`feature = "grpc"`）。
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
    /// 价格表 — 用于成本计算。默认空 vec，未配置时 CostCalculator 全部返回 0.0
    #[serde(default)]
    pub pricing: Vec<PricingConfig>,
    /// [auth] 段 — API Key 认证（摘要存储）。None = 配置中无该段，认证禁用
    #[serde(default)]
    pub auth: Option<AuthSection>,
    /// [cors] 段。None = 配置中无该段
    #[serde(default)]
    pub cors: Option<CorsSection>,
    /// [retry] 段 — 出站重试策略。缺段时用 Default（429/5xx 重试 3 次 + jitter）
    #[serde(default)]
    pub retry: RetrySection,
    /// [routing] 段 — 路由权重系数与选择算法（recId=10）。缺段时用 Default
    /// （0.4/0.3/0.3 + 新维度全关），与历史 3D 权重行为一致
    #[serde(default)]
    pub routing: RoutingSection,
    /// [cache] 段 — 精确响应缓存（recId=12）。缺段时 enabled=false（关闭）
    #[serde(default)]
    pub cache: CacheSection,
    /// [rate_limit] 段 — 入站限流（recId=3）。缺段/空 rules = limiteron
    /// Governor 内置默认兜底（与历史行为逐位一致）。restart-required。
    #[serde(default)]
    pub rate_limit: RateLimitSection,
    /// [resilience] 段 — 韧性覆盖面（recId=2）。缺段时 cover_all_paths=false
    ///（三条裸路径行为与历史逐位一致）。Section 字段落地防热更抹段。
    #[serde(default)]
    pub resilience: ResilienceSection,
    /// [admission] 段 — 拥塞感知优先级准入（recId=16）。缺段时 enabled=false
    ///（完全旁路，行为与历史逐位一致）。
    #[serde(default)]
    pub admission: AdmissionSection,
    /// [grpc] 段 — gRPC 服务器（`feature = "grpc"`）。None = 配置中无该段，
    /// gRPC 服务器禁用（不监听 gRPC 端口，行为与历史逐位一致）
    #[serde(default)]
    pub grpc: Option<GrpcConfig>,
}

/// 单条定价配置 — vendor + model 维度的输入/输出 token 单价（每 1k tokens，USD）
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PricingConfig {
    pub vendor_id: String,
    pub model_id: String,
    pub input_per_1k_tokens: f64,
    pub output_per_1k_tokens: f64,
}
