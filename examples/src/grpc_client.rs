// Copyright (c) 2026 Kirky.X🌠
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! NexusFlow gRPC 门面客户端样例。
//!
//! 网关侧（`cargo run --features grpc` + config.toml `[grpc] enabled = true`）
//! 暴露 sdforge 的 `sdforge.v1.SdForgeService` 门面（GetInfo / Call /
//! CallStream，JSON over gRPC）。本样例演示：GetInfo 免认证直取服务信息；
//! Call 无凭据与带凭据两条路径——凭据从 `NEXUSFLOW_API_KEY` 环境变量读取，
//! 源码不落任何 key 材料。
//!
//! 与 HTTP `/health` 白名单的差异以代码为准：门面仅对 Call/CallStream 挂
//! auth_verifier，GetInfo 免认证（NexusFlow src/server/grpc.rs 模块文档；
//! sdforge src/grpc/grpc_impl.rs `get_info` 不经过 `authenticate`）。
//!
//! 本模块整体由 `grpc` feature 门控（见 lib.rs），代码生成经 build.rs 在启用
//! `grpc` 时从 `proto/sdforge.v1.proto` 产出，故 OUT_DIR 引用只在该 feature 下存在。

mod pb {
    include!(concat!(env!("OUT_DIR"), "/sdforge.v1.rs"));
}

use std::collections::HashMap;

use pb::{sd_forge_service_client::SdForgeServiceClient, CallRequest, InfoRequest};
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::Request;

/// 网关 `[grpc]` 段默认端口 50051、host 默认 0.0.0.0（本机回环直连 127.0.0.1）。
const ENDPOINT: &str = "http://127.0.0.1:50051";

/// Call 的 method 名对应网关注册的 `#[forge(grpc_method = "...")]` 处理器。
/// NexusFlow 当前未注册任何业务 grpc_method，因此本样例的 Call 预期终点是
/// `NotFound`；网关注册业务方法后替换为真实方法名，成功响应的 `data` 字段
/// 即 handler 返回值的 JSON 文本。
const DEMO_METHOD: &str = "demo_method";

pub async fn run() {
    let channel = match Channel::builder(ENDPOINT.parse().expect("内置端点必须是合法 URI"))
        .connect()
        .await
    {
        Ok(channel) => channel,
        Err(e) => {
            eprintln!(
                "连接 {ENDPOINT} 失败（网关未启动，或未以 --features grpc 启用 [grpc] 段？）: {e}"
            );
            std::process::exit(1);
        }
    };
    let mut client = SdForgeServiceClient::new(channel);

    // GetInfo 免认证：失败即网关不可用（端口/feature/配置任一不满足），
    // 后续调用必败，fail-fast 终止而非带着坏连接继续演示。
    match client
        .get_info(InfoRequest {
            version: String::new(),
        })
        .await
    {
        Ok(resp) => {
            let info = resp.into_inner();
            println!(
                "GetInfo: {} v{} — {}",
                info.name, info.version, info.description
            );
            println!(
                "可用方法: {}",
                if info.methods.is_empty() {
                    "（无——网关未注册 grpc_method）".to_string()
                } else {
                    info.methods.join(", ")
                }
            );
        }
        Err(status) => {
            eprintln!(
                "GetInfo 失败: code={:?} message={}",
                status.code(),
                status.message()
            );
            std::process::exit(1);
        }
    }

    // 无凭据 Call——认证门演示。[auth] enabled（默认）时被拒为
    // Unauthenticated；enabled=false 时放行进入方法查找。两种观察值都
    // 原样打印，不预设成功也不吞掉失败。
    let unauthenticated = Request::new(CallRequest {
        method: DEMO_METHOD.to_string(),
        parameters: HashMap::new(),
        data: String::new(),
    });
    match client.call(unauthenticated).await {
        Ok(resp) => {
            let call = resp.into_inner();
            println!(
                "[无凭据 Call] 网关放行（[auth] enabled=false？）: success={} data={}",
                call.success, call.data
            );
        }
        Err(status) => {
            println!(
                "[无凭据 Call] 被拒: code={:?} message={}",
                status.code(),
                status.message()
            );
        }
    }

    // 带凭据 Call——`x-api-key` 裸值（服务器侧优先提取）；等价形态是
    // `authorization` metadata 携带 `Bearer <key>`。网关对明文 key 现场算
    // SHA256 摘要后与 [auth].api_keys 常量时间比较。
    let api_key = match std::env::var("NEXUSFLOW_API_KEY") {
        Ok(key) if !key.is_empty() => key,
        Ok(_) => {
            println!("[带凭据 Call] NEXUSFLOW_API_KEY 为空，跳过");
            return;
        }
        Err(std::env::VarError::NotPresent) => {
            println!("[带凭据 Call] 未设置 NEXUSFLOW_API_KEY，跳过");
            return;
        }
        Err(e) => {
            eprintln!("[带凭据 Call] NEXUSFLOW_API_KEY 读取失败: {e}");
            std::process::exit(1);
        }
    };
    let key_value = match MetadataValue::try_from(api_key.as_str()) {
        Ok(value) => value,
        Err(e) => {
            eprintln!("[带凭据 Call] API key 含非法 metadata 字符: {e}");
            std::process::exit(1);
        }
    };
    // parameters 逐键映射为 handler 参数（值恒为字符串）；data 仅在 handler
    // 声明 body 参数时投递（缺声明而 data 非空会被网关拒绝）。
    let mut authenticated = Request::new(CallRequest {
        method: DEMO_METHOD.to_string(),
        parameters: HashMap::from([("prompt".to_string(), "ping".to_string())]),
        data: String::new(),
    });
    authenticated.metadata_mut().insert("x-api-key", key_value);
    match client.call(authenticated).await {
        Ok(resp) => {
            let call = resp.into_inner();
            if call.success {
                println!(
                    "[带凭据 Call] success=true status_code={} data={}",
                    call.status_code, call.data
                );
            } else {
                // 门面内业务失败走响应体（success=false + error 文案），与
                // 传输层 gRPC Status 是两条通道
                println!(
                    "[带凭据 Call] success=false status_code={} error={}",
                    call.status_code, call.error
                );
            }
        }
        Err(status) => {
            // 库存网关无注册方法时的预期终点：NotFound。业务错误的 details
            // 携带 UnifiedError JSON（code/field/trace_id），原样透出供上层解析。
            println!(
                "[带凭据 Call] 失败: code={:?} message={}",
                status.code(),
                status.message()
            );
            let details = status.details();
            if !details.is_empty() {
                println!("  details: {}", String::from_utf8_lossy(details));
            }
        }
    }
}
