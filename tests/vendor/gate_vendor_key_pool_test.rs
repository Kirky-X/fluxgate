// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 集成测试 — vendor 内 key 池（N5）：多 key 轮换顺序、耗尽回退、
//! 401/403 认证类失败触发的轮换、配置热更新换池，以及 parity 矩阵第 6 项。
//!
//! 语义源：agentstem `provider_chain.rs`（池展开 / 按 key 指纹
//! 隔离熔断计数 / 认证失败切下一 key），实现为 vendor 模块独立重写。
//!
//! 轮换契约：401/403 推进游标，**生效于下次请求**；单请求内不换 key
//! 重试（401 属 Fatal 分类，无重试——mockito 上界锁定恰 1 次 HTTP）。

#![cfg(feature = "vendor-management")]

#[path = "../common/mod.rs"]
mod common;

use nexusflow::config::{Config, ConfigManager, RetrySection, VendorConfig};
use nexusflow::error::GateError;
use nexusflow::protocol::{Message, MessageContent, Role, UnifiedRequest};
use nexusflow::vendor::VendorManager;
use std::sync::Arc;

const OPENAI_SUCCESS_BODY: &str = r#"{
    "id": "chatcmpl-kp",
    "object": "chat.completion",
    "model": "gpt-4",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}, "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
}"#;

/// key 池 env 名（各测试唯一，避免并行 set_var 竞态）
const ENV_KEY_FIRST: &str = "NEXUSFLOW_KP_KEY_FIRST";
const ENV_KEY_SECOND: &str = "NEXUSFLOW_KP_KEY_SECOND";
const ENV_KEY_THIRD: &str = "NEXUSFLOW_KP_KEY_THIRD";
const KEY_FIRST: &str = "kp-key-first";
const KEY_SECOND: &str = "kp-key-second";
const KEY_THIRD: &str = "kp-key-third";

fn make_vendor(
    id: &str,
    api_base: String,
    api_key_env: &str,
    api_key_envs: &[&str],
) -> VendorConfig {
    common::ensure_test_api_key();
    let mut v = common::base_vendor(id, api_base);
    v.api_key_env = api_key_env.into();
    v.api_key_envs = api_key_envs.iter().map(|s| s.to_string()).collect();
    v
}

/// 同款：max_retries=0 —— 每请求恰 1 次 HTTP，轮换断言不受
/// 内部重试噪声干扰
fn make_config(vendors: Vec<VendorConfig>) -> Config {
    Config {
        retry: RetrySection {
            max_retries: 0,
            ..Default::default()
        },
        ..common::test_config(vendors)
    }
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

async fn bearer_mock(
    server: &mut mockito::Server,
    key: &str,
    status: usize,
    body: &str,
) -> mockito::Mock {
    let mock = server.mock("POST", "/v1/chat/completions");
    let mock = mock.match_header("authorization", format!("Bearer {key}").as_str());
    let mock = mock.with_status(status);
    let mock = mock.with_body(body);
    mock.create_async().await
}

/// 池展开顺序 + 401 轮换：请求 1 用显式列表首名（KEY_FIRST）收到 401，
/// 游标推进；请求 2 用下一 key（KEY_SECOND）成功。
///
/// 「轮换生效于下次尝试，不在单请求内换 key 重试」由 mock 上界锁定：
/// KEY_SECOND 的 mock `expect(0)`——若实现在请求 1 内换 key 重试，
/// 该 mock 即被命中翻红。
#[tokio::test]
async fn auth_failure_rotates_to_next_key_on_next_request() {
    std::env::set_var(ENV_KEY_FIRST, KEY_FIRST);
    std::env::set_var(ENV_KEY_SECOND, KEY_SECOND);
    std::env::set_var(ENV_KEY_THIRD, KEY_THIRD);

    let mut server = mockito::Server::new_async().await;
    let mock_first = bearer_mock(&mut server, KEY_FIRST, 401, "{\"error\":\"bad key\"}").await;
    let mock_second = bearer_mock(&mut server, KEY_SECOND, 200, OPENAI_SUCCESS_BODY).await;

    let v = make_vendor(
        "kp-rotate",
        server.url(),
        ENV_KEY_THIRD,
        &[ENV_KEY_FIRST, ENV_KEY_SECOND],
    );
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init");

    // 池展开顺序：显式列表在前（首名 KEY_FIRST），legacy 兜底名（ENV_KEY_THIRD）
    // 去重后不重复出现
    let info = manager.key_pool_info("kp-rotate").expect("pool info");
    assert_eq!(
        info.envs,
        vec![ENV_KEY_FIRST, ENV_KEY_SECOND, ENV_KEY_THIRD],
        "合并去重定序：显式列表在前，legacy 兜底在后"
    );
    assert_eq!(info.size, 3);
    assert_eq!(info.current_index, 0);

    let first = manager
        .execute_request("kp-rotate", make_unified_request())
        .await;
    match &first {
        Err(GateError::VendorUnavailable { vendor }) => {
            assert!(vendor.contains("401"), "401 透传进错误文案: {vendor}");
        }
        other => panic!("expected VendorUnavailable, got {:?}", other.as_ref().err()),
    }
    // 「不在单请求内换 key 重试」 witness：请求 1 若换 key 重试，
    // KEY_SECOND 的 200 mock 会在请求 1 内被命中，请求 1 即成功而非 401

    // 轮换生效于下次尝试：请求 2 用 KEY_SECOND 成功
    let second = manager
        .execute_request("kp-rotate", make_unified_request())
        .await;
    assert!(
        second.is_ok(),
        "second request should succeed: {:?}",
        second.err()
    );

    let info = manager.key_pool_info("kp-rotate").expect("pool info");
    assert_eq!(info.current_index, 1, "游标已推进到第 2 名");

    mock_first.assert_async().await;
    mock_second.assert_async().await;
}

/// 403 同属认证类触发集（provider_chain 切换触发集合 401/403）。
#[tokio::test]
async fn forbidden_403_also_triggers_rotation() {
    std::env::set_var(ENV_KEY_FIRST, KEY_FIRST);
    std::env::set_var(ENV_KEY_SECOND, KEY_SECOND);
    std::env::set_var(ENV_KEY_THIRD, KEY_THIRD);

    let mut server = mockito::Server::new_async().await;
    let mock_first = bearer_mock(&mut server, KEY_FIRST, 403, "{\"error\":\"forbidden\"}")
        .await
        .expect(1);
    let mock_second = bearer_mock(&mut server, KEY_SECOND, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(1);

    let v = make_vendor(
        "kp-403",
        server.url(),
        ENV_KEY_THIRD,
        &[ENV_KEY_FIRST, ENV_KEY_SECOND],
    );
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init");

    let _ = manager
        .execute_request("kp-403", make_unified_request())
        .await;
    let ok = manager
        .execute_request("kp-403", make_unified_request())
        .await;
    assert!(
        ok.is_ok(),
        "403 后下一请求应轮换到 KEY_SECOND: {:?}",
        ok.err()
    );

    mock_first.assert_async().await;
    mock_second.assert_async().await;
}

/// parity 矩阵第 6 项（design.md D6）：多 key 轮换 —— env 池展开、轮转
/// 顺序、按 key 指纹隔离的熔断计数（语义源 provider_chain.rs）。
///
/// 等价断言分两相：
/// - **同 key 跨请求共享计数**：5 次 400（非认证类失败，不触发轮换）全部
///   落在 KEY_FIRST 的指纹熔断器上触达阈值 5 → Open——5 个请求累计到同
///   一指纹；
/// - **不同 key 隔离**：KEY_SECOND 的指纹熔断器全程 Closed——坏 key 的
///   健康度不污染同 vendor 其他 key；
/// - **认证轮换**：CB 短窗越过后 KEY_FIRST 再 401 → 游标轮换到
///   KEY_SECOND，后续请求成功且 vendor 恢复 Available。
#[cfg(feature = "resilience")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity6_multi_key_rotation_env_pool_expansion_and_per_key_fingerprint_isolated_breaker() {
    use limiteron::error::CircuitState;
    use nexusflow::vendor::VendorStatus;
    use std::time::Duration;

    std::env::set_var(ENV_KEY_FIRST, KEY_FIRST);
    std::env::set_var(ENV_KEY_SECOND, KEY_SECOND);

    let mut server = mockito::Server::new_async().await;
    // 相 2 的 401/200 mock 先建；相 1 期间 KEY_FIRST 只有 400 mock 可匹配
    //（401 mock 同头匹配但单 mock 存在期不冲突，见下 drop 注销时序）
    let mock_second = bearer_mock(&mut server, KEY_SECOND, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(1);
    let mock_first_400 = bearer_mock(&mut server, KEY_FIRST, 400, "{\"error\":\"bad request\"}")
        .await
        .expect(5);

    // 双 key 池（空 legacy 名被过滤）：轮转顺序 FIRST → SECOND
    let v = make_vendor(
        "kp-parity6",
        server.url(),
        "",
        &[ENV_KEY_FIRST, ENV_KEY_SECOND],
    );
    let mut config = make_config(vec![v]);
    // 短窗注入（与 vendor_circuit_breaker_test 同款）：CB Open 窗口 300ms
    config.resilience.circuit_open_timeout_ms = 300;
    let cm = Arc::new(ConfigManager::new(config));
    let manager = Arc::new(VendorManager::new(cm));
    manager.initialize().await.expect("init");

    // 池展开顺序（env 池展开等价性）：显式列表在前
    let before = manager.key_pool_info("kp-parity6").expect("pool info");
    assert_eq!(before.envs, vec![ENV_KEY_FIRST, ENV_KEY_SECOND]);
    assert_eq!(before.current_index, 0);

    // 相 1：5 次 400 —— 同 key 跨请求共享计数（5 个请求累计到同一指纹）
    for _ in 0..5 {
        let r = manager
            .execute_request("kp-parity6", make_unified_request())
            .await;
        assert!(r.is_err(), "phase-1 400s must fail");
    }
    let after_400s = manager.key_pool_info("kp-parity6").expect("pool info");
    assert_eq!(
        after_400s.current_index, 0,
        "400 非认证类失败不推进轮换游标"
    );
    assert_eq!(
        manager
            .get_circuit_state_for_fingerprint("kp-parity6", &before.fingerprints[0])
            .await,
        Some(CircuitState::Open),
        "KEY_FIRST 的 5 次失败按自身指纹累计熔断（同 key 跨请求共享）"
    );
    assert_eq!(
        manager
            .get_circuit_state_for_fingerprint("kp-parity6", &before.fingerprints[1])
            .await,
        None,
        "KEY_SECOND 尚无任何请求——不存在计数条目（不同 key 隔离：KEY_FIRST \
         的失败未在 KEY_SECOND 指纹下创建/污染任何熔断器）"
    );
    assert_eq!(
        manager.get_circuit_state("kp-parity6").await,
        Some(CircuitState::Open),
        "当前 key 的熔断器决定 vendor 熔断观测态"
    );
    assert_eq!(
        manager.get_status("kp-parity6").await,
        VendorStatus::Unavailable,
        "当前 key 熔断 Open → vendor 观测不可用（轮换仅由认证失败驱动）"
    );

    // 相 1 mock 断言后注销：KEY_FIRST 的探针流量改由 401 mock 接住
    mock_first_400.assert_async().await;
    drop(mock_first_400);
    let mock_first_401 = bearer_mock(&mut server, KEY_FIRST, 401, "{\"error\":\"bad key\"}")
        .await
        .expect(1);

    // 相 2：越过 CB 短窗，KEY_FIRST 探针 401 → 轮换到 KEY_SECOND 成功
    tokio::time::sleep(Duration::from_millis(400)).await;
    let probe = manager
        .execute_request("kp-parity6", make_unified_request())
        .await;
    assert!(probe.is_err(), "KEY_FIRST 探针 401 应失败");
    let recovered = manager
        .execute_request("kp-parity6", make_unified_request())
        .await;
    assert!(
        recovered.is_ok(),
        "轮换到 KEY_SECOND 后应成功: {:?}",
        recovered.err()
    );
    let after_401 = manager.key_pool_info("kp-parity6").expect("pool info");
    assert_eq!(
        after_401.current_index, 1,
        "401 认证失败把游标轮换到 KEY_SECOND"
    );
    assert_eq!(
        manager
            .get_circuit_state_for_fingerprint("kp-parity6", &before.fingerprints[1])
            .await,
        Some(CircuitState::Closed),
        "KEY_SECOND 通道独立计数且健康（成功请求后熔断器存在且 Closed）"
    );
    assert_eq!(
        manager.get_status("kp-parity6").await,
        VendorStatus::Available,
        "健康 key 在位时 vendor 恢复可用"
    );

    mock_first_401.assert_async().await;
    mock_second.assert_async().await;
}

/// 池耗尽/全缺 env：回退既有单 key 行为（legacy env 缺失 → Config 错误
/// 携带 env 名），池观测 size=0，绝不静默。
#[tokio::test]
async fn exhausted_pool_falls_back_to_legacy_single_key_behavior() {
    const MISSING_A: &str = "NEXUSFLOW_KP_MISSING_A_Q7";
    const MISSING_B: &str = "NEXUSFLOW_KP_MISSING_B_Q7";
    const MISSING_LEGACY: &str = "NEXUSFLOW_KP_MISSING_LEGACY_Q7";
    std::env::remove_var(MISSING_A);
    std::env::remove_var(MISSING_B);
    std::env::remove_var(MISSING_LEGACY);

    let v = make_vendor(
        "kp-exhausted",
        "http://127.0.0.1:1/v1".into(),
        MISSING_LEGACY,
        &[MISSING_A, MISSING_B],
    );
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init");

    let info = manager.key_pool_info("kp-exhausted").expect("pool info");
    assert_eq!(info.size, 0, "全缺 env 池为空");
    assert_eq!(info.fingerprints.len(), 0);

    let result = manager
        .execute_request("kp-exhausted", make_unified_request())
        .await;
    match result {
        Err(GateError::Config { message }) => {
            assert!(
                message.contains(MISSING_LEGACY),
                "legacy 回退行为：错误携带 api_key_env 名 {MISSING_LEGACY}: {message}"
            );
        }
        other => panic!("expected Config error, got {:?}", other.as_ref().err()),
    }
}

/// 配置热更新换池：POST /admin/config 语义（ConfigManager::update_config，
/// 校验/快照/原子切换）改 api_key_envs 后，下一次请求立即使用新池。
#[tokio::test]
async fn hot_update_swaps_key_pool_for_next_request() {
    std::env::set_var(ENV_KEY_FIRST, KEY_FIRST);
    std::env::set_var(ENV_KEY_SECOND, KEY_SECOND);
    std::env::set_var(ENV_KEY_THIRD, KEY_THIRD);

    let mut server = mockito::Server::new_async().await;
    let mock_first = bearer_mock(&mut server, KEY_FIRST, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(1);
    let mock_second = bearer_mock(&mut server, KEY_SECOND, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(1);

    let v = make_vendor("kp-hot", server.url(), ENV_KEY_THIRD, &[ENV_KEY_FIRST]);
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    // 热更新落盘路径：ConfigManager 仅在经 ConfigLoader 注入路径后接受
    // update_config（内存态构造拒绝持久化）——tempdir 提供 RAII 唯一路径
    let dir = tempfile::tempdir().expect("tempdir");
    cm.set_path(dir.path().join("config.toml"));
    let manager = VendorManager::new(cm.clone());
    manager.initialize().await.expect("init");

    let ok = manager
        .execute_request("kp-hot", make_unified_request())
        .await;
    assert!(ok.is_ok(), "first request with KEY_FIRST: {:?}", ok.err());

    // 热更新：换池为 [KEY_SECOND]（legacy 名清空避免并入合并结果；
    // 校验/快照/原子切换语义与既有 vendors 字段一致，经
    // ConfigManager::update_config 同一路径）
    let mut updated = cm.get_config();
    updated.vendors[0].api_key_env = String::new();
    updated.vendors[0].api_key_envs = vec![ENV_KEY_SECOND.to_string()];
    cm.update_config(updated).expect("hot update");

    // 池快照为请求期惰性刷新（select 检测名单变更重解析）——驱动一次
    // 请求后新池生效且观测可见
    let ok = manager
        .execute_request("kp-hot", make_unified_request())
        .await;
    assert!(ok.is_ok(), "request after hot update: {:?}", ok.err());
    let info = manager.key_pool_info("kp-hot").expect("pool info");
    assert_eq!(info.envs, vec![ENV_KEY_SECOND], "池观测已切换");

    mock_first.assert_async().await;
    mock_second.assert_async().await;
}

/// 三态集成 —— 永久错误黑名单：KEY_FIRST 首请求 401 后该 key 不再
/// 被选中（后续 3 请求恒走 KEY_SECOND，不随时间恢复）；池内其余 key
/// 继续服务；管理面 reset 后 KEY_FIRST 恢复可选。
///
/// 轮换/黑名单均生效于下次请求（单请求内不换 key）：KEY_SECOND 的首个
/// mock 若在请求 1 内被命中即翻红（沿用本文件既有上界锁定惯例）。
#[tokio::test]
async fn permanent_error_blacklists_key_until_reset() {
    const ENV_A: &str = "NEXUSFLOW_KP_BL_A";
    const ENV_B: &str = "NEXUSFLOW_KP_BL_B";
    const KEY_A: &str = "kp-bl-key-a";
    const KEY_B: &str = "kp-bl-key-b";
    std::env::set_var(ENV_A, KEY_A);
    std::env::set_var(ENV_B, KEY_B);

    let mut server = mockito::Server::new_async().await;
    // KEY_A：首请求 401（expect 1 锁定单次）。
    let mock_a_401 = bearer_mock(&mut server, KEY_A, 401, "{\"error\":\"bad key\"}")
        .await
        .expect(1);
    let mock_b_ok = bearer_mock(&mut server, KEY_B, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(3);

    let v = make_vendor("kp-blacklist", server.url(), ENV_A, &[ENV_A, ENV_B]);
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init");

    // 请求 1：KEY_A 401 → 错误 + 黑名单 + 游标推进。
    let first = manager
        .execute_request("kp-blacklist", make_unified_request())
        .await;
    assert!(first.is_err(), "401 必须失败: {:?}", first.ok());
    mock_a_401.assert_async().await;

    // 请求 2/3/4：恒走 KEY_SECOND（黑名单不随时间恢复）。
    for i in 0..3 {
        let ok = manager
            .execute_request("kp-blacklist", make_unified_request())
            .await;
        assert!(
            ok.is_ok(),
            "请求 {} 必须由其余 key 服务: {:?}",
            i + 2,
            ok.err()
        );
    }

    // 管理面 reset：黑名单清空 + 游标回绕 → KEY_FIRST 恢复可选。
    assert!(manager.reset_key_pool_state("kp-blacklist"));
    assert!(
        !manager.reset_key_pool_state("ghost-vendor"),
        "未知 vendor 显性 false"
    );
    // 恢复 mock 在 reset 后创建（避免与 401 mock 同匹配器歧义）。
    let mock_a_recovered = bearer_mock(&mut server, KEY_A, 200, OPENAI_SUCCESS_BODY)
        .await
        .expect(1);
    let ok = manager
        .execute_request("kp-blacklist", make_unified_request())
        .await;
    assert!(ok.is_ok(), "reset 后 KEY_A 恢复: {:?}", ok.err());

    mock_b_ok.assert_async().await;
    mock_a_recovered.assert_async().await;
}

/// 回归 —— 429 瞬时：同 key 复用（无轮换、无黑名单），行为与现状一致。
#[tokio::test]
async fn rate_limited_reuses_same_key_without_blacklist() {
    const ENV_A: &str = "NEXUSFLOW_KP_RL_A";
    const ENV_B: &str = "NEXUSFLOW_KP_RL_B";
    const KEY_A: &str = "kp-rl-key-a";
    const KEY_B: &str = "kp-rl-key-b";
    std::env::set_var(ENV_A, KEY_A);
    std::env::set_var(ENV_B, KEY_B);

    let mut server = mockito::Server::new_async().await;
    // 429 两次恒走 KEY_A（expect 2）；KEY_B 零触达（expect 0 锁定无轮换）。
    let mock_a_429 = bearer_mock(&mut server, KEY_A, 429, "rate limited")
        .await
        .expect(2);
    let mock_b_untouched = bearer_mock(&mut server, KEY_B, 429, "rate limited")
        .await
        .expect(0);

    let v = make_vendor("kp-ratelimit", server.url(), ENV_A, &[ENV_A, ENV_B]);
    let cm = Arc::new(ConfigManager::new(make_config(vec![v])));
    let manager = VendorManager::new(cm);
    manager.initialize().await.expect("init");

    for i in 0..2 {
        let err = manager
            .execute_request("kp-ratelimit", make_unified_request())
            .await;
        assert!(err.is_err(), "429 请求 {i} 必须失败");
    }

    mock_a_429.assert_async().await;
    mock_b_untouched.assert_async().await;
}
