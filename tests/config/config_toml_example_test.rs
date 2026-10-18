// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

// 快速验证：config.example.toml 可被 fluxgate::config::Config 反序列化
//
// 事实源声明：随仓唯一的全量参考是 `config.example.toml`（运行时的
// `config.toml` 是部署私有物、已进 .gitignore，CI 干净检出拿不到它）。
// 本测试对示例做两类锁定：① 默认值结构锁定（复制即用 = 默认行为）；
// ② 键存在性锁定（serde `#[serde(default)]` 会让缺席键静默过解析，只有
// 键名清单能测出示例过期）——键名清单分两层：`config_toml_example_parses`
// 里的显式锚点（键级 + 值级），与 `config_toml_example_covers_full_key_surface`
// 对 Config 序列化面做的全量键名扫描（新键缺席即红，不再依赖人工补清单）。
// 新增配置项必须同步示例；新 `skip_serializing_if` 字段须同步序列化面探针。
#[test]
fn config_toml_example_parses() {
    let content = std::fs::read_to_string("config.example.toml").unwrap();
    let config: fluxgate::config::Config = toml::from_str(&content).unwrap();
    assert_eq!(config.retry.max_retries, 3);
    assert_eq!(config.retry.max_total_attempts, 8);
    // 同步惊群展宽默认全开；回滚阀 = 0（见 config.example.toml 注释）
    assert!((config.retry.spread_jitter - 1.0).abs() < 1e-9);
    // 重试时间预算默认 0 = 不限时、放大守门器默认 false
    //（双开关默认 → 单 vendor 重试与跨 vendor fallback 行为与现状逐位一致）
    assert_eq!(config.retry.retry_budget_secs, 0);
    assert!(!config.retry.amplification_gate);
    // 示例文件必须显式带出两个新键（serde #[serde(default)] 会让缺席键
    // 静默过解析，解析断言测不出示例缺席）
    assert!(
        content.contains("retry_budget_secs"),
        "config.example.toml 示例缺 retry_budget_secs 键"
    );
    assert!(
        content.contains("amplification_gate"),
        "config.example.toml 示例缺 amplification_gate 键"
    );
    // [routing] 段示例与默认行为一致（3D 权重、新维度全关）
    assert_eq!(config.routing.w_model, 0.4);
    assert_eq!(config.routing.w_queue, 0.3);
    assert_eq!(config.routing.w_window, 0.3);
    assert_eq!(config.routing.w_latency, 0.0);
    assert_eq!(config.routing.w_error, 0.0);
    assert_eq!(config.routing.selection, "weighted");
    // 负载信号档位默认 "queue"（Po2C 同款结构性锁定——默认路径
    // 行为与历史逐位一致）
    assert_eq!(config.routing.load_signal, "queue");
    // Peak-EWMA τ 默认 10s（Kong 同值）；min+buffer 候选集默认
    // 0.0 关闭（结构性锁定：轮盘赌/Po2C 与历史逐位一致）
    assert_eq!(config.routing.ewma_tau_ms, 10_000);
    assert_eq!(config.routing.latency_buffer, 0.0);
    // 冷却/候选集判定改进包三开关默认关（请求级权重头忽略、
    // 全冷却保持 Validation 错误、耗尽原错误冒泡——均与现状逐位一致）
    assert!(!config.routing.allow_request_weight);
    assert!(!config.routing.fail_open_when_all_cooling);
    assert!(!config.routing.retry_after_hint);
    // 示例文件必须显式带出三个新键（serde 缺省会让缺席键静默过解析）
    assert!(
        content.contains("allow_request_weight"),
        "config.example.toml 示例缺 allow_request_weight 键"
    );
    assert!(
        content.contains("fail_open_when_all_cooling"),
        "config.example.toml 示例缺 fail_open_when_all_cooling 键"
    );
    assert!(
        content.contains("retry_after_hint"),
        "config.example.toml 示例缺 retry_after_hint 键"
    );
    // 亲和软化三参数默认（hard 模式 = 现状逐位一致、boost 基准
    // 1.0 仅 soft 模式生效、租户隔离关 = legacy 键格式）
    assert_eq!(config.routing.affinity_mode, "hard");
    assert!((config.routing.affinity_boost_factor - 1.0).abs() < 1e-9);
    assert!(!config.routing.affinity_tenant_scoped);
    assert!(
        content.contains("affinity_mode"),
        "config.example.toml 示例缺 affinity_mode 键"
    );
    assert!(
        content.contains("affinity_boost_factor"),
        "config.example.toml 示例缺 affinity_boost_factor 键"
    );
    assert!(
        content.contains("affinity_tenant_scoped"),
        "config.example.toml 示例缺 affinity_tenant_scoped 键"
    );
    // SLA 虚拟队列打分项默认关（sla_v=0.0 → 惩罚因子不参与权重、
    // Q 不更新，路由行为与现状逐位一致；Po2C/latency_buffer 同款结构性
    // 锁定），sla_alpha 默认 0.95（目标服务水平）
    assert_eq!(config.routing.sla_v, 0.0);
    assert!((config.routing.sla_alpha - 0.95).abs() < 1e-9);
    assert!(
        content.contains("sla_v"),
        "config.example.toml 示例缺 sla_v 键"
    );
    assert!(
        content.contains("sla_alpha"),
        "config.example.toml 示例缺 sla_alpha 键"
    );
    // selection 默认 "weighted"（上方已有断言——LinUCB 第三档
    // 不被咨询、无状态更新，Po2C 开关同款结构性锁定）；ucb_alpha 默认 1.0
    //（仅 linucb 档被咨询）
    assert!((config.routing.ucb_alpha - 1.0).abs() < 1e-9);
    assert!(
        content.contains("ucb_alpha"),
        "config.example.toml 示例缺 ucb_alpha 键"
    );
    // [admission] 段默认关（enabled=false → 判定旁路、EWMA 不
    // 记账，行为与现状逐位一致；Po2C 开关同款结构性断言）；auth.keys
    // 缺省（空）→ 全部 key 缺省 elastic，档位准入不启用
    assert!(!config.admission.enabled);
    assert!((config.admission.congestion_threshold - 0.9).abs() < 1e-9);
    assert!((config.admission.alpha_slo - 2.0).abs() < 1e-9);
    assert!((config.admission.alpha_burst - 1.0).abs() < 1e-9);
    assert!((config.admission.alpha_debt - 4.0).abs() < 1e-9);
    assert!(
        config
            .auth
            .as_ref()
            .map(|a| a.keys.is_empty())
            .unwrap_or(true),
        "示例的 [auth].keys 必须缺省（全 elastic）"
    );
    assert!(
        content.contains("congestion_threshold"),
        "config.example.toml 示例缺 congestion_threshold 键"
    );
    assert!(
        content.contains("auth.keys"),
        "config.example.toml 示例缺 auth.keys 注释示例"
    );
    // LPS 前瞻默认关（lps_delta=0.0 → Ŵ 不参与权重、
    // lps_deadline_gate=false → 不消费 deadline，行为与现状逐位一致）
    assert_eq!(config.routing.lps_delta, 0.0);
    assert!(!config.routing.lps_deadline_gate);
    assert!((config.routing.default_deadline_ms - 10000.0).abs() < 1e-9);
    assert!(
        content.contains("lps_delta"),
        "config.example.toml 示例缺 lps_delta 键"
    );
    assert!(
        content.contains("lps_deadline_gate"),
        "config.example.toml 示例缺 lps_deadline_gate 键"
    );
    // [resilience] 段示例与默认行为一致 —— cover_all_paths 默认
    // false（三条裸路径行为与历史逐位一致；Po2C selection 开关同款结构性断言）
    assert!(!config.resilience.cover_all_paths);
    // 回退观测头默认关（成功响应与现状逐字节一致）；示例必须带出键名
    assert!(!config.resilience.expose_fallback_headers);
    assert!(
        content.contains("expose_fallback_headers"),
        "config.example.toml 示例缺 expose_fallback_headers 键"
    );
    // 健康探针默认 off（零后台任务）；示例必须带出段与四键
    assert_eq!(config.health.mode, "off");
    assert_eq!(config.health.interval_secs, 300);
    assert_eq!(config.health.timeout_ms, 5000);
    assert_eq!(config.health.probe_strategy, "free");
    assert!(
        content.contains("[health]"),
        "config.example.toml 示例缺 [health] 段"
    );
    assert!(
        content.contains("probe_strategy"),
        "config.example.toml 示例缺 probe_strategy 键"
    );
    // 配额默认关（零判限）；示例必须带出段与三键 + keys 配额注释。
    // 余量响应头（expose_remaining_headers）为规格 Out of Scope，已移除——
    // 示例与结构体同步 absence（键回归即红）。
    assert!(!config.quota.enabled);
    assert_eq!(config.quota.window_secs, 3600);
    assert_eq!(config.quota.est_chars_per_token, 4);
    assert!(
        content.contains("[quota]"),
        "config.example.toml 示例缺 [quota] 段"
    );
    assert!(
        !content.contains("expose_remaining_headers"),
        "余量头特性已按规格移除，示例不得再出现该键"
    );
    assert!(
        content.contains("token_quota"),
        "config.example.toml 示例缺 token_quota 注释示例"
    );
    assert!(
        content.contains("cost_quota_millicents"),
        "config.example.toml 示例缺 cost_quota_millicents 注释示例"
    );
    // 失败率软降权默认关（gate=false → 路由权重与现状逐位
    // 一致；窗口计数纯观测），阈值参数与 LiteLLM 数值对照
    assert!(!config.resilience.failure_rate_gate);
    assert_eq!(config.resilience.failure_rate_min_samples, 5);
    assert!((config.resilience.failure_rate_threshold - 0.5).abs() < 1e-9);
    assert!((config.resilience.failure_rate_penalty - 0.1).abs() < 1e-9);
    assert_eq!(config.vendors[0].priority, 1);
    // 出站三层超时默认值（行为修复本体；0=对应层不限时的逃生阀
    // 约定见 config.example.toml [[vendors]] 注释）
    assert_eq!(config.vendors[0].timeout_ms, 60_000);
    assert_eq!(config.vendors[0].stream_ttft_ms, 60_000);
    assert_eq!(config.vendors[0].stream_idle_ms, 30_000);
    // 入站限流示例 rules 为空（结构性断言：空 = limiteron Governor
    // 内置默认兜底 + 无端点表，行为与历史版本逐位一致）
    assert!(config.rate_limit.rules.is_empty());
    match &config.vendors[0].rate_limit {
        fluxgate::config::RateLimitConfig::Token {
            max_tokens,
            window_hours,
            sliding,
        } => {
            assert_eq!(*max_tokens, 10000000);
            assert_eq!(*window_hours, Some(24));
            // 随仓 vendor 配置保持固定窗口（sliding 缺省 false =
            // 行为与现状逐位一致；滑动窗口为显式 opt-in，消除翻越点双花）
            assert!(!sliding, "随仓示例必须保持 fixed 窗口（sliding=false）");
        }
        other => panic!("expected Token rate limit, got {:?}", other),
    }
    // Interval 变体（若随仓出现）同样锁定 fixed 缺省
    for v in &config.vendors {
        if let fluxgate::config::RateLimitConfig::Interval { sliding, .. } = &v.rate_limit {
            assert!(!sliding, "随仓 Interval 配置必须保持 fixed 窗口");
        }
    }
    // TTL 负载联动默认关（纯静态 TTL 与现状逐位一致；Po2C/
    // admission 同款结构性断言），权重默认 0、类别表缺省空（全 1.0）
    assert!(!config.cache.ttl_load_linked);
    assert_eq!(config.cache.w_l, 0.0);
    assert_eq!(config.cache.w_q, 0.0);
    assert!((config.cache.beta_max - 1.5).abs() < 1e-9);
    assert!((config.cache.l_target - 1000.0).abs() < 1e-9);
    assert!((config.cache.q_target - 0.5).abs() < 1e-9);
    assert!(
        config.cache.categories.is_empty(),
        "随仓示例类别表必须缺省（全 1.0）"
    );
    assert!(
        content.contains("ttl_load_linked"),
        "config.example.toml 示例缺 ttl_load_linked 键"
    );
    assert!(
        content.contains("cache.categories"),
        "config.example.toml 示例缺 cache.categories 注释示例"
    );
    // token 加权票据默认关（单票据现状，行为逐位一致）
    assert!(!config.vendors[0].concurrency.token_weighted);
    assert_eq!(config.vendors[0].concurrency.token_ticket_min, 1);
    assert_eq!(config.vendors[0].concurrency.token_ticket_max, 16384);
    assert_eq!(config.vendors[0].concurrency.token_ticket_default, 4096);
    assert!(
        content.contains("token_weighted"),
        "config.example.toml 示例缺 token_weighted 注释"
    );
    // [grpc] 段：示例显式禁用（enabled=false 与历史行为逐位一致），键齐全
    //（serde default 会让缺席键静默过解析，示例缺席必须显性断言）
    let grpc = config
        .grpc
        .as_ref()
        .expect("config.example.toml 示例必须带 [grpc] 段");
    assert!(!grpc.enabled, "示例 [grpc] 默认禁用（显式 opt-in）");
    assert_eq!(grpc.host, "0.0.0.0");
    assert_eq!(grpc.port, 50051);
    assert_eq!(grpc.timeout_seconds, 30);
    assert_eq!(grpc.max_connections, 1000);
    assert_eq!(grpc.http2_keepalive_interval_secs, Some(30));
    assert_eq!(grpc.http2_keepalive_timeout_secs, Some(10));
    assert!(
        content.contains("http2_keepalive_interval_secs"),
        "config.example.toml 示例缺 http2_keepalive_interval_secs 键"
    );
    // ---- 全量参考面：Config 结构体的每个段与可选字段都必须在示例出现 ----
    // 示例是随仓唯一参考，缺席即示例过期
    // 显式锚点（键级清单此前漏项；全量兜底见
    // config_toml_example_covers_full_key_surface）：词法压缩 / 难度定档 /
    // 前缀亲和三段 + 价格因子口径 + chat 端点路径覆盖
    for key in [
        "[lexcompress]",
        "[routing.difficulty]",
        "[routing.prefix_affinity]",
        "price_factor_mode",
        "chat_path",
    ] {
        assert!(content.contains(key), "config.example.toml 示例缺 {key}");
    }
    // 难度定档新增子段：缺省词法表 = 内置默认，计分 = 历史常量
    assert!(config.routing.difficulty.lexicon.hard.is_none());
    assert_eq!(config.routing.difficulty.scoring.w_hard, 0.40);
    assert_eq!(config.routing.difficulty.scoring.length_cap, 8000.0);
    // [docs] 段：默认关闭（/api-docs 与 Swagger UI 路由不存在）
    assert!(
        !config.docs.enabled,
        "示例 [docs] 默认禁用（路由建站期消费）"
    );
    assert!(
        content.contains("[docs]"),
        "config.example.toml 缺 [docs] 段"
    );
    // [database].url：非空优先于 path（后端为编译期选择）
    assert!(
        content.contains("url = \"postgres://"),
        "config.example.toml 缺 database.url 注释示例"
    );
    // [[model_catalog]] 目录段（能力/定价查询输入，示例必须带出键名）
    assert!(
        content.contains("model_catalog"),
        "config.example.toml 缺 model_catalog 注释示例"
    );
    // 目录条目的长上下文价两面（cache_expected 模式下超阈值长 prompt 的输入价
    // 替代 + 触发阈值；0 = 不启用）——示例此前整块缺席
    assert!(
        content.contains("long_context_input_price"),
        "config.example.toml 缺 long_context_input_price 键"
    );
    assert!(
        content.contains("long_context_threshold_tokens"),
        "config.example.toml 缺 long_context_threshold_tokens 键"
    );
    // vendor 侧可选面：多 key 池 / 抽象模型名 / 并发上限 / 配额三形态
    assert!(
        content.contains("api_key_envs"),
        "config.example.toml 缺 api_key_envs 注释示例"
    );
    // 出站自定义头：默认空表（零行为变化），示例必须带出键名
    //（✅ 热生效——出站构造每请求现读；早期 restart-required 标注不实）
    assert!(
        config.vendors[0].extra_headers.is_empty(),
        "随仓示例 extra_headers 必须缺省空表"
    );
    assert!(
        content.contains("extra_headers"),
        "config.example.toml 缺 extra_headers 注释示例"
    );
    // D8 厂商三新键：anthropic_version 缺省 None（出站注入 2023-06-01）、
    // beta 转发与非标参数发送默认 false；示例注释带出键名与默认值
    //（anthropic_version 为 skip_serializing_if 字段，全量键面扫描不可见，
    // 须显式锚点）
    let anth = config
        .vendors
        .iter()
        .find(|v| v.type_ == "anthropic")
        .expect("随仓示例必须带 anthropic 厂商");
    assert_eq!(anth.anthropic_version, None, "示例缺省 None → 2023-06-01");
    assert!(!anth.forward_beta_headers, "示例默认关闭 beta 转发");
    assert!(!anth.send_nonstandard_params, "示例默认不发送非标参数");
    for key in [
        "anthropic_version",
        "forward_beta_headers",
        "send_nonstandard_params",
    ] {
        assert!(content.contains(key), "config.example.toml 缺 {key} 键");
    }
    assert!(
        content.contains("abstract_id"),
        "config.example.toml 缺 models.abstract_id 注释示例"
    );
    assert!(
        content.contains("max_concurrent_requests"),
        "config.example.toml 缺 concurrency.max_concurrent_requests 注释示例"
    );
    assert!(
        content.contains("window_hours"),
        "config.example.toml 缺 rate_limit.window_hours 键"
    );
    assert!(
        content.contains("sliding"),
        "config.example.toml 缺 rate_limit.sliding 键"
    );
    assert!(
        content.contains("type = \"interval\""),
        "config.example.toml 缺 interval 配额形态示例"
    );
    assert!(
        content.contains("type = \"time_window\""),
        "config.example.toml 缺 time_window 配额形态示例"
    );
    // [routing].price_factor_enabled 默认开启（目录单价参与加权路由）
    assert!(config.routing.price_factor_enabled);
    assert!(
        content.contains("price_factor_enabled"),
        "config.example.toml 缺 price_factor_enabled 键"
    );
    // [cache] 剩余两面：TTL 抖动默认 0（精确 TTL）、空响应负缓存默认 60s
    assert!((config.cache.ttl_jitter - 0.0).abs() < 1e-9);
    assert_eq!(config.cache.null_cache_ttl_secs, 60);
    assert!(
        content.contains("ttl_jitter"),
        "config.example.toml 缺 ttl_jitter 键"
    );
    assert!(
        content.contains("null_cache_ttl_secs"),
        "config.example.toml 缺 null_cache_ttl_secs 键"
    );
    // fallback 优先级显式带出（数值越小越优先）
    assert!(
        content.contains("priority"),
        "config.example.toml 缺 vendors.priority 键"
    );

    // 示例本身必须是可加载的配置：validate_config（加载与 POST /admin/config
    // 两路共用的校验面）必须放行——vendor/model/catalog id 去重、CB 窗口下限、
    // admin 摘要形态在此验证，不只看 serde 能解析
    fluxgate::config::validate_config(&config)
        .expect("config.example.toml 必须通过 validate_config");
}

/// 序列化面探针 TOML：显式置值 `skip_serializing_if` 字段（空 vec/空表/None
/// 时被跳过、示例解析面看不到）与空表/空 vec 段（auth.keys、rate_limit.rules、
/// cache.categories、model_catalog、pricing、routing.difficulty.tiers.mid、
/// 三种配额形态与全部 Option 段），使这些键名进入序列化面。
/// 新增此类「默认不进序列化面」的字段必须同步本探针——探针即该类的显式清单。
const SURFACE_PROBE_TOML: &str = r#"
[server]
host = "127.0.0.1"

[database]

[[vendors]]
id = "surface-probe-token"
name = "Surface Probe (token)"
type_ = "openai"
api_key_env = "SURFACE_PROBE_KEY"
api_key_envs = ["SURFACE_PROBE_KEY_2"]
extra_headers = { "x-surface-probe" = "1" }
rate_limit = { type = "token", max_tokens = 1 }
concurrency = {}

[[vendors.models]]
model_id = "surface-probe-model"

[[vendors]]
id = "surface-probe-interval"
name = "Surface Probe (interval)"
type_ = "openai"
api_key_env = "SURFACE_PROBE_KEY"
rate_limit = { type = "interval", hours = 1 }
concurrency = {}

[[vendors]]
id = "surface-probe-time-window"
name = "Surface Probe (time_window)"
type_ = "openai"
api_key_env = "SURFACE_PROBE_KEY"
rate_limit = { type = "time_window", start = "00:00", end = "23:59" }
concurrency = {}

[auth]
api_keys = []

[[auth.keys]]
hash = "0000000000000000000000000000000000000000000000000000000000000000"

[cors]

[global_model]

[grpc]

[[rate_limit.rules]]
id = "surface-probe-rule"
window_secs = 1
max_requests = 1

[[cache.categories]]
model_id_prefix = "surface-probe"
multiplier = 1.0

[[pricing]]
vendor_id = "surface-probe-token"
model_id = "surface-probe-model"
input_per_1k_tokens = 0.0
output_per_1k_tokens = 0.0

[[model_catalog]]
model_id = "surface-probe-catalog"
context_window = 1

[[model_catalog]]
model_id = "surface-probe-catalog-breakpoints"
context_window = 1

[model_catalog.cache_breakpoints]
enabled = true

[routing.difficulty.tiers]
low = "surface-probe-low"
mid = "surface-probe-mid"
high = "surface-probe-high"

[routing.difficulty.lexicon]
hard = ["surface-probe-hard"]
easy = ["surface-probe-easy"]
math_symbols = ["+"]
question_marks = ["?"]
code_fence_markers = ["```"]
"#;

/// 键名为「用户数据」而非配置字段的映射（`extra_headers` 的键是部署自定义头名，
/// 与本仓字段清单无关）：只登记字段名本身，不下钻其数据键。
const DATA_MAP_FIELDS: [&str; 1] = ["extra_headers"];

/// 递归采集序列化面键名：对象逐键登记后下钻，数组逐元素下钻（标量无键）。
fn collect_key_names(value: &serde_json::Value, keys: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                keys.insert(key.clone());
                if !DATA_MAP_FIELDS.contains(&key.as_str()) {
                    collect_key_names(child, keys);
                }
            }
        }
        serde_json::Value::Array(items) => {
            items.iter().for_each(|item| collect_key_names(item, keys));
        }
        _ => {}
    }
}

/// 「全量参考面」的机械化执行：Config 序列化面的每个键名都必须在示例文本
/// 出现（含注释示例），缺席即示例过期——一次列出全部缺席键，而非首个。
/// 序列化面取「示例解析面 ∪ 探针面」：前者覆盖示例已带出的段（新段落进示例
/// 即自动纳入），后者补齐 skip_serializing_if 字段与空段（示例整段缺席也能测出）。
#[test]
fn config_toml_example_covers_full_key_surface() {
    let content = std::fs::read_to_string("config.example.toml").unwrap();
    let example: fluxgate::config::Config = toml::from_str(&content).unwrap();
    let probe: fluxgate::config::Config =
        toml::from_str(SURFACE_PROBE_TOML).expect("序列化面探针必须是可解析的配置");
    let mut keys = std::collections::BTreeSet::new();
    for config in [&example, &probe] {
        collect_key_names(&serde_json::to_value(config).unwrap(), &mut keys);
    }
    // 反向断言：探针自身的键必须都落在结构体序列化面——拼错/错位的键会被
    // serde 静默忽略、静默缩小探针覆盖，反向断言使这种漂移变红
    let probe_toml: toml::Value = toml::from_str(SURFACE_PROBE_TOML).unwrap();
    let mut probe_keys = std::collections::BTreeSet::new();
    collect_key_names(&serde_json::to_value(&probe_toml).unwrap(), &mut probe_keys);
    let unknown: Vec<&str> = probe_keys
        .iter()
        .map(String::as_str)
        .filter(|key| !keys.contains(*key))
        .collect();
    assert!(
        unknown.is_empty(),
        "序列化面探针含结构体未知键（拼写/错位即探针覆盖失效）：{unknown:?}"
    );
    let missing: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|key| !content.contains(key))
        .collect();
    assert!(
        missing.is_empty(),
        "config.example.toml 缺 Config 序列化面键名（示例过期，须同步示例）：{missing:?}"
    );
}
