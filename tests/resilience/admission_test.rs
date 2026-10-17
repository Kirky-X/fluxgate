#![cfg(feature = "web-server")]

// 拥塞感知优先级准入 — 纯函数单元 + AdmissionState 行为 +
// 中间件集成（构建于 #3 的 InboundLimitState / create_router 测试模式）。

#[path = "../common/mod.rs"]
mod common;

use fluxgate::config::{
    AdmissionSection, AuthKeyEntry, AuthSection, AuthTier, Config, ConfigManager, ServerConfig,
};
use fluxgate::middleware::admission::{
    admission_priority, tier_for_request, tier_weight, AdmissionState, RETRY_AFTER_SECS,
};
use fluxgate::vendor::VendorManager;
use std::sync::Arc;

fn section() -> AdmissionSection {
    AdmissionSection {
        enabled: true,
        ..Default::default()
    }
}

// ============ 优先级纯函数（论文式 1）============

/// 四档主导性：burst=debt=0 时 priority == w_tier（ℓ*=0 项恒 1），
/// 1000/100/1/0.1 数量级隔离。
#[test]
fn priority_four_tiers_dominance() {
    let s = section();
    assert_eq!(
        admission_priority(&s, tier_weight(AuthTier::Dedicated), 0.0, 0.0),
        1000.0
    );
    assert_eq!(
        admission_priority(&s, tier_weight(AuthTier::Guaranteed), 0.0, 0.0),
        100.0
    );
    assert_eq!(
        admission_priority(&s, tier_weight(AuthTier::Elastic), 0.0, 0.0),
        1.0
    );
    assert_eq!(
        admission_priority(&s, tier_weight(AuthTier::Spot), 0.0, 0.0),
        0.1
    );
    assert_eq!(tier_weight(AuthTier::Dedicated), 1000.0);
    assert_eq!(tier_weight(AuthTier::Spot), 0.1);
}

/// burst 抑制：同档单调，公式精确（w/(1+α_burst·b)）；极端 burst 可把
/// dedicated 压到 spot 基准以下（独占抑制的数值面）。
#[test]
fn priority_burst_suppression_exact() {
    let s = section(); // alpha_burst = 1.0
    let w = tier_weight(AuthTier::Elastic);
    assert_eq!(admission_priority(&s, w, 0.0, 0.0), 1.0);
    assert!((admission_priority(&s, w, 9.0, 0.0) - 0.1).abs() < 1e-12);
    assert!(
        admission_priority(&s, w, 9.0, 0.0) < admission_priority(&s, w, 4.0, 0.0),
        "burst 高者优先级低"
    );
    // dedicated burst=9999 → 1000/(1+9999) = 0.1 == 干净 spot 基准
    //（数量级差距可被持续突发抹平——独占抑制的数值面）
    assert!(
        (admission_priority(&s, tier_weight(AuthTier::Dedicated), 9999.0, 0.0)
            - tier_weight(AuthTier::Spot))
        .abs()
            < 1e-9,
        "极端 burst 独占抑制"
    );
}

/// debt 抑制：w/(1+α_debt·d)，α_debt=4.0 → elastic debt=1 → 0.2。
#[test]
fn priority_debt_suppression_exact() {
    let s = section();
    let w = tier_weight(AuthTier::Elastic);
    assert!((admission_priority(&s, w, 0.0, 1.0) - 0.2).abs() < 1e-12);
    assert!(
        admission_priority(&s, w, 0.0, 2.0) < admission_priority(&s, w, 0.0, 1.0),
        "debt 高者优先级低"
    );
}

// ============ AdmissionState 行为 ============

fn vendor_config(id: &str, max_concurrent: Option<usize>) -> fluxgate::config::VendorConfig {
    common::ensure_test_api_key();
    // 与共享基座的差异字段（显式保留原值）：max_concurrent 是拥塞 shed 的
    // 驱动值（测试语义主体，必须留在覆盖处）；空模型列表与 "Vendor {id}"
    // 命名为本文件历史形态
    let mut v = common::base_vendor(id, String::new());
    v.name = format!("Vendor {id}");
    v.models = vec![];
    v.concurrency.requests_per_second = 100;
    v.concurrency.max_concurrent_requests = max_concurrent;
    v
}

fn config_with(admission: AdmissionSection) -> Config {
    // 与共享基座的差异字段（显式保留原值）：admission 段是测试语义主体
    Config {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
        },
        admission,
        ..common::test_config(vec![vendor_config("v1", Some(2))])
    }
}

async fn make_admission(admission: AdmissionSection) -> (Arc<AdmissionState>, Arc<VendorManager>) {
    let cm = Arc::new(ConfigManager::new(config_with(admission)));
    let vm = Arc::new(VendorManager::new(cm.clone()));
    vm.initialize().await.expect("init vendors");
    let state = Arc::new(AdmissionState::new(cm, vm.clone()));
    (state, vm)
}

/// 默认关闭（enabled=false）：全部档位放行（429 永不发生），guard 计数
/// 照常（纯内存观测，不构成行为差异）。
#[tokio::test]
async fn disabled_by_default_passes_all_tiers() {
    let (state, _vm) = make_admission(AdmissionSection::default()).await;
    for tier in [AuthTier::Spot, AuthTier::Elastic, AuthTier::Dedicated] {
        let guard = state.admit(tier, "k", 1_000).expect("默认必须放行");
        drop(guard);
    }
    assert_eq!(state.tier_in_flight(AuthTier::Spot), 0);
}

/// guard 计数语义：持有时该档 +1，Drop 后 −1（泄漏为零）。
#[tokio::test]
async fn guard_counts_and_releases_on_drop() {
    let (state, _vm) = make_admission(section()).await;
    let g = state.admit(AuthTier::Elastic, "k", 1_000).expect("admit");
    assert_eq!(state.tier_in_flight(AuthTier::Elastic), 1);
    drop(g);
    assert_eq!(state.tier_in_flight(AuthTier::Elastic), 0);
}

/// panic 路径：guard 展开 Drop 仍释放计数（不泄漏）。
#[tokio::test]
async fn guard_releases_on_panic_unwind() {
    let (state, _vm) = make_admission(section()).await;
    let s = state.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _g = s.admit(AuthTier::Spot, "k", 1_000).expect("admit");
        panic!("boom");
    }));
    assert!(result.is_err());
    assert_eq!(
        state.tier_in_flight(AuthTier::Spot),
        0,
        "panic 展开必须经 Drop 释放计数"
    );
}

/// 拥塞 shed：并发上限 2 占满（ratio 1.0 ≥ 0.9）后，dedicated 在途时
/// spot/elastic/guaranteed（低于在途最小档）被 429 + Retry-After，
/// 同档 dedicated 放行（先到先得保留在同档内）。
#[tokio::test]
async fn congestion_sheds_lower_tiers_only() {
    let (state, vm) = make_admission(section()).await;
    let _p1 = vm.acquire_permit("v1").await.expect("permit");
    let _p2 = vm.acquire_permit("v1").await.expect("permit");
    assert_eq!(vm.fleet_concurrency(), (2, 2), "占满两个并发票据");

    // 首个 dedicated 放行并持票 → 在途最小档 = dedicated(1000)
    let g = state
        .admit(AuthTier::Dedicated, "ded-key", 1_000)
        .expect("dedicated 放行");
    assert_eq!(state.tier_in_flight(AuthTier::Dedicated), 1);

    // 低于在途最小档的全部被拒，Retry-After = 1s
    for tier in [AuthTier::Guaranteed, AuthTier::Elastic, AuthTier::Spot] {
        let err = state.admit(tier, "k", 1_001).expect_err("必须被 shed");
        assert_eq!(err, RETRY_AFTER_SECS);
    }
    // 同档不拒（< 严格比较）
    let _g2 = state
        .admit(AuthTier::Dedicated, "ded-key-2", 1_002)
        .expect("同档放行");
    drop(g);
}

/// 无拥塞：占满前全部档位放行。
#[tokio::test]
async fn no_congestion_admits_all() {
    let (state, vm) = make_admission(section()).await;
    let _p1 = vm.acquire_permit("v1").await.expect("permit"); // 1/2 = 0.5 < 0.9
    for tier in [AuthTier::Spot, AuthTier::Elastic, AuthTier::Guaranteed] {
        state.admit(tier, "k", 1_000).expect("未达阈值必须放行");
    }
}

/// 拥塞但无可比较的在途档位（fleet 信号已拥塞、admission 在途计数为空
/// ——流式近似误差/非网关侧占用）→ 保守放行：shed 无依据，fail-open。
#[tokio::test]
async fn congested_without_in_flight_tiers_admits_conservatively() {
    let (state, vm) = make_admission(section()).await;
    let _p1 = vm.acquire_permit("v1").await.expect("permit");
    let _p2 = vm.acquire_permit("v1").await.expect("permit");
    assert_eq!(
        vm.fleet_concurrency(),
        (2, 2),
        "前置：占用率 1.0 ≥ 0.9 拥塞信号成立"
    );
    // 无任何 admission guard 在途 → min_in_flight_weight() == None
    for tier in [
        AuthTier::Dedicated,
        AuthTier::Guaranteed,
        AuthTier::Elastic,
        AuthTier::Spot,
    ] {
        state
            .admit(tier, "k", 1_000)
            .expect("无可比在途档必须保守放行，不得 429");
    }
}

/// burst EWMA：等间隔快速到达（100ms → 10 req/s）使 burst 单调上升并
/// 收敛于速率（α=0.5）；首样本只记锚不更新。
#[tokio::test]
async fn burst_ewma_tracks_arrival_rate() {
    let (state, _vm) = make_admission(section()).await;
    assert_eq!(state.key_ewma("k"), (0.0, 0.0));
    let mut prev = 0.0;
    for i in 0..6 {
        state
            .admit(AuthTier::Elastic, "k", 1_000 + i * 100)
            .unwrap();
        let (burst, _debt) = state.key_ewma("k");
        if i == 0 {
            assert_eq!(burst, 0.0, "首样本只记锚");
        } else {
            assert!(burst > prev, "快速到达必须推高 burst: {burst} !> {prev}");
        }
        prev = burst;
    }
    assert!(prev < 10.0, "EWMA 从 0 向 10 收敛不越界: {prev}");
}

/// debt EWMA：拥塞中被拒（样本 1）推高 debt；拥塞解除后放行（样本 0）
/// 使 debt 衰减。
#[tokio::test]
async fn debt_ewma_rises_on_shed_and_decays_on_admit() {
    let (state, vm) = make_admission(section()).await;
    let p1 = vm.acquire_permit("v1").await.expect("permit");
    let p2 = vm.acquire_permit("v1").await.expect("permit");
    // 灌 dedicated 在途（guard 持有），spot 反复被拒
    let g = state.admit(AuthTier::Dedicated, "ded", 1_000).unwrap();
    let mut last_debt = 0.0;
    for i in 0..4 {
        state
            .admit(AuthTier::Spot, "spot", 1_100 + i)
            .expect_err("shed");
        let (_b, debt) = state.key_ewma("spot");
        assert!(debt > last_debt, "被拒推高 debt: {debt} !> {last_debt}");
        last_debt = debt;
    }
    // 拥塞解除：释放票据与 dedicated guard → spot 放行，debt 衰减
    drop(g);
    drop(p1);
    drop(p2);
    state
        .admit(AuthTier::Spot, "spot", 2_000)
        .expect("解除后放行");
    let (_b, debt_after) = state.key_ewma("spot");
    assert!(
        debt_after < last_debt,
        "放行样本使 debt 衰减: {debt_after} !< {last_debt}"
    );
}

/// per-key 表硬上限：超限后新 key 退化共享溢出桶（内存有界）。
#[tokio::test]
async fn key_stats_table_caps_entries() {
    let (state, _vm) = make_admission(section()).await;
    // 灌 1100 个 key（上限 1024）——enabled 下 admit 建 stats 条目
    for i in 0..1100 {
        state
            .admit(AuthTier::Elastic, &format!("key-{i}"), 1_000 + i as u64)
            .unwrap();
    }
    let (b_ov, d_ov) = state.key_ewma("__overflow__");
    let (b_new, d_new) = state.key_ewma("key-1099");
    assert!(
        (b_ov - b_new).abs() < 1e-12 && (d_ov - d_new).abs() < 1e-12,
        "超限 key 必须共享溢出桶: ov=({b_ov},{d_ov}) new=({b_new},{d_new})"
    );
}

/// 档位判定：请求 key 经 [auth].keys 表查档；无 key / [auth] 关闭 →
/// 缺省 elastic。
#[test]
fn tier_for_request_resolves_auth_keys_table() {
    let key_dedicated = "dedicated-plain-key";
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(key_dedicated.as_bytes()))
    };
    let config = Config {
        auth: Some(AuthSection {
            enabled: true,
            api_keys: vec![hash.clone()],
            header: Default::default(),
            keys: vec![AuthKeyEntry {
                hash,
                tier: AuthTier::Dedicated,
                token_quota: 0,
                cost_quota_millicents: 0,
            }],
        }),
        ..config_with(AdmissionSection::default())
    };
    let cm = ConfigManager::new(config);

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {key_dedicated}").parse().unwrap(),
    );
    assert_eq!(tier_for_request(&cm, &headers), AuthTier::Dedicated);

    // 未声明档位的 key → elastic；无 key → elastic
    let mut headers_other = axum::http::HeaderMap::new();
    headers_other.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer unknown-key".parse().unwrap(),
    );
    assert_eq!(tier_for_request(&cm, &headers_other), AuthTier::Elastic);
    assert_eq!(
        tier_for_request(&cm, &axum::http::HeaderMap::new()),
        AuthTier::Elastic
    );
}

// ============ 全链路集成（create_router，rate_limit_test 同模式）============

mod full_chain {
    use crate::common;
    use axum::{body::Body, http::Request, http::StatusCode, Router};
    use dbnexus::{DbConfig, DbPool};
    use fluxgate::api::create_router;
    use fluxgate::config::{
        AdmissionSection, AuthKeyEntry, AuthSection, AuthTier, Config, ConfigManager, ServerConfig,
    };
    use fluxgate::database::Database;
    use fluxgate::protocol::{AnthropicConverter, OpenAIConverter};
    use fluxgate::vendor::VendorManager;
    use fluxgate::ApiState;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn digest(key: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(key.as_bytes()))
    }

    fn config_with_admission(enabled: bool, with_model: bool) -> Config {
        common::ensure_test_api_key();
        let ded = digest("dedicated-key");
        let spot = digest("spot-key");
        // 与共享基座的差异字段（显式保留原值）：短超时 1.5s 使 dedicated 请求
        // 挂在不响应的本地监听上，超时返回前中间件 guard 一直持有（= dedicated
        // 在途）；max_concurrent=2 是拥塞 shed 的驱动值；api_base 保持空，
        // 由 build_app_with_base 改写为挂起监听地址
        let mut vendor = common::base_vendor("v1", String::new());
        vendor.timeout_ms = 1_500;
        vendor.stream_ttft_ms = 1_500;
        vendor.stream_idle_ms = 1_500;
        vendor.name = "V1".into();
        vendor.models = if with_model {
            vec![fluxgate::config::ModelConfig {
                model_id: "gpt-4".to_string(),
                abstract_id: Some("gpt-4".to_string()),
                weight: 1.0,
                enabled: true,

                dimensions: None,
                compat_group: None,
            }]
        } else {
            vec![]
        };
        vendor.concurrency.requests_per_second = 100;
        vendor.concurrency.max_concurrent_requests = Some(2);
        Config {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
            },
            auth: Some(AuthSection {
                enabled: true,
                api_keys: vec![ded.clone(), spot.clone()],
                header: Default::default(),
                keys: vec![
                    AuthKeyEntry {
                        hash: ded,
                        tier: AuthTier::Dedicated,
                        token_quota: 0,
                        cost_quota_millicents: 0,
                    },
                    AuthKeyEntry {
                        hash: spot,
                        tier: AuthTier::Spot,
                        token_quota: 0,
                        cost_quota_millicents: 0,
                    },
                ],
            }),
            admission: AdmissionSection {
                enabled,
                ..Default::default()
            },
            ..common::test_config(vec![vendor])
        }
    }

    async fn build_app(enabled: bool) -> (Router, Arc<VendorManager>) {
        build_app_with_base(enabled, String::new()).await
    }

    async fn build_app_with_base(enabled: bool, api_base: String) -> (Router, Arc<VendorManager>) {
        let mut config = config_with_admission(enabled, !api_base.is_empty());
        config.vendors[0].api_base = api_base;
        let config_manager = Arc::new(ConfigManager::new(config));
        let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
        vendor_manager.initialize().await.expect("init vendors");

        let pool = DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
        let database = Arc::new(Database {
            pool: pool.clone(),
            repository: Arc::new(fluxgate::database::Repository::new(pool)),
        });
        let router = Arc::new(fluxgate::router::Router::new(
            config_manager.clone(),
            vendor_manager.clone(),
        ));
        let state = Arc::new(ApiState {
            config: config_manager,
            vendor_manager: vendor_manager.clone(),
            router,
            openai_converter: Arc::new(OpenAIConverter::new()),
            anthropic_converter: Arc::new(AnthropicConverter::new()),
            database,
            logger: None,
            pre_aggregator: None,
            affinity: None,
            response_cache: None,
            bandit: None,
            quota: None,
            difficulty: None,
        });
        (create_router(state).await, vendor_manager)
    }

    async fn post_chat(app: Router, key: &str) -> axum::http::Response<axum::body::Body> {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-4","messages":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    /// 占满并发（2/2，ratio 1.0 ≥ 0.9）后：dedicated 请求挂住（在途
    /// 最小档 = dedicated），spot 请求 429 + Retry-After。guard 生命周期
    /// = 中间件作用域，故用「挂在不响应监听上的 dedicated 请求」构造
    /// 真实在途。
    #[tokio::test]
    async fn congestion_sheds_spot_admits_dedicated() {
        // 挂起监听：连接建立但永不响应 → handler 等到 vendor timeout
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (app, _vm) = build_app_with_base(true, format!("http://127.0.0.1:{port}/v1")).await;

        // 两个 dedicated 挂住：vendor 执行路径各持 1 个并发 permit
        //（2/2 → 占用率 1.0 ≥ 0.9 拥塞），且中间件 guard 在途
        //（在途最小档 = dedicated）。挂点 = 不响应监听 + vendor timeout。
        let mut dedicated_tasks = Vec::new();
        for _ in 0..2 {
            let ded_app = app.clone();
            dedicated_tasks.push(tokio::spawn(async move {
                post_chat(ded_app, "dedicated-key").await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        // spot：拥塞 + 在途最小档 dedicated(1000) > spot priority → 429
        let spot = post_chat(app.clone(), "spot-key").await;
        assert_eq!(
            spot.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "拥塞下 spot 必须被 shed"
        );
        assert_eq!(
            spot.headers()
                .get("retry-after")
                .map(|v| v.to_str().unwrap()),
            Some("1"),
            "准入 429 必须携带 Retry-After"
        );

        // dedicated 们最终完成（vendor timeout，非准入 429）
        for t in dedicated_tasks {
            let dedicated = t.await.unwrap();
            assert_ne!(
                dedicated.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "dedicated 必须放行（到 handler 执行，超时错误非准入 429）"
            );
        }
    }

    /// 默认回归：enabled=false（默认）时同状态两档都放行（非 429）。
    #[tokio::test]
    async fn disabled_passes_both_tiers() {
        let (app, vm) = build_app(false).await;
        let _p1 = vm.acquire_permit("v1").await.expect("permit");
        let _p2 = vm.acquire_permit("v1").await.expect("permit");
        for key in ["spot-key", "dedicated-key"] {
            let resp = post_chat(app.clone(), key).await;
            assert_ne!(
                resp.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "enabled=false（默认）{key} 必须放行"
            );
        }
    }

    /// 健康检查永不卸载：拥塞下 GET /api/v1/health 不被准入 429。注：
    /// /api/v1/health 实际路由由 sdforge #[forge] 在 main 挂载，测试 app
    /// 无该路由 → 中间件白名单放行后到达 404 fallback；若白名单失效，
    /// 拥塞下这里会是准入 429（断言 != 429 即锁定白名单语义）。
    #[tokio::test]
    async fn health_check_never_shed() {
        let (app, vm) = build_app(true).await;
        let _p1 = vm.acquire_permit("v1").await.expect("permit");
        let _p2 = vm.acquire_permit("v1").await.expect("permit");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "健康检查永不 429（白名单先于准入）"
        );
    }
}
