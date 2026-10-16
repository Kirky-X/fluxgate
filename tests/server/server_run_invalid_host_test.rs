// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.
#![cfg(all(feature = "web-server", feature = "logging"))]

//! run 失败分支覆盖：[server].host 非 IPv4 → AddrParseError 映射为
//! Config 错误且消息插值携带非法 host（http.rs::run 的 main-invalid-server-
//! host 分支）。配置面另带 enabled=false 的 [cors]（覆盖 sdforge_config 中
//! CORS 关闭早退分支）。
//!
//! 独立成文件的原因：run 内部 init_global_state（OnceCell 一次性写入），
//! 同进程只能消费一次全局态，每个 run 场景必须独占一个测试二进制。

#![recursion_limit = "256"]

use fluxgate::error::{FluxgateError, GateError};

#[tokio::test]
async fn run_rejects_non_ipv4_server_host_with_localized_message() {
    let tmpdir = tempfile::tempdir().expect("tempdir creatable");
    let dir = tmpdir.path().to_path_buf();
    let config_path = dir.join("config.toml");
    std::fs::write(
        &config_path,
        r#"
[server]
host = "not-an-ipv4-host"
port = 18429

[database]
path = ":memory:"
pool_size = 1

[[vendors]]
id = "openai_main"
name = "OpenAI Main"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"
concurrency = { requests_per_second = 1000 }
rate_limit = { type = "token", max_tokens = 1000000 }

[cors]
enabled = false
"#,
    )
    .expect("write run config");

    let config = config_path.to_string_lossy().to_string();
    let result = fluxgate::server::run(Some(config)).await;

    match result {
        Err(FluxgateError::Gate(GateError::Config { message })) => {
            assert!(
                message.contains("not-an-ipv4-host"),
                "错误消息必须插值携带非法 host: {message}"
            );
            assert_ne!(
                message, "main-invalid-server-host",
                "键必须注册进 FTL 目录（缺键回退返回键名）"
            );
        }
        other => panic!("非 IPv4 host 应报 Config 错误，实际 {other:?}"),
    }

    let _ = std::fs::remove_dir_all(dir);
}
