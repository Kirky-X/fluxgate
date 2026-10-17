// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 集成测试 — 主动健康检查探针：三模式（off/suspect_only/all）
//! 与被动恢复（成功清冷却/黑名单，失败保持现状）。经可测入口
//! `run_probe_cycle` 直接驱动（无 timing，mock 上游断言行为）。

#![cfg(feature = "vendor-management")]

#[path = "../common/mod.rs"]
mod common;

use fluxgate::config::{Config, ConfigManager, HealthSection, VendorConfig};
use fluxgate::protocol::{Message, MessageContent, Role, UnifiedRequest};
use fluxgate::vendor::{probe, VendorManager, VendorStatus};
use std::sync::Arc;
use std::time::Duration;

fn make_vendor(id: &str, api_base: String) -> VendorConfig {
    common::ensure_test_api_key();
    common::base_vendor(id, api_base)
}

fn health_config(
    vendors: Vec<VendorConfig>,
    mode: &str,
    strategy: &str,
) -> (Arc<ConfigManager>, Config) {
    let mut config = common::test_config(vendors);
    config.health = HealthSection {
        mode: mode.into(),
        interval_secs: 300,
        timeout_ms: 5000,
        probe_strategy: strategy.into(),
    };
    let cm = Arc::new(ConfigManager::new(config.clone()));
    (cm, config)
}

async fn make_manager(cm: Arc<ConfigManager>) -> Arc<VendorManager> {
    let manager = Arc::new(VendorManager::new(cm));
    manager.initialize().await.expect("init");
    manager
}

fn make_unified_request() -> UnifiedRequest {
    UnifiedRequest {
        model: "gpt-4".into(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text("hello".into()),
        }],
        temperature: None,
        max_tokens: Some(10),
        stream: false,
        stop: vec![],
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        logit_bias: Default::default(),
        user: None,
        tools: vec![],
        tool_choice: None,
        ..Default::default()
    }
}

/// mode=off：零探测（即使 vendor 处于冷却），状态保持。
#[tokio::test]
async fn off_mode_probes_nothing() {
    let (cm, _) = health_config(
        vec![make_vendor("off-v", "http://127.0.0.1:1".into())],
        "off",
        "free",
    );
    let manager = make_manager(cm.clone()).await;
    manager.mark_throttled("off-v", Duration::from_secs(60));

    let report = probe::run_probe_cycle(&manager, &cm).await;
    assert_eq!(report.probed, 0, "off 模式零探测");
    assert_eq!(report.recovered, 0);
    assert_eq!(
        manager.get_status("off-v").await,
        VendorStatus::Throttled,
        "off 模式不改变状态"
    );
}

/// suspect_only + free：冷却 vendor 被探测，models 端点 200 即恢复
/// Available；健康 vendor 不被触达（probed 计数证明）。
#[tokio::test]
async fn suspect_only_recovers_cooling_vendor() {
    let mut server = mockito::Server::new_async().await;
    let mock_models = server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"object":"list","data":[]}"#)
        .expect(1)
        .create_async()
        .await;

    let (cm, _) = health_config(
        vec![make_vendor("sus-v", server.url())],
        "suspect_only",
        "free",
    );
    let manager = make_manager(cm.clone()).await;
    manager.mark_throttled("sus-v", Duration::from_secs(60));
    assert_eq!(manager.get_status("sus-v").await, VendorStatus::Throttled);

    let report = probe::run_probe_cycle(&manager, &cm).await;
    assert_eq!(report.probed, 1);
    assert_eq!(report.recovered, 1);
    assert_eq!(
        manager.get_status("sus-v").await,
        VendorStatus::Available,
        "free 成功必须清除冷却（被动恢复）"
    );

    mock_models.assert_async().await;
}

/// suspect_only 跳过健康 vendor（死 api_base 亦无任何 HTTP——probed=0）。
#[tokio::test]
async fn suspect_only_skips_healthy_vendor() {
    let (cm, _) = health_config(
        vec![make_vendor("healthy-v", "http://127.0.0.1:1".into())],
        "suspect_only",
        "free",
    );
    let manager = make_manager(cm.clone()).await;
    assert_eq!(
        manager.get_status("healthy-v").await,
        VendorStatus::Available
    );

    let report = probe::run_probe_cycle(&manager, &cm).await;
    assert_eq!(report.probed, 0, "健康对象不得被探测");
    assert_eq!(report.recovered, 0);
}

/// all 模式探测健康 vendor（成功但无恢复可计）；失败保持现状。
#[tokio::test]
async fn all_mode_probes_healthy_and_failure_keeps_state() {
    // 成功侧：健康 vendor 被探测（probed=1），无 suspect 故 recovered=0。
    let mut server_ok = mockito::Server::new_async().await;
    let mock_ok = server_ok
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_body(r#"{"object":"list","data":[]}"#)
        .expect(1)
        .create_async()
        .await;
    let (cm_ok, _) = health_config(vec![make_vendor("all-ok", server_ok.url())], "all", "free");
    let manager_ok = make_manager(cm_ok.clone()).await;
    let report = probe::run_probe_cycle(&manager_ok, &cm_ok).await;
    assert_eq!((report.probed, report.recovered), (1, 0));
    mock_ok.assert_async().await;

    // 失败侧：冷却 vendor + 500 → 状态保持 Throttled。
    let mut server_bad = mockito::Server::new_async().await;
    let mock_bad = server_bad
        .mock("GET", "/v1/models")
        .with_status(500)
        .with_body("boom")
        .expect(1)
        .create_async()
        .await;
    let (cm_bad, _) = health_config(
        vec![make_vendor("all-bad", server_bad.url())],
        "all",
        "free",
    );
    let manager_bad = make_manager(cm_bad.clone()).await;
    manager_bad.mark_throttled("all-bad", Duration::from_secs(60));
    let report = probe::run_probe_cycle(&manager_bad, &cm_bad).await;
    assert_eq!((report.probed, report.recovered), (1, 0));
    assert_eq!(
        manager_bad.get_status("all-bad").await,
        VendorStatus::Throttled,
        "探测失败必须保持现状"
    );
    mock_bad.assert_async().await;
}

/// minimal_call 被动恢复黑名单：401 致黑 key A → 周期恢复（recovered≥1，
/// 含当前 key B 的验证）→ 第二周期无 suspect（0,0，证明黑名单已清）→
/// 其余 key 全程继续服务。
#[tokio::test]
async fn minimal_call_recovers_blacklisted_key() {
    const ENV_A: &str = "FLUXGATE_HP_KEY_A";
    const ENV_B: &str = "FLUXGATE_HP_KEY_B";
    const KEY_A: &str = "hp-key-a";
    const KEY_B: &str = "hp-key-b";
    std::env::set_var(ENV_A, KEY_A);
    std::env::set_var(ENV_B, KEY_B);

    let mut server = mockito::Server::new_async().await;
    // 驱动 401：Bearer A 首请求 401（expect 1）。
    let mock_a_401 = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", format!("Bearer {KEY_A}").as_str())
        .with_status(401)
        .with_body("{\"error\":\"bad key\"}")
        .expect(1)
        .create_async()
        .await;
    // 其余 key 服务：Bearer B 全部 200（周期内 minimal_call 验证 + 请求）。
    let openai_ok = r#"{"id":"hp","object":"chat.completion","model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#;
    let mock_b_200 = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", format!("Bearer {KEY_B}").as_str())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok)
        .expect_at_least(1)
        .create_async()
        .await;

    let mut v = make_vendor("hp-v", server.url());
    v.api_key_env = ENV_A.into();
    v.api_key_envs = vec![ENV_A.to_string(), ENV_B.to_string()];
    let mut probe_config = common::test_config(vec![v]);
    probe_config.health = HealthSection {
        mode: "suspect_only".into(),
        interval_secs: 300,
        timeout_ms: 5000,
        probe_strategy: "minimal_call".into(),
    };
    // 401 走 Fatal 单次（无重试噪声），与 key 池既有测试同口径。
    probe_config.retry.max_retries = 0;
    let cm = Arc::new(ConfigManager::new(probe_config));

    let manager = make_manager(cm.clone()).await;
    let first = manager
        .execute_request("hp-v", make_unified_request())
        .await;
    assert!(first.is_err(), "401 必须失败: {:?}", first.ok());
    mock_a_401.assert_async().await;

    // 周期 1：恢复 mock（Bearer A → 200，401 mock 已耗尽，新 mock 接管）。
    let mock_a_probe_200 = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", format!("Bearer {KEY_A}").as_str())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(openai_ok)
        .expect(1)
        .create_async()
        .await;
    let report = probe::run_probe_cycle(&manager, &cm).await;
    assert_eq!(report.probed, 1);
    assert!(report.recovered >= 1, "黑名单 key A 必须被恢复: {report:?}");
    mock_a_probe_200.assert_async().await;

    // 其余 key 全程服务 + 周期 2 无 suspect（黑名单已清的证明）。
    let ok = manager
        .execute_request("hp-v", make_unified_request())
        .await;
    assert!(ok.is_ok(), "其余 key 继续服务: {:?}", ok.err());
    let report2 = probe::run_probe_cycle(&manager, &cm).await;
    assert_eq!(
        (report2.probed, report2.recovered),
        (0, 0),
        "恢复后无 suspect（黑名单已清）: {report2:?}"
    );

    mock_b_200.assert_async().await;
}

// ==================== 并发上限与优雅停机退出 ====================

use std::sync::atomic::{AtomicUsize, Ordering};

/// 挂起式假上游：accept 后永不响应（探针按 `timeout_ms` 失败），并在**测试自己
/// 的 accept 循环内**记录在途峰值与累计连接数。
///
/// 为何不用 mockito：mockito 1.7.2 无请求延迟 API（`src/` 全量无 `with_delay`），
/// 且 mock 侧无法回调项目代码 → 并发只能在项目可见的一侧观测。在途计数必须在
/// 对端关闭时递减（探针超时后 reqwest 会释放连接），否则退化成累计值。
async fn spawn_hanging_upstream() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>)
{
    use tokio::io::AsyncReadExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hanging upstream");
    let addr = listener.local_addr().expect("local addr");
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (i_accept, p_accept, a_hold, i_hold) = (
        inflight.clone(),
        peak.clone(),
        accepted.clone(),
        inflight.clone(),
    );
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let live = i_accept.fetch_add(1, Ordering::SeqCst) + 1;
            p_accept.fetch_max(live, Ordering::SeqCst);
            a_hold.fetch_add(1, Ordering::SeqCst);
            let i_release = i_hold.clone();
            tokio::spawn(async move {
                let mut socket = socket;
                let mut buf = [0u8; 1024];
                // 只读不回：直到对端关闭（探针超时后 reqwest 释放连接）才减计数。
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
                i_release.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (format!("http://{addr}/v1"), inflight, peak, accepted)
}

fn probe_config(vendors: Vec<VendorConfig>, interval_secs: u64, timeout_ms: u64) -> Config {
    let mut config = common::test_config(vendors);
    config.health = HealthSection {
        mode: "all".into(),
        interval_secs,
        timeout_ms,
        probe_strategy: "free".into(),
    };
    config
}

/// `all` 模式的并发上限：4 个挂起目标同轮探测时在途峰值 ≤ `PROBE_CONCURRENCY`，
/// 且周期总耗时证明分批推进（非一次性抛 4 个）。
#[tokio::test]
async fn all_mode_probe_concurrency_is_capped() {
    let (base, inflight, peak, accepted) = spawn_hanging_upstream().await;
    let vendors: Vec<VendorConfig> = (0..4)
        .map(|i| make_vendor(&format!("cap-v{i}"), base.clone()))
        .collect();
    let config = probe_config(vendors, 300, 200);
    let cm = Arc::new(ConfigManager::new(config));
    let manager = make_manager(cm.clone()).await;

    let started = std::time::Instant::now();
    let report = probe::run_probe_cycle(&manager, &cm).await;
    let elapsed = started.elapsed();

    assert_eq!(report.probed, 4, "全量目标必须全部被处理: {report:?}");
    // 周期收束后等对端关闭把在途计数落干（峰值已记录，不受影响）。
    let mut settled = 0;
    while inflight.load(Ordering::SeqCst) != 0 && settled < 40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        settled += 1;
    }
    let observed = peak.load(Ordering::SeqCst);
    assert!(observed >= 1, "探针应确实发出探测: {observed}");
    assert!(
        observed <= probe::PROBE_CONCURRENCY,
        "在途峰值不得越过并发上限 {}，实测 {observed}",
        probe::PROBE_CONCURRENCY
    );
    // 4 目标 / 上限 2 = 至少两批，每批一个 timeout_ms；不封顶则总耗时≈单批。
    let min_expected = Duration::from_millis(200) * (4 / probe::PROBE_CONCURRENCY as u32)
        - Duration::from_millis(50);
    assert!(
        elapsed >= min_expected,
        "分批推进的总耗时应≥ {:?}，实测 {elapsed:?}（上限未生效则远小于此）",
        min_expected
    );
    assert_eq!(
        inflight.load(Ordering::SeqCst),
        0,
        "周期收束后不得持悬挂在途"
    );
    assert!(accepted.load(Ordering::SeqCst) >= 1, "至少有探测连接被接受");
}

/// 后台循环随优雅停机退出：`abort()` 后任务以取消收束，且跳过一个 interval
/// 周期后上游接受数不再增长（与 bootstrap 的 `probe_handle` 同语义）。
#[tokio::test]
async fn probe_loop_exits_on_abort_without_further_probes() {
    let (base, _inflight, _peak, accepted) = spawn_hanging_upstream().await;
    let config = probe_config(vec![make_vendor("loop-v", base)], 1, 200);
    let cm = Arc::new(ConfigManager::new(config));
    let manager = make_manager(cm.clone()).await;

    let handle = tokio::spawn(probe::run_probe_loop(manager.clone(), cm.clone()));
    // 首周期延迟一个 interval 后开探：轮询等到至少一次探测发生。
    let mut rounds = 0;
    while accepted.load(Ordering::SeqCst) == 0 && rounds < 40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        rounds += 1;
    }
    assert!(
        accepted.load(Ordering::SeqCst) > 0,
        "循环应周期性探测（等待 {rounds} 轮仍未命中）"
    );

    handle.abort();
    let joined = handle.await;
    let cancelled = matches!(&joined, Err(e) if e.is_cancelled());
    assert!(cancelled, "abort 后任务必须以取消收束: {joined:?}");

    // 跨一个 interval（1s）+ 余量：已停的循环不得再探测。
    let snapshot = accepted.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        snapshot,
        "停机后探测数不得增长（后台任务泄漏）"
    );
}
