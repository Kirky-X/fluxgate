// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(all(feature = "web-server", feature = "logging"))]

//! run 失败分支覆盖：[server] 端口被占 → TcpListener::bind 失败 →
//! io::Error 经 From<io::Error> 映射为 Database 变体冒泡（http.rs::run 的
//! bind `?` 分支）。配置面另带全部通配符的 [cors]（覆盖 sdforge_config 中
//! origin 过滤后为空 → 不挂 CORS 层的分支）。
//!
//! 独立成文件的原因：run 内部 init_global_state（OnceCell 一次性写入），
//! 同进程只能消费一次全局态，每个 run 场景必须独占一个测试二进制。

#![recursion_limit = "256"]

use fluxgate::error::FluxgateError;

#[tokio::test]
async fn run_maps_occupied_server_port_to_error() {
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blocker");
    let port = blocker.local_addr().unwrap().port();

    let tmpdir = tempfile::tempdir().expect("tempdir creatable");
    let dir = tmpdir.path().to_path_buf();
    let config_path = dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[server]
host = "127.0.0.1"
port = {port}

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

[cors]
enabled = true
allowed_origins = ["*"]
allowed_methods = ["GET"]
allowed_headers = ["*"]
"#
        ),
    )
    .expect("write run config");

    let config = config_path.to_string_lossy().to_string();
    let result = fluxgate::server::run(Some(config)).await;

    match result {
        Err(FluxgateError::Database { message }) => {
            assert_ne!(
                message, "error-io-source",
                "io 错误键必须注册进 FTL 目录（缺键回退返回键名）"
            );
            assert!(
                message.contains("os error"),
                "应携带底层 io 错误文本: {message}"
            );
        }
        other => panic!("端口被占应报 Database(io) 错误，实际 {other:?}"),
    }

    drop(blocker);
    let _ = std::fs::remove_dir_all(dir);
}
