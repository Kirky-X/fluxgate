// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

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
    if config.type_ == "anthropic" {
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

/// 单对象探测 → (probed, recovered)。`recovered` 仅当对象曾为 suspect 且成功。
async fn probe_one(
    manager: &Arc<VendorManager>,
    client: &reqwest::Client,
    target: &crate::vendor::manager::ProbeTarget,
    strategy: ProbeStrategy,
    timeout: Option<Duration>,
) -> (usize, usize) {
    let suspect = target.status != VendorStatus::Available || !target.blacklist.is_empty();
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
    let url = match VendorManager::build_vendor_url(&target.config) {
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
    let body = minimal_body(&target.config);
    let mut recovered = 0usize;
    for (key, fingerprint) in candidates {
        let mut request = client.post(&url).json(&body);
        for (name, value) in auth_headers(&target.config, &key) {
            request = request.header(name, value);
        }
        if let Some(duration) = timeout {
            request = request.timeout(duration);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                if recover {
                    manager
                        .probe_recover_key(&target.vendor_id, &fingerprint)
                        .await;
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
