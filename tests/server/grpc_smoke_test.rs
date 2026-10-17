// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(all(feature = "grpc", feature = "web-server", feature = "logging"))]

//! gRPC 服务器冒烟 — `build_app` 装配 → `take_grpc_server` → sdforge 门面
//! 起服的真实网络链路契约（tonic Channel，口径同 sdforge grpc 集成测试）：
//! 1. GetInfo 免认证可达（对应 HTTP /health 白名单位置）
//! 2. Call 无 key / 错 key → UNAUTHENTICATED（[auth] 摘要 verifier 拦截）
//! 3. Call 对 key + 注册的 handler method → handler 响应（门面路由通）
//! 4. 停机 signal → serve 返回 Ok → 监听释放 → handle.shutdown 幂等
//!
//! build_app 内部执行 init_global_state（OnceCell 二次 set 即 panic），
//! 全部断言收敛在单一 #[tokio::test] 内。

#[path = "../common/mod.rs"]
mod common;

use std::time::Duration;

use fluxgate::config::{AuthSection, Config, ConfigManager, GrpcConfig, ServerConfig};
use fluxgate::server::build_app;
use sdforge::grpc::sdforge_v1::{
    sd_forge_service_client::SdForgeServiceClient, CallRequest, InfoRequest,
};
use sdforge::grpc::GrpcServerConfig;
use sdforge::tonic::{transport::Channel, Request, Status};
use std::sync::Arc;

/// sha256("smoke-test-key") — 与 server_smoke_test 同源摘要
const SMOKE_KEY_HASH: &str = "e9547242bd6a03bfab1da509ab0da8b70381955dbb0f3a36d0fe292e58a9b9e9";
const SMOKE_KEY: &str = "smoke-test-key";

/// 门面 handler：`fluxgate.health`（#[forge] 经 inventory 注册到
/// SdForgeService 调度表；测试 crate 的注册项与 lib 服务器同进程可见）。
#[sdforge::forge(
    name = "grpc_health",
    version = "v1",
    grpc_method = "fluxgate.health",
    description = "gRPC smoke health handler"
)]
async fn grpc_health() -> Result<serde_json::Value, sdforge::core::ApiError> {
    Ok(serde_json::json!({ "status": "ok" }))
}

fn test_config() -> Config {
    let mut vendor = common::base_vendor("openai_main", String::new());
    // 与共享基座的差异字段（显式保留原值）：锁真实 env 名与历史 vendor 名
    vendor.name = "OpenAI Main".into();
    vendor.api_key_env = "OPENAI_API_KEY".into();
    Config {
        server: ServerConfig {
            host: "127.0.0.1".into(),
            port: 18421,
        },
        auth: Some(AuthSection {
            enabled: true,
            api_keys: vec![SMOKE_KEY_HASH.to_string()],
            keys: vec![],
            header: Default::default(),
        }),
        grpc: Some(GrpcConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            // 预 bind 探测的动态端口在 test_config 无法预知；取非常规
            // 高位端口，与 server_smoke 的 18421（HTTP）同风险口径
            port: 18422,
            ..Default::default()
        }),
        ..common::test_config(vec![vendor])
    }
}

/// 轮询等待 gRPC 端口可连。
async fn wait_ready(addr: std::net::SocketAddr) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gRPC server did not become ready within 5s at {addr}");
}

/// 轮询等待端口不可连（监听已释放）。
async fn wait_closed(addr: std::net::SocketAddr) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gRPC server still listening within 5s after shutdown at {addr}");
}

async fn connect_client(addr: std::net::SocketAddr) -> SdForgeServiceClient<Channel> {
    let channel = tokio::time::timeout(
        Duration::from_secs(5),
        Channel::builder(format!("http://{addr}").parse().unwrap()).connect(),
    )
    .await
    .expect("client connect timeout")
    .expect("client connect failed");
    SdForgeServiceClient::new(channel)
}

#[tokio::test]
async fn grpc_facade_serves_auth_and_shuts_down() {
    let config_manager = Arc::new(ConfigManager::new(test_config()));
    let (_app, mut handle) = build_app(config_manager)
        .await
        .expect("build_app should assemble the full chain");

    // 装配产物：gRPC 已随句柄登记，取出并启动
    let grpc = handle
        .take_grpc_server()
        .expect("[grpc].enabled=true 时 take_grpc_server 必须产出服务器");
    let addr = grpc.addr();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let grpc_task = tokio::spawn(async move {
        grpc.serve(async {
            let _ = shutdown_rx.await;
        })
        .await
        .expect("grpc serve should stop gracefully")
    });
    wait_ready(addr).await;

    // ① GetInfo 免认证（sdforge 门面 authenticate 仅覆盖 call/call_stream）
    {
        let mut client = connect_client(addr).await;
        let info = client
            .get_info(Request::new(InfoRequest {
                version: String::new(),
            }))
            .await
            .expect("GetInfo 应免认证可达（/health 对等白名单位置）");
        let info = info.into_inner();
        assert!(!info.name.is_empty());
    }

    // ② Call 无 key → UNAUTHENTICATED（verifier 拒绝）
    {
        let mut client = connect_client(addr).await;
        let err = client
            .call(Request::new(CallRequest {
                method: "fluxgate.health".into(),
                parameters: Default::default(),
                data: String::new(),
            }))
            .await
            .expect_err("无 key 的 Call 必须被拒");
        assert_eq!(err.code(), Status::unauthenticated("").code());
    }

    // ③ Call 错 key → UNAUTHENTICATED（摘要比对失败）
    {
        let mut client = connect_client(addr).await;
        let mut request = Request::new(CallRequest {
            method: "fluxgate.health".into(),
            parameters: Default::default(),
            data: String::new(),
        });
        request
            .metadata_mut()
            .insert("x-api-key", "wrong-key".parse().expect("metadata value"));
        let err = client
            .call(request)
            .await
            .expect_err("错 key 的 Call 必须被拒");
        assert_eq!(err.code(), Status::unauthenticated("").code());
    }

    // ④ Call 对 key（x-api-key）→ handler 响应（门面路由 + 摘要校验通过）
    {
        let mut client = connect_client(addr).await;
        let mut request = Request::new(CallRequest {
            method: "fluxgate.health".into(),
            parameters: Default::default(),
            data: String::new(),
        });
        request
            .metadata_mut()
            .insert("x-api-key", SMOKE_KEY.parse().expect("metadata value"));
        let response = client
            .call(request)
            .await
            .expect("对 key 的 fluxgate.health Call 应到达 handler");
        let body: serde_json::Value =
            serde_json::from_str(&response.into_inner().data).expect("handler 返回 JSON");
        assert_eq!(body["status"], "ok", "handler 响应内容必须直达调用方");
    }

    // ⑤ 优雅停机：signal → serve Ok → 监听释放 → 再连接失败
    shutdown_tx
        .send(())
        .expect("grpc task should still be alive");
    tokio::time::timeout(Duration::from_secs(10), grpc_task)
        .await
        .expect("graceful stop should return well before 10s")
        .expect("grpc task join");
    wait_closed(addr).await;

    // ⑥ 停机收尾句柄幂等（二次调用 no-op 不 panic）
    handle.shutdown().await;
    handle.shutdown().await;
}

/// GrpcServerConfig 仅用于类型导入校验（Fluxgate 侧不直接构造 sdforge
/// 配置——sdforge_grpc_config 为私有装配函数，此处锁定其公共依赖可达）。
#[test]
fn grpc_config_type_reachable() {
    let _ = GrpcServerConfig::default();
}
