// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! gRPC 服务器：GrpcServerConfig 构建、sdforge grpc 门面装配、
//! 优雅停机 serve（镜像 [`crate::server::http`] 的封装方式）。
//!
//! sdforge 只用于装配：`build_server_with_graceful_shutdown` 复用其
//! 全部服务器级配置（认证拦截器/并发/超时/keepalive），signal future
//! 完成后停止接受新连接、等待 in-flight 完成后返回——语义与 HTTP 侧
//! `axum::serve(...).with_graceful_shutdown(...)` 逐位对齐。
//!
//! 服务面（v1）：sdforge 自带 `sdforge.v1.SdForgeService` 门面
//! （GetInfo / Call / CallStream，JSON over gRPC），无自有 proto。
//! 认证：门面 Call/CallStream 经 per-call `auth_verifier` 校验
//! （`x-api-key` 或 `authorization: Bearer`，复用 [auth] 段 SHA256
//! 摘要 + 常量时间比较，语义对齐 HTTP `api_auth_middleware`）；
//! GetInfo 免认证（对应 HTTP `/health` 白名单位置，sdforge 门面
//! 现状如此，见 sdforge src/grpc/grpc_impl.rs `authenticate` 仅覆盖
//! call/call_stream）。
//!
//! TLS 不经 sdforge 终结（明文监听，由外部反代承担），与 HTTP 同策略。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{ConfigManager, GrpcConfig};
use crate::error::GateError;
use crate::error::NexusFlowError;
use crate::i18n::t_args;
use crate::middleware::auth::ApiAuthState;
use crate::server::bootstrap;

/// gRPC 服务器（已装配，未启动）。
///
/// 由 [`build`] 产出（[grpc] 段缺席或 `enabled=false` 时为 `None`）；
/// [`serve`](GrpcServer::serve) 消费之，返回即监听已释放。
pub struct GrpcServer {
    addr: SocketAddr,
    config: sdforge::grpc::GrpcServerConfig,
}

impl std::fmt::Debug for GrpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcServer")
            .field("addr", &self.addr)
            .field("timeout_seconds", &self.config.timeout_seconds)
            .field("max_connections", &self.config.max_connections)
            .finish_non_exhaustive()
    }
}

impl GrpcServer {
    /// 监听地址（供调用方就绪探测/日志）。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 起服并阻塞至优雅停机完成：signal future 完成后停止接受新连接、
    /// 等待 in-flight 请求完成后返回；永不完成的 signal 即永驻。
    pub async fn serve(self, signal: impl Future<Output = ()>) -> Result<(), NexusFlowError> {
        let addr = self.addr.to_string();
        tracing::info!(
            "{}",
            t_args("main-grpc-starting", &[("addr", addr.clone())])
        );
        sdforge::grpc::build_server_with_graceful_shutdown(&addr, self.config, signal)
            .await
            .map_err(|e| {
                NexusFlowError::from(GateError::Config {
                    message: t_args("main-grpc-server-error", &[("error", e.to_string())]),
                })
            })?;
        tracing::info!("{}", t_args("main-grpc-stopped", &[("addr", addr)]));
        Ok(())
    }
}

/// [grpc] 段装配：启用则产出 [`GrpcServer`]，禁用/缺段返回 `None`
///（不监听 gRPC 端口，行为与历史逐位一致）。
///
/// `assembly` 当前仅用于签名对齐（gRPC 认证走热读 config，不持有
/// `ApiState`）；保留参数使后续业务 service 挂载（`extra_services`）
/// 无需改调用方签名。
pub(crate) fn build(
    config_manager: &Arc<ConfigManager>,
    assembly: &bootstrap::Assembly,
) -> Result<Option<GrpcServer>, NexusFlowError> {
    let _ = assembly;
    let grpc_config = {
        let config_ref = config_manager.read();
        match config_ref.grpc.as_ref() {
            Some(g) if g.enabled => g.clone(),
            _ => return Ok(None),
        }
    };
    let addr = grpc_addr(&grpc_config)?;
    let config = sdforge_grpc_config(config_manager, &grpc_config);
    Ok(Some(GrpcServer { addr, config }))
}

/// 监听地址解析（host 仅接受 IPv4，与 HTTP 侧 main-invalid-server-host
/// 同口径）。
fn grpc_addr(grpc_config: &GrpcConfig) -> Result<SocketAddr, NexusFlowError> {
    let ipv4: std::net::Ipv4Addr =
        grpc_config
            .host
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                NexusFlowError::from(GateError::Config {
                    message: t_args(
                        "main-grpc-invalid-host",
                        &[("host", grpc_config.host.clone()), ("error", e.to_string())],
                    ),
                })
            })?;
    Ok(SocketAddr::from((ipv4, grpc_config.port)))
}

/// GrpcServerConfig 构建（[grpc] 段 → sdforge 门面配置）。
///
/// - `require_auth = false`：关掉服务器级 BearerAuth(JWT) 拦截器（NexusFlow
///   认证是 [auth] 段 SHA256 摘要制，非 JWT；verifier 走 per-call，不受
///   require_auth 门控——sdforge build_server_inner 仅以 require_auth &&
///   auth.is_none() 做 fail-fast）。
/// - `auth_verifier`：`NexusFlowGrpcAuthVerifier`，Call/CallStream 逐调用
///   校验（GetInfo 免认证，见模块文档）。
/// - TLS 恒 None：NexusFlow 不经 sdforge 终结 TLS，由外部反代承担。
fn sdforge_grpc_config(
    config_manager: &Arc<ConfigManager>,
    grpc: &GrpcConfig,
) -> sdforge::grpc::GrpcServerConfig {
    sdforge::grpc::GrpcServerConfig {
        require_auth: false,
        timeout_seconds: grpc.timeout_seconds,
        max_connections: grpc.max_connections,
        http2_keepalive_interval: grpc.http2_keepalive_interval_secs.map(Duration::from_secs),
        http2_keepalive_timeout: grpc.http2_keepalive_timeout_secs.map(Duration::from_secs),
        auth: None,
        auth_verifier: Some(Arc::new(NexusFlowGrpcAuthVerifier {
            config_manager: Arc::clone(config_manager),
        })),
        ..Default::default()
    }
}

/// NexusFlow 认证桥接：[auth] 段 SHA256 摘要 + 常量时间比较，逐调用
/// 热读配置（与 HTTP `api_auth_middleware` 同一构造路径
/// [`ApiAuthState::from_section`]，[auth] 段热更后下一次 gRPC 调用即生效）。
///
/// 凭据提取：`x-api-key` 裸值优先，其次 `authorization: Bearer <key>`
/// （与 HTTP 中间件的 Authorization/Bearer 提取对齐；sdforge 侧以这两个
/// metadata 名传入）。[auth] 缺段或 enabled=false 时放行（validate_key
/// 的既有语义，与 HTTP 中间件逐位一致）。
///
/// `Err` 载荷遵守 sdforge `GrpcAuthVerifier` 契约：固定短语，不含凭据
/// 材料。
struct NexusFlowGrpcAuthVerifier {
    config_manager: Arc<ConfigManager>,
}

impl sdforge::security::grpc_auth::GrpcAuthVerifier for NexusFlowGrpcAuthVerifier {
    fn verify(
        &self,
        authorization: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<sdforge::security::AuthContext, String> {
        let key = api_key
            .map(str::to_string)
            .or_else(|| {
                authorization
                    .and_then(|a| a.strip_prefix("Bearer "))
                    .map(str::to_string)
            })
            .filter(|k| !k.is_empty())
            .ok_or_else(|| "missing api key".to_string())?;
        // 热读：每调用从配置现读 [auth] 段（HTTP 中间件同语义）
        let auth_state = {
            let section = self.config_manager.read().auth.clone();
            match section {
                Some(s) => ApiAuthState::from_section(&s),
                None => ApiAuthState::new(false),
            }
        };
        if !auth_state.validate_key(&key) {
            return Err("invalid api key".to_string());
        }
        Ok(sdforge::security::AuthContext::new(
            None,
            Vec::new(),
            sdforge::security::AuthMetadata::new(None, None),
        ))
    }
}
