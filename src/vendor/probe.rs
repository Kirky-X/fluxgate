// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 主动健康检查探针：周期探测 suspect/all vendor，成功被动恢复（清除
//! 冷却/黑名单；失败保持现状仅记 debug）。
//!
//! mode/interval/strategy 每周期热读（默认 off 时该周期 no-op）；探测
//! 出站与正常请求共用 RPS/并发治理与出站 Client。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{ConfigManager, VendorConfig};
use crate::i18n::{t, t_args};
use crate::vendor::{VendorManager, VendorStatus};

pub const PROBE_CONCURRENCY: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeMode {
    Off,
    SuspectOnly,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeStrategy {
    Free,
    MinimalCall,
}

fn normalize_mode(raw: &str) -> ProbeMode {
    match raw.trim() {
        "suspect_only" => ProbeMode::SuspectOnly,
        "all" => ProbeMode::All,
        "off" => ProbeMode::Off,
        other => {
            tracing::warn!(mode = %other, "{}", t("vendor-probe-unknown-mode"));
            ProbeMode::Off
        }
    }
}

fn normalize_strategy(raw: &str) -> ProbeStrategy {
    match raw.trim() {
        "minimal_call" => ProbeStrategy::MinimalCall,
        "free" => ProbeStrategy::Free,
        other => {
            tracing::warn!(strategy = %other, "{}", t("vendor-probe-unknown-strategy"));
            ProbeStrategy::Free
        }
    }
}

/// 单周期报告：处理的目标数 / 执行恢复动作数（成功且对象曾为 suspect）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProbeReport {
    pub probed: usize,
    pub recovered: usize,
}

/// 单周期执行（可测入口）：采集目标 → 并发 ≤2 探测 → 成功恢复。
pub async fn run_probe_cycle(
    vendor_manager: &Arc<VendorManager>,
    config_manager: &Arc<ConfigManager>,
) -> ProbeReport {
    let health = config_manager.read().health.clone();
    let mode = normalize_mode(&health.mode);
    if mode == ProbeMode::Off {
        return ProbeReport::default();
    }
    let strategy = normalize_strategy(&health.probe_strategy);
    let timeout = (health.timeout_ms > 0).then(|| Duration::from_millis(health.timeout_ms));
    // 出站 Client 复用：与正常请求同一共享 Client（连接池/超时基线一致）
    let client = vendor_manager.outbound_client();
    let targets = vendor_manager
        .probe_targets(mode == ProbeMode::SuspectOnly)
        .await;

    let semaphore = Arc::new(tokio::sync::Semaphore::new(PROBE_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for target in targets {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .unwrap_or_else(|_| panic!("{}", t("vendor-probe-semaphore-closed")));
        let manager = vendor_manager.clone();
        let client = client.clone();
        set.spawn(async move {
            let _permit = permit;
            // 探测计入 vendor 出站限流/并发；实例缺失 → 跳过该目标
            let Some(_capacity) = manager.acquire_probe_capacity(&target.vendor_id).await else {
                tracing::debug!(
                    vendor = %target.vendor_id,
                    "{}",
                    t("vendor-probe-target-missing")
                );
                return (0, 0);
            };
            probe_one(&manager, &client, &target, strategy, timeout).await
        });
    }
    let mut report = ProbeReport::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((probed, recovered)) => {
                report.probed += probed;
                report.recovered += recovered;
            }
            Err(e) => tracing::debug!(
                "{}",
                t_args("vendor-probe-task-error", &[("error", e.to_string())])
            ),
        }
    }
    report
}

/// 后台循环（bootstrap 常驻 spawn；abort 即停）：首周期延迟一个 interval，interval/mode/strategy 每轮热读。
pub async fn run_probe_loop(manager: Arc<VendorManager>, config: Arc<ConfigManager>) {
    loop {
        let secs = config.read().health.interval_secs.max(1);
        tokio::time::sleep(Duration::from_secs(secs)).await;
        let report = run_probe_cycle(&manager, &config).await;
        tracing::debug!(
            probed = report.probed,
            recovered = report.recovered,
            "{}",
            t("vendor-probe-cycle-done")
        );
    }
}

fn auth_headers(config: &VendorConfig, key: &str) -> Vec<(String, String)> {
    if config.type_ == "gemini" {
        vec![("x-goog-api-key".to_string(), key.to_string())]
    } else if config.type_ == "anthropic" {
        vec![
            ("x-api-key".to_string(), key.to_string()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ]
    } else {
        vec![("authorization".to_string(), format!("Bearer {key}"))]
    }
}

fn minimal_body(config: &VendorConfig) -> Value {
    let model = config
        .models
        .iter()
        .find(|m| m.enabled)
        .map(|m| m.model_id.clone())
        .unwrap_or_else(|| "probe".to_string());
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
    })
}

/// Responses 最小探测体：1 条 input（`store=false` 不落上游状态）。
fn minimal_responses_body(config: &VendorConfig) -> Value {
    let model = config
        .models
        .iter()
        .find(|m| m.enabled)
        .map(|m| m.model_id.clone())
        .unwrap_or_else(|| "probe".to_string());
    json!({
        "model": model,
        "input": "ping",
        "store": false,
    })
}

/// Gemini 最小探测体：1 条 user text + `maxOutputTokens=1`。
fn minimal_gemini_body(_config: &VendorConfig) -> Value {
    json!({
        "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
        "generationConfig": {"maxOutputTokens": 1},
    })
}

/// rerank 最小探测体：1 文档 + `top_n=1`（与 chat minimal_call 同量级的
/// 最小计费调用，不引入新的计价面）
fn minimal_rerank_body(config: &VendorConfig) -> Value {
    let model = config
        .models
        .iter()
        .find(|m| m.enabled)
        .map(|m| m.model_id.clone())
        .unwrap_or_else(|| "probe".to_string());
    json!({
        "model": model,
        "query": "ping",
        "documents": ["ping"],
        "top_n": 1,
    })
}

/// 单对象探测 → (probed, recovered)。`recovered` 仅当对象曾为 suspect 且成功。
async fn probe_one(
    manager: &Arc<VendorManager>,
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    strategy: ProbeStrategy,
    timeout: Option<Duration>,
) -> (usize, usize) {
    let suspect = target.status != VendorStatus::Available || !target.blacklist.is_empty();
    // rerank 厂商：free（GET models）与 minimal_call（chat）均不适用，
    // 走专用最小重排探测（1 文档，2xx 即恢复）
    if target.config.type_ == "rerank" {
        let recovered = probe_rerank_call(manager, client, target, timeout, suspect).await;
        return (1, recovered);
    }
    match strategy {
        ProbeStrategy::Free => {
            // probed 恒 1（目标已处理，HTTP 未发出同样计入）
            let success = probe_free(client, target, timeout).await;
            if success && suspect {
                manager.mark_available(&target.vendor_id);
                tracing::info!(
                    vendor = %target.vendor_id,
                    "{}",
                    t("vendor-probe-free-recovered")
                );
                (1, 1)
            } else {
                (1, 0)
            }
        }
        ProbeStrategy::MinimalCall => {
            // 非 suspect 仅验证连通性，不做恢复动作（无状态可恢复）
            let recovered = probe_minimal_call(manager, client, target, timeout, suspect).await;
            (1, recovered)
        }
    }
}

/// free 探测：零成本 GET 上游模型列表端点。2xx 即成功（不验证 key）。
async fn probe_free(
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    timeout: Option<Duration>,
) -> bool {
    let url = match VendorManager::build_vendor_url_at(&target.config, "models") {
        Ok(url) => url,
        Err(e) => {
            tracing::debug!(
                vendor = %target.vendor_id,
                error = %e,
                "{}",
                t("vendor-probe-url-build-failed")
            );
            return false;
        }
    };
    let mut request = client.get(&url);
    // 出站凭证：当前游标 key；池空/legacy 现读 env（缺失则无认证头）
    let credential: Option<(String, String)> = match &target.current {
        Some(pair) => Some(pair.clone()),
        None => std::env::var(&target.config.api_key_env)
            .ok()
            .map(|key| (key, String::new())),
    };
    if let Some((key, _)) = credential.as_ref() {
        for (name, value) in auth_headers(&target.config, key) {
            request = request.header(name, value);
        }
    }
    if let Some(duration) = timeout {
        request = request.timeout(duration);
    }
    match request.send().await {
        Ok(response) if response.status().is_success() => true,
        Ok(response) => {
            tracing::debug!(
                vendor = %target.vendor_id,
                status = %response.status(),
                "{}",
                t("vendor-probe-free-non-2xx")
            );
            false
        }
        Err(e) => {
            tracing::debug!(
                vendor = %target.vendor_id,
                error = %e,
                "{}",
                t("vendor-probe-free-failed")
            );
            false
        }
    }
}

/// minimal_call 探测：对黑名单 key + 当前 key 发 max_tokens=1 计费请求；
/// `recover` 且逐 key 成功即恢复（解黑名单 + 清冷却 + 重置模型级熔断计数）。
async fn probe_minimal_call(
    manager: &Arc<VendorManager>,
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    timeout: Option<Duration>,
    recover: bool,
) -> usize {
    let (url, body) = if target.config.is_responses_style() {
        (
            VendorManager::build_vendor_url_at(&target.config, "responses"),
            minimal_responses_body(&target.config),
        )
    } else if target.config.type_ == "gemini" {
        let model = target
            .config
            .models
            .iter()
            .find(|m| m.enabled)
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| "probe".to_string());
        (
            VendorManager::build_vendor_url_for_model(
                &target.config,
                &model,
                false,
                "generateContent",
            ),
            minimal_gemini_body(&target.config),
        )
    } else {
        (
            VendorManager::build_vendor_url(&target.config),
            minimal_body(&target.config),
        )
    };
    let url = match url {
        Ok(url) => url,
        Err(e) => {
            tracing::debug!(
                vendor = %target.vendor_id,
                error = %e,
                "{}",
                t("vendor-probe-url-build-failed")
            );
            return 0;
        }
    };
    probe_measured_call(manager, client, target, timeout, recover, &url, &body).await
}

/// rerank 专用探测：最小重排请求（1 文档）替代 chat minimal_call；
/// 恢复语义与 minimal_call 完全一致（解黑名单 + 清冷却 + 重置模型熔断）
async fn probe_rerank_call(
    manager: &Arc<VendorManager>,
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    timeout: Option<Duration>,
    recover: bool,
) -> usize {
    let url = match VendorManager::build_vendor_url_at(&target.config, "rerank") {
        Ok(url) => url,
        Err(e) => {
            tracing::debug!(
                vendor = %target.vendor_id,
                error = %e,
                "{}",
                t("vendor-probe-url-build-failed")
            );
            return 0;
        }
    };
    let body = minimal_rerank_body(&target.config);
    probe_measured_call(manager, client, target, timeout, recover, &url, &body).await
}

/// 最小计费探测共用体：对黑名单 key + 当前 key 逐个发请求，2xx 且 `recover`
/// 即 `probe_recover_key`；返回实际恢复的 key 数（无候选→ 0）。
async fn probe_measured_call(
    manager: &Arc<VendorManager>,
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    timeout: Option<Duration>,
    recover: bool,
    url: &str,
    body: &Value,
) -> usize {
    // 候选 = 黑名单 key（按指纹回查明文）+ 当前 key（去重）。
    let mut candidates: Vec<(String, String)> = Vec::new();
    for fp in &target.blacklist {
        if let Some(pair) = target.keys.iter().find(|(_, f)| f == fp) {
            candidates.push(pair.clone());
        }
    }
    if let Some(current) = &target.current {
        if !candidates.iter().any(|(_, f)| f == &current.1) {
            candidates.push(current.clone());
        }
    }
    if candidates.is_empty() {
        // 无池/黑名单：legacy env 凭证（空指纹；成功只恢复 vendor 可用态，
        // 不解黑名单——无指纹可解）
        if let Ok(key) = std::env::var(&target.config.api_key_env) {
            candidates.push((key, String::new()));
        }
    }
    let mut recovered = 0usize;
    for (key, fingerprint) in candidates {
        let mut request = client.post(url).json(body);
        for (name, value) in auth_headers(&target.config, &key) {
            request = request.header(name, value);
        }
        if let Some(duration) = timeout {
            request = request.timeout(duration);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                if recover {
                    if fingerprint.is_empty() {
                        // env 凭证无池指纹：仅恢复 vendor 级可用态
                        manager.mark_available(&target.vendor_id);
                    } else {
                        manager
                            .probe_recover_key(&target.vendor_id, &fingerprint)
                            .await;
                    }
                    tracing::info!(
                        vendor = %target.vendor_id,
                        "{}",
                        t("vendor-probe-minimal-recovered")
                    );
                    recovered += 1;
                }
            }
            Ok(response) => {
                tracing::debug!(
                    vendor = %target.vendor_id,
                    status = %response.status(),
                    "{}",
                    t("vendor-probe-minimal-non-2xx")
                );
            }
            Err(e) => {
                tracing::debug!(
                    vendor = %target.vendor_id,
                    error = %e,
                    "{}",
                    t("vendor-probe-minimal-failed")
                );
            }
        }
    }
    recovered
}

#[cfg(test)]
mod tests {
    //! 探针单元测试：归一化/构造纯函数直测；HTTP 探测经 mockito 本地桩；
    //! Manager 空实例装配（恢复动作对未知 vendor 为 no-op，断言聚焦报告值）。

    use super::*;
    use crate::vendor::manager::ProbeTarget;
    use std::path::Path;

    fn probe_config(api_base: &str, type_: &str) -> VendorConfig {
        VendorConfig {
            id: "v-probe".into(),
            name: "Probe".into(),
            type_: type_.into(),
            api_key_env: "PROBE_UNUSED_ENV".into(),
            api_key_envs: vec![],
            extra_headers: Default::default(),
            api_base: api_base.into(),
            enabled: true,
            priority: 1,
            models: vec![],
            rate_limit: crate::config::RateLimitConfig::Token {
                max_tokens: 1000,
                window_hours: None,
                sliding: false,
            },
            concurrency: crate::config::ConcurrencyConfig::default(),
            timeout_ms: 60_000,
            stream_ttft_ms: 60_000,
            stream_idle_ms: 30_000,
            supports_batch: false,
            chat_path: None,
            api_style: None,
        }
    }

    fn model(model_id: &str, enabled: bool) -> crate::config::ModelConfig {
        crate::config::ModelConfig {
            model_id: model_id.into(),
            abstract_id: None,
            weight: 1.0,
            enabled,

            dimensions: None,
            compat_group: None,
            model_type: None,
        }
    }

    fn target(config: VendorConfig, current: Option<(String, String)>) -> ProbeTarget {
        ProbeTarget {
            vendor_id: "v-probe".into(),
            config,
            status: VendorStatus::Unavailable,
            keys: Vec::new(),
            blacklist: Vec::new(),
            current,
        }
    }

    fn empty_manager() -> Arc<VendorManager> {
        let toml = "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 0\n\n\
                    [database]\npath = \":memory:\"\n";
        let config = crate::config::loader::parse_config_content(Path::new("config.toml"), toml)
            .expect("探针测试基线配置必须合法");
        Arc::new(VendorManager::new(Arc::new(ConfigManager::new(config))))
    }

    fn cycle_manager(mode: &str) -> Arc<ConfigManager> {
        let toml = format!(
            "vendors = []\n[server]\nhost = \"127.0.0.1\"\nport = 0\n\n\
             [database]
path = \":memory:\"

[health]
mode = \"{mode}\"
"
        );
        let config = crate::config::loader::parse_config_content(Path::new("config.toml"), &toml)
            .expect("探针周期测试配置必须合法");
        Arc::new(ConfigManager::new(config))
    }

    // ---- 归一化与构造纯函数 ----

    #[test]
    fn normalize_mode_and_strategy_fall_back_on_unknown_values() {
        assert_eq!(normalize_mode("suspect_only"), ProbeMode::SuspectOnly);
        assert_eq!(normalize_mode(" all "), ProbeMode::All);
        assert_eq!(normalize_mode("off"), ProbeMode::Off);
        assert_eq!(normalize_mode("yolo"), ProbeMode::Off, "未知 mode 回退 Off");
        assert_eq!(
            normalize_strategy("minimal_call"),
            ProbeStrategy::MinimalCall
        );
        assert_eq!(normalize_strategy(" free "), ProbeStrategy::Free);
        assert_eq!(
            normalize_strategy("bogus"),
            ProbeStrategy::Free,
            "未知 strategy 回退 Free"
        );
    }

    #[test]
    fn auth_headers_follow_vendor_type() {
        let anthropic = probe_config("", "anthropic");
        assert_eq!(
            auth_headers(&anthropic, "k-1"),
            vec![
                ("x-api-key".to_string(), "k-1".to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ]
        );
        let openai = probe_config("", "openai");
        assert_eq!(
            auth_headers(&openai, "k-1"),
            vec![("authorization".to_string(), "Bearer k-1".to_string())]
        );
        let gemini = probe_config("", "gemini");
        assert_eq!(
            auth_headers(&gemini, "k-1"),
            vec![("x-goog-api-key".to_string(), "k-1".to_string())]
        );
    }

    #[test]
    fn minimal_body_uses_enabled_model_or_probe_placeholder() {
        let mut config = probe_config("", "openai");
        config.models = vec![model("m-disabled", false), model("m-live", true)];
        let body = minimal_body(&config);
        assert_eq!(body["model"], "m-live", "取首个 enabled 模型");
        assert_eq!(body["max_tokens"], 1, "minimal_call 计费下限");
        config.models = vec![model("m-off", false)];
        assert_eq!(
            minimal_body(&config)["model"],
            "probe",
            "无 enabled 模型回退占位名"
        );
    }

    #[test]
    fn minimal_responses_body_uses_enabled_model_and_store_false() {
        let mut config = probe_config("http://127.0.0.1:1", "openai");
        config.api_style = Some("responses".into());
        config.models = vec![model("m-disabled", false), model("m-live", true)];
        let body = minimal_responses_body(&config);
        assert_eq!(body["model"], "m-live");
        assert_eq!(body["input"], "ping");
        assert_eq!(body["store"], false);
    }

    #[test]
    fn minimal_gemini_body_has_single_turn_and_budget() {
        let body = minimal_gemini_body(&probe_config("", "gemini"));
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "ping");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1);
        assert!(body.get("model").is_none(), "Gemini body 不含 model");
    }

    // ---- probe_free：2xx / 非 2xx / 连接失败 / URL 构建失败 / env 凭证回退 ----

    #[tokio::test]
    async fn probe_free_2xx_with_current_key_header_succeeds() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer k-current")
            .with_status(200)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let config = probe_config(&server.url(), "openai");
        let t = target(config, Some(("k-current".into(), "fp-x".into())));
        assert!(
            probe_free(&client, &t, Some(Duration::from_millis(2000))).await,
            "2xx 即成功"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn probe_free_env_credential_fallback_when_current_missing() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/v1/models")
            .match_header("authorization", "Bearer probe-fake-credential-1")
            .with_status(200)
            .create();
        std::env::set_var("PROBE_TEST_FREE_ENV_XYZ", "probe-fake-credential-1");
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut config = probe_config(&server.url(), "openai");
        config.api_key_env = "PROBE_TEST_FREE_ENV_XYZ".into();
        let t = target(config, None);
        assert!(
            probe_free(&client, &t, None).await,
            "current 缺失现读 env 凭证"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn probe_free_non_2xx_fails() {
        let mut server = mockito::Server::new_async().await;
        server.mock("GET", "/v1/models").with_status(401).create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let t = target(
            probe_config(&server.url(), "openai"),
            Some(("k".into(), "fp".into())),
        );
        assert!(!probe_free(&client, &t, None).await, "非 2xx 失败");
    }

    #[tokio::test]
    async fn probe_free_connection_failure_fails() {
        let manager = empty_manager();
        let client = manager.outbound_client();
        // 127.0.0.1:1 连接拒绝（RST 快速失败，不依赖外部网络）
        let t = target(
            probe_config("http://127.0.0.1:1", "openai"),
            Some(("k".into(), "fp".into())),
        );
        assert!(!probe_free(&client, &t, None).await, "连接失败失败");
    }

    #[test]
    fn probe_free_url_build_failure_fails() {
        // 空链 unknown type：官方端点回退表无此类型 → URL 构建失败
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let manager = empty_manager();
            let client = manager.outbound_client();
            let t = target(probe_config("", "weird-type"), None);
            assert!(!probe_free(&client, &t, None).await, "URL 构建失败失败");
        });
    }

    // ---- probe_minimal_call：候选装配 / 恢复 / 非 2xx / 失败 ----

    #[tokio::test]
    async fn probe_minimal_call_recovers_blacklisted_and_current_keys_on_2xx() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .expect(2)
            .with_status(200)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut t = target(
            probe_config(&server.url(), "openai"),
            Some((
                "k-good".into(),
                crate::vendor::key_pool::fingerprint_of("k-good"),
            )),
        );
        t.keys = vec![(
            "k-bad".into(),
            crate::vendor::key_pool::fingerprint_of("k-bad"),
        )];
        t.blacklist = vec![crate::vendor::key_pool::fingerprint_of("k-bad")];
        let recovered = probe_minimal_call(
            &manager,
            &client,
            &t,
            Some(Duration::from_millis(2000)),
            true,
        )
        .await;
        assert_eq!(
            recovered, 2,
            "黑名单 key + 当前 key 全部 2xx → 逐 key 计恢复"
        );
        mock.assert();
    }

    #[tokio::test]
    async fn probe_minimal_call_non_2xx_does_not_recover() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let t = target(
            probe_config(&server.url(), "openai"),
            Some(("k".into(), crate::vendor::key_pool::fingerprint_of("k"))),
        );
        let recovered = probe_minimal_call(&manager, &client, &t, None, true).await;
        assert_eq!(recovered, 0, "非 2xx 不恢复");
    }

    #[tokio::test]
    async fn probe_minimal_call_connection_failure_yields_zero() {
        let manager = empty_manager();
        let client = manager.outbound_client();
        let t = target(
            probe_config("http://127.0.0.1:1", "openai"),
            Some(("k".into(), "fp".into())),
        );
        assert_eq!(
            probe_minimal_call(&manager, &client, &t, None, true).await,
            0,
            "连接失败不恢复"
        );
    }

    #[test]
    fn probe_minimal_call_url_build_failure_yields_zero() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let manager = empty_manager();
            let client = manager.outbound_client();
            let t = target(probe_config("", "weird-type"), None);
            assert_eq!(
                probe_minimal_call(&manager, &client, &t, None, true).await,
                0,
                "URL 构建失败不恢复"
            );
        });
    }

    // ---- probe_one：suspect 判定与恢复门槛 ----

    #[tokio::test]
    async fn probe_one_free_recovers_only_suspect_targets() {
        let mut server = mockito::Server::new_async().await;
        server.mock("GET", "/v1/models").with_status(200).create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let suspect = target(
            probe_config(&server.url(), "openai"),
            Some(("k".into(), "fp".into())),
        );
        assert_eq!(
            probe_one(&manager, &client, &suspect, ProbeStrategy::Free, None).await,
            (1, 1),
            "suspect 且探测成功 → 恢复计数 1"
        );
        let mut healthy = target(
            probe_config(&server.url(), "openai"),
            Some(("k".into(), "fp".into())),
        );
        healthy.status = VendorStatus::Available;
        assert_eq!(
            probe_one(&manager, &client, &healthy, ProbeStrategy::Free, None).await,
            (1, 0),
            "非 suspect 无状态可恢复"
        );
    }

    #[tokio::test]
    async fn probe_one_minimal_call_on_healthy_target_skips_recovery() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut healthy = target(
            probe_config(&server.url(), "openai"),
            Some(("k".into(), "fp".into())),
        );
        healthy.status = VendorStatus::Available;
        assert_eq!(
            probe_one(
                &manager,
                &client,
                &healthy,
                ProbeStrategy::MinimalCall,
                None
            )
            .await,
            (1, 0),
            "健康目标 minimal_call 仅验证连通性，不做恢复动作"
        );
    }

    // ---- run_probe_cycle：mode 门控与目标采集 ----

    #[tokio::test]
    async fn run_probe_cycle_off_mode_is_noop() {
        let manager = empty_manager();
        let config = cycle_manager("off");
        assert_eq!(
            run_probe_cycle(&manager, &config).await,
            ProbeReport::default(),
            "off 模式整周期 no-op"
        );
    }

    #[tokio::test]
    async fn run_probe_cycle_unknown_mode_falls_back_to_off() {
        let manager = empty_manager();
        let config = cycle_manager("yolo");
        assert_eq!(
            run_probe_cycle(&manager, &config).await,
            ProbeReport::default(),
            "未知 mode 归一化为 Off → no-op"
        );
    }

    #[tokio::test]
    async fn run_probe_cycle_all_mode_without_instances_reports_zero() {
        let manager = empty_manager();
        let config = cycle_manager("all");
        assert_eq!(
            run_probe_cycle(&manager, &config).await,
            ProbeReport::default(),
            "无实例无目标 → 零报告"
        );
    }

    #[tokio::test]
    async fn run_probe_cycle_suspect_only_targets_suspects_and_recovers() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server.mock("GET", "/v1/models").with_status(200).create();
        // v-a 将被标记 Throttled（suspect），v-b 保持 Available（健康，不采集）
        std::env::set_var("PROBE_TEST_POOL_ENV_XYZ", "probe-fake-pool-credential-1");
        let toml = format!(
            "[server]\nhost = \"127.0.0.1\"\nport = 0\n\n\
             [database]\npath = \":memory:\"\n\n\
             [[vendors]]
id = \"v-a\"
name = \"VA\"
type_ = \"openai\"
\
             api_key_env = \"PROBE_TEST_POOL_ENV_XYZ\"\napi_base = \"{url}\"\n\
             rate_limit = {{ type = \"time_window\", start = \"00:00\", end = \"24:00\" }}\n\
             concurrency = {{ requests_per_second = 1 }}\n\
             [[vendors]]
id = \"v-b\"
name = \"VB\"
type_ = \"openai\"
\
             api_key_env = \"PROBE_TEST_POOL_ENV_XYZ\"\napi_base = \"http://127.0.0.1:1\"\n\
             rate_limit = {{ type = \"time_window\", start = \"00:00\", end = \"24:00\" }}\n\
             concurrency = {{ requests_per_second = 1 }}\n\n\
             [health]\nmode = \"suspect_only\"\n",
            url = server.url()
        );
        let config = crate::config::loader::parse_config_content(Path::new("config.toml"), &toml)
            .expect("suspect_only 测试配置必须合法");
        let cm = Arc::new(ConfigManager::new(config));
        let manager = Arc::new(VendorManager::new(cm.clone()));
        manager.initialize().await.expect("实例初始化必须成功");
        // 60s 退避：覆盖整个测试窗口（0ms 退避即刻过期回 Available）
        manager.mark_throttled("v-a", Duration::from_secs(60));
        assert_eq!(
            manager.get_status("v-a").await,
            VendorStatus::Throttled,
            "前置：v-a 为 suspect（Throttled）"
        );

        let report = run_probe_cycle(&manager, &cm).await;
        assert_eq!(
            report,
            ProbeReport {
                probed: 1,
                recovered: 1
            },
            "suspect_only 只采集 suspect 目标，且探测成功即恢复"
        );
        assert_eq!(
            manager.get_status("v-a").await,
            VendorStatus::Available,
            "恢复动作把目标翻回 Available"
        );
    }

    /// rerank 厂商使用专用探测，仍进入探测目标（不再跳过）
    #[tokio::test]
    async fn probe_targets_include_rerank_vendors() {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 0

[database]
path = ":memory:"

[[vendors]]
id = "chat-v"
name = "Chat"
type_ = "openai"
api_key_env = "CHAT_KEY"  # pragma: allowlist secret（测试环境变量名，非凭证）
api_base = "http://127.0.0.1:1/v1"
rate_limit = { type = "time_window", start = "00:00", end = "24:00" }
concurrency = { requests_per_second = 10 }

[[vendors.models]]
model_id = "gpt-4o"

[[vendors]]
id = "rr-v"
name = "Rerank"
type_ = "rerank"
api_key_env = "RR_KEY"  # pragma: allowlist secret（测试环境变量名，非凭证）
api_base = "http://127.0.0.1:1"
rate_limit = { type = "time_window", start = "00:00", end = "24:00" }
concurrency = { requests_per_second = 10 }

[[vendors.models]]
model_id = "rerank-v3"
model_type = "rerank"
"#;
        let config = crate::config::loader::parse_config_content(Path::new("config.toml"), toml)
            .expect("配置必须合法");
        let cm = Arc::new(ConfigManager::new(config));
        let manager = Arc::new(VendorManager::new(cm));
        manager.initialize().await.expect("实例初始化必须成功");

        let targets = manager.probe_targets(false).await;
        let ids: Vec<&str> = targets.iter().map(|t| t.vendor_id.as_str()).collect();
        assert!(ids.contains(&"chat-v"), "普通厂商仍在探测目标: {ids:?}");
        assert!(
            ids.contains(&"rr-v"),
            "rerank 厂商必须由专用探测覆盖: {ids:?}"
        );

        let suspects = manager.probe_targets(true).await;
        assert!(
            !suspects.iter().any(|t| t.vendor_id == "rr-v"),
            "全 Available 时 suspects_only 不含任何厂商"
        );

        // 置为 suspect 后，rerank 厂商同样进入 suspects_only 视角
        manager.mark_throttled("rr-v", Duration::from_secs(60));
        let suspects = manager.probe_targets(true).await;
        assert!(
            suspects.iter().any(|t| t.vendor_id == "rr-v"),
            "suspects_only 同样覆盖 rerank 厂商"
        );
    }

    #[test]
    fn minimal_rerank_body_uses_first_enabled_model() {
        let mut config = probe_config("http://127.0.0.1:1", "rerank");
        config.models = vec![model("disabled", false), model("rerank-v3", true)];
        let body = minimal_rerank_body(&config);
        assert_eq!(body["model"], "rerank-v3");
        assert_eq!(body["query"], "ping");
        assert_eq!(body["documents"], json!(["ping"]));
        assert_eq!(body["top_n"], 1);
    }

    /// rerank 专用探测：2xx 恢复（未知 vendor 的恢复动作为 no-op，计数为准）
    #[tokio::test]
    async fn probe_rerank_call_recovers_on_2xx() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/rerank")
            .match_header("authorization", "Bearer k-current")
            .with_status(200)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut config = probe_config(&server.url(), "rerank");
        config.models = vec![model("rerank-v3", true)];
        let t = target(config, Some(("k-current".into(), "fp-cur".into())));

        let recovered = probe_rerank_call(&manager, &client, &t, None, true).await;
        assert_eq!(recovered, 1, "当前 key 2xx 即计恢复");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn probe_rerank_call_non_2xx_does_not_recover() {
        let mut server = mockito::Server::new_async().await;
        server.mock("POST", "/rerank").with_status(500).create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut config = probe_config(&server.url(), "rerank");
        config.models = vec![model("rerank-v3", true)];
        let t = target(config, Some(("k-current".into(), "fp-cur".into())));

        let recovered = probe_rerank_call(&manager, &client, &t, None, true).await;
        assert_eq!(recovered, 0, "非 2xx 不恢复");
    }

    /// 无池/无黑名单：回退 api_key_env 现读凭证，成功计恢复
    #[tokio::test]
    async fn probe_rerank_call_falls_back_to_env_credential() {
        let mut server = mockito::Server::new_async().await;
        std::env::set_var("PROBE_RERANK_ENV_XYZ", "env-key"); // pragma: allowlist secret（测试凭证占位）
        let mock = server
            .mock("POST", "/rerank")
            .match_header("authorization", "Bearer env-key")
            .with_status(200)
            .create();
        let manager = empty_manager();
        let client = manager.outbound_client();
        let mut config = probe_config(&server.url(), "rerank");
        config.api_key_env = "PROBE_RERANK_ENV_XYZ".into(); // pragma: allowlist secret（测试环境变量名）
        config.models = vec![model("rerank-v3", true)];
        let t = target(config, None);

        let recovered = probe_rerank_call(&manager, &client, &t, None, true).await;
        assert_eq!(recovered, 1, "env 凭证成功也计恢复");
        mock.assert_async().await;
    }
}
