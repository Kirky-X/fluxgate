// 快速验证：config.toml 可被 nexusflow::config::Config 反序列化
#[test]
fn config_toml_example_parses() {
    let content = std::fs::read_to_string("config.toml").unwrap();
    let config: nexusflow::config::Config = toml::from_str(&content).unwrap();
    assert_eq!(config.retry.max_retries, 3);
    assert_eq!(config.retry.max_total_attempts, 8);
    // recId=5：同步惊群展宽默认全开（1.0）；回滚阀 = 0（见 config.toml 注释）
    assert!((config.retry.spread_jitter - 1.0).abs() < 1e-9);
    // recId=9（W5）：重试时间预算默认 0 = 不限时、放大守门器默认 false
    //（双开关默认 → 单 vendor 重试与跨 vendor fallback 行为与现状逐位一致）
    assert_eq!(config.retry.retry_budget_secs, 0);
    assert!(!config.retry.amplification_gate);
    // 示例文件必须显式带出两个新键（serde #[serde(default)] 会让缺席键
    // 静默过解析，解析断言测不出示例缺席）
    assert!(
        content.contains("retry_budget_secs"),
        "config.toml 示例缺 retry_budget_secs 键"
    );
    assert!(
        content.contains("amplification_gate"),
        "config.toml 示例缺 amplification_gate 键"
    );
    // recId=10：[routing] 段示例与默认行为一致（3D 权重、新维度全关）
    assert_eq!(config.routing.w_model, 0.4);
    assert_eq!(config.routing.w_queue, 0.3);
    assert_eq!(config.routing.w_window, 0.3);
    assert_eq!(config.routing.w_latency, 0.0);
    assert_eq!(config.routing.w_error, 0.0);
    assert_eq!(config.routing.selection, "weighted");
    // recId=6：负载信号档位默认 "queue"（Po2C 同款结构性锁定——默认路径
    // 行为与历史逐位一致）
    assert_eq!(config.routing.load_signal, "queue");
    // recId=8：Peak-EWMA τ 默认 10s（Kong 同值）；min+buffer 候选集默认
    // 0.0 关闭（结构性锁定：轮盘赌/Po2C 与历史逐位一致）
    assert_eq!(config.routing.ewma_tau_ms, 10_000);
    assert_eq!(config.routing.latency_buffer, 0.0);
    // recId=19：冷却/候选集判定改进包三开关默认关（请求级权重头忽略、
    // 全冷却保持 Validation 错误、耗尽原错误冒泡——均与现状逐位一致）
    assert!(!config.routing.allow_request_weight);
    assert!(!config.routing.fail_open_when_all_cooling);
    assert!(!config.routing.retry_after_hint);
    // 示例文件必须显式带出三个新键（serde 缺省会让缺席键静默过解析）
    assert!(
        content.contains("allow_request_weight"),
        "config.toml 示例缺 allow_request_weight 键"
    );
    assert!(
        content.contains("fail_open_when_all_cooling"),
        "config.toml 示例缺 fail_open_when_all_cooling 键"
    );
    assert!(
        content.contains("retry_after_hint"),
        "config.toml 示例缺 retry_after_hint 键"
    );
    // recId=10：亲和软化三参数默认（hard 模式 = 现状逐位一致、boost 基准
    // 1.0 仅 soft 模式生效、租户隔离关 = legacy 键格式）
    assert_eq!(config.routing.affinity_mode, "hard");
    assert!((config.routing.affinity_boost_factor - 1.0).abs() < 1e-9);
    assert!(!config.routing.affinity_tenant_scoped);
    assert!(
        content.contains("affinity_mode"),
        "config.toml 示例缺 affinity_mode 键"
    );
    assert!(
        content.contains("affinity_boost_factor"),
        "config.toml 示例缺 affinity_boost_factor 键"
    );
    assert!(
        content.contains("affinity_tenant_scoped"),
        "config.toml 示例缺 affinity_tenant_scoped 键"
    );
    // recId=14：SLA 虚拟队列打分项默认关（sla_v=0.0 → 惩罚因子不参与权重、
    // Q 不更新，路由行为与现状逐位一致；Po2C/latency_buffer 同款结构性
    // 锁定），sla_alpha 默认 0.95（目标服务水平）
    assert_eq!(config.routing.sla_v, 0.0);
    assert!((config.routing.sla_alpha - 0.95).abs() < 1e-9);
    assert!(content.contains("sla_v"), "config.toml 示例缺 sla_v 键");
    assert!(
        content.contains("sla_alpha"),
        "config.toml 示例缺 sla_alpha 键"
    );
    // recId=15：selection 默认 "weighted"（上方已有断言——LinUCB 第三档
    // 不被咨询、无状态更新，Po2C 开关同款结构性锁定）；ucb_alpha 默认 1.0
    //（仅 linucb 档被咨询）
    assert!((config.routing.ucb_alpha - 1.0).abs() < 1e-9);
    assert!(
        content.contains("ucb_alpha"),
        "config.toml 示例缺 ucb_alpha 键"
    );
    // recId=16：[admission] 段默认关（enabled=false → 判定旁路、EWMA 不
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
        "config.toml 示例缺 congestion_threshold 键"
    );
    assert!(
        content.contains("auth.keys"),
        "config.toml 示例缺 auth.keys 注释示例"
    );
    // recId=17：LPS 前瞻默认关（lps_delta=0.0 → Ŵ 不参与权重、
    // lps_deadline_gate=false → 不消费 deadline，行为与现状逐位一致）
    assert_eq!(config.routing.lps_delta, 0.0);
    assert!(!config.routing.lps_deadline_gate);
    assert!((config.routing.default_deadline_ms - 10000.0).abs() < 1e-9);
    assert!(
        content.contains("lps_delta"),
        "config.toml 示例缺 lps_delta 键"
    );
    assert!(
        content.contains("lps_deadline_gate"),
        "config.toml 示例缺 lps_deadline_gate 键"
    );
    // recId=2：[resilience] 段示例与默认行为一致 —— cover_all_paths 默认
    // false（三条裸路径行为与历史逐位一致；Po2C selection 开关同款结构性断言）
    assert!(!config.resilience.cover_all_paths);
    // recId=7（W5）：失败率软降权默认关（gate=false → 路由权重与现状逐位
    // 一致；窗口计数纯观测），阈值参数与 LiteLLM 数值对照
    assert!(!config.resilience.failure_rate_gate);
    assert_eq!(config.resilience.failure_rate_min_samples, 5);
    assert!((config.resilience.failure_rate_threshold - 0.5).abs() < 1e-9);
    assert!((config.resilience.failure_rate_penalty - 0.1).abs() < 1e-9);
    assert_eq!(config.vendors[0].priority, 1);
    // recId=4：出站三层超时默认值（行为修复本体；0=对应层不限时的逃生阀
    // 约定见 config.toml [[vendors]] 注释）
    assert_eq!(config.vendors[0].timeout_ms, 60_000);
    assert_eq!(config.vendors[0].stream_ttft_ms, 60_000);
    assert_eq!(config.vendors[0].stream_idle_ms, 30_000);
    // recId=3：入站限流示例 rules 为空（结构性断言：空 = limiteron Governor
    // 内置默认兜底 + 无端点表，行为与历史版本逐位一致）
    assert!(config.rate_limit.rules.is_empty());
    match &config.vendors[0].rate_limit {
        nexusflow::config::RateLimitConfig::Token {
            max_tokens,
            window_hours,
            sliding,
        } => {
            assert_eq!(*max_tokens, 10000000);
            assert_eq!(*window_hours, Some(24));
            // recId=11：随仓 vendor 配置保持固定窗口（sliding 缺省 false =
            // 行为与现状逐位一致；滑动窗口为显式 opt-in，消除翻越点双花）
            assert!(!sliding, "随仓示例必须保持 fixed 窗口（sliding=false）");
        }
        other => panic!("expected Token rate limit, got {:?}", other),
    }
    // Interval 变体（若随仓出现）同样锁定 fixed 缺省
    for v in &config.vendors {
        if let nexusflow::config::RateLimitConfig::Interval { sliding, .. } = &v.rate_limit {
            assert!(!sliding, "随仓 Interval 配置必须保持 fixed 窗口");
        }
    }
    // recId=12：TTL 负载联动默认关（纯静态 TTL 与现状逐位一致；Po2C/
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
        "config.toml 示例缺 ttl_load_linked 键"
    );
    assert!(
        content.contains("cache.categories"),
        "config.toml 示例缺 cache.categories 注释示例"
    );
    // recId=18：token 加权票据默认关（单票据现状，行为逐位一致）
    assert!(!config.vendors[0].concurrency.token_weighted);
    assert_eq!(config.vendors[0].concurrency.token_ticket_min, 1);
    assert_eq!(config.vendors[0].concurrency.token_ticket_max, 16384);
    assert_eq!(config.vendors[0].concurrency.token_ticket_default, 4096);
    assert!(
        content.contains("token_weighted"),
        "config.toml 示例缺 token_weighted 注释"
    );
    // [grpc] 段：示例显式禁用（enabled=false 与历史行为逐位一致），键齐全
    //（serde default 会让缺席键静默过解析，示例缺席必须显性断言）
    let grpc = config
        .grpc
        .as_ref()
        .expect("config.toml 示例必须带 [grpc] 段");
    assert!(!grpc.enabled, "示例 [grpc] 默认禁用（显式 opt-in）");
    assert_eq!(grpc.host, "0.0.0.0");
    assert_eq!(grpc.port, 50051);
    assert_eq!(grpc.timeout_seconds, 30);
    assert_eq!(grpc.max_connections, 1000);
    assert_eq!(grpc.http2_keepalive_interval_secs, Some(30));
    assert_eq!(grpc.http2_keepalive_timeout_secs, Some(10));
    assert!(
        content.contains("http2_keepalive_interval_secs"),
        "config.toml 示例缺 http2_keepalive_interval_secs 键"
    );
    // 最小示例 config.example.toml 同样必须带 [grpc] 段
    let example = std::fs::read_to_string("config.example.toml").unwrap();
    let example_config: nexusflow::config::Config = toml::from_str(&example).unwrap();
    let example_grpc = example_config
        .grpc
        .as_ref()
        .expect("config.example.toml 必须带 [grpc] 段");
    assert!(!example_grpc.enabled);
    assert_eq!(example_grpc.port, 50051);
}
