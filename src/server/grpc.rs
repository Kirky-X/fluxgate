// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! gRPC 服务器：sdforge grpc 门面装配与优雅停机 serve（停接新连接、等
//! in-flight 完成后返回）。服务面（v1）：`sdforge.v1.SdForgeService`
//!（GetInfo/Call/CallStream，JSON over gRPC）；Call/CallStream 经 per-call
//! `auth_verifier` 校验（[auth] 段 SHA256 摘要 + 常量时间比较），GetInfo 免认证。
//! TLS 不经 sdforge 终结，由外部反代承担。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{ConfigManager, GrpcConfig};
use crate::error::FluxgateError;
use crate::error::GateError;
use crate::i18n::t_args;
use crate::middleware::auth::ApiAuthState;
use crate::server::bootstrap;

/// 已装配未启动的 gRPC 服务器；[grpc] 缺席或 `enabled=false` 时 [`build`] 返回 `None`。
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

    /// 起服并阻塞至优雅停机：signal 完成后停接新连接并等 in-flight 请求完成。
    pub async fn serve(self, signal: impl Future<Output = ()>) -> Result<(), FluxgateError> {
        let addr = self.addr.to_string();
        tracing::info!(
            "{}",
            t_args("main-grpc-starting", &[("addr", addr.clone())])
        );
        sdforge::grpc::build_server_with_graceful_shutdown(&addr, self.config, signal)
            .await
            .map_err(|e| {
                FluxgateError::from(GateError::Config {
                    message: t_args("main-grpc-server-error", &[("error", e.to_string())]),
                })
            })?;
        tracing::info!("{}", t_args("main-grpc-stopped", &[("addr", addr)]));
        Ok(())
    }
}

/// [grpc] 段装配：启用则产出 [`GrpcServer`]，禁用/缺段返回 `None`。
///
/// `assembly` 仅用于签名对齐；gRPC 认证走热读 config，不持有 `ApiState`。
pub(crate) fn build(
    config_manager: &Arc<ConfigManager>,
    assembly: &bootstrap::Assembly,
) -> Result<Option<GrpcServer>, FluxgateError> {
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

/// 监听地址解析（host 仅接受 IPv4）。
fn grpc_addr(grpc_config: &GrpcConfig) -> Result<SocketAddr, FluxgateError> {
    let ipv4: std::net::Ipv4Addr =
        grpc_config
            .host
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                FluxgateError::from(GateError::Config {
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
/// `require_auth = false`：认证走 per-call `auth_verifier`（[auth] SHA256 摘要制，
/// 非 JWT）；TLS 恒 None，不经 sdforge 终结。
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
        auth_verifier: Some(Arc::new(FluxgateGrpcAuthVerifier {
            config_manager: Arc::clone(config_manager),
        })),
        ..Default::default()
    }
}

/// 认证桥接：[auth] 段 SHA256 摘要 + 常量时间比较，逐调用热读（热更后下次调用生效）。
/// 凭据 `x-api-key` 优先，其次 `authorization: Bearer`；[auth] 缺段或 enabled=false 放行。
/// `Err` 载荷为固定短语，不含凭据材料。
struct FluxgateGrpcAuthVerifier {
    config_manager: Arc<ConfigManager>,
}

impl sdforge::security::grpc_auth::GrpcAuthVerifier for FluxgateGrpcAuthVerifier {
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
