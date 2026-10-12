// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(all(feature = "web-server", feature = "logging"))]

//! server::run 生产起服链路覆盖测试。
//!
//! 新建目的：src/server/http.rs 的 run 主体（bootstrap logger/config 装配、
//! bind/serve、停机广播消费）与 sdforge_config 的 [cors] 过滤/构造分支此前
//! 仅由 build_app 冒烟间接触及，run 主体 0 覆盖。
//!
//! 手段：真实启动 run(Some(config))，用 std TcpStream 轮询 /api/v1/health
//! 直到 200（拿到响应即证明 axum::serve 已 accept、停机信号 handler 已在
//! 其之前注册），随后向本进程发 SIGTERM 触发优雅停机，断言 run 返回 Ok。
//!
//! run 内部经 init_global_state 注册全局态（OnceCell 一次性写入），本文件
//! 进程内仅此一个消费全局态的测试，全部断言收敛在单一 #[tokio::test] 内。

#![recursion_limit = "256"]

use std::io::{Read, Write};
use std::time::Duration;

/// 探测一个当前空闲的回环端口并释放（bind :0 → 读取端口号 → drop）。
/// 消除固定端口的并行 CI / TIME_WAIT 敏感性；极小概率的释放-复用竞态
/// 由 wait_http_ready 的重试窗口吸收。
fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 to find a free port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// run 的配置面：[cors] 混入通配符 "*"（覆盖 sdforge_config 的 origin 过滤
/// 与 CorsConfig 构造分支）、[cache] 开启（覆盖 bootstrap 的 enabled 日志
/// 分支）、[grpc] 段启用（default 构建下仅解析不生效；grpc feature 下覆盖
/// run 的 gRPC 装配/启动/排空路径）。
fn run_config_toml(http_port: u16, grpc_port: u16) -> String {
    format!(
        r#"
[server]
host = "127.0.0.1"
port = {http_port}

[database]
path = ":memory:"
pool_size = 1

[[vendors]]
id = "openai_main"
name = "OpenAI Main"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"
concurrency = {{ requests_per_second = 1000 }}
rate_limit = {{ type = "token", max_tokens = 1000000 }}

[[vendors.models]]
model_id = "gpt-4"
abstract_id = "gpt-4"

[global_model]
enabled = true
strategy = "weighted"

[cors]
enabled = true
allowed_origins = ["https://console.example.com", "*"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Authorization"]

[cache]
enabled = true

[grpc]
enabled = true
host = "127.0.0.1"
port = {grpc_port}
"#
    )
}

/// 轮询直到 /api/v1/health 返回 200。std 阻塞 IO + 短超时，不引入 reqwest
/// 依赖；read_timeout 兜底防响应未回时读挂。
async fn wait_http_ready(addr: std::net::SocketAddr) {
    for _ in 0..200 {
        if let Ok(mut stream) = std::net::TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let request =
                format!("GET /api/v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                if String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200") {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("server did not serve /api/v1/health 200 within 10s at {addr}");
}

/// SIGTERM 触发优雅停机（shutdown_signal 的 terminate 分支）：
/// run 起服 → 真实 HTTP 探活 → kill 自身 → run 排空并返回 Ok。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_serves_http_until_sigterm_then_flushes() {
    let tmpdir = tempfile::tempdir().expect("tempdir creatable");
    let dir = tmpdir.path().to_path_buf();
    let http_port = free_loopback_port();
    let grpc_port = free_loopback_port();
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, run_config_toml(http_port, grpc_port)).expect("write run config");

    let config = config_path.to_string_lossy().to_string();
    let run_task = tokio::spawn(async move { nexusflow::server::run(Some(config)).await });

    wait_http_ready(format!("127.0.0.1:{http_port}").parse().unwrap()).await;

    // 此刻停机广播 handler 已注册（HTTP 200 证明 serve 已 accept），
    // SIGTERM 只会触发优雅停机而非进程默认终止
    let status = std::process::Command::new("kill")
        .args(["-s", "TERM", &std::process::id().to_string()])
        .status()
        .expect("kill(1) should exist on unix");
    assert!(status.success(), "kill -TERM delivery should succeed");

    // 若本测试"无输出地消失"而非断言失败，说明 run 的信号注册路径被破坏，
    // SIGTERM 走了进程默认终止——先查 shutdown_signal 的 handler 注册
    let result = tokio::time::timeout(Duration::from_secs(30), run_task)
        .await
        .expect("run should stop well before 30s after SIGTERM")
        .expect("run task must not panic");
    assert!(result.is_ok(), "优雅停机后 run 必须返回 Ok: {result:?}");

    let _ = std::fs::remove_dir_all(dir);
}
