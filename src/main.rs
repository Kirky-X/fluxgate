// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::net::SocketAddr;
use std::sync::Arc;

use nexusflow::api::{init_global_state, ApiState};
use nexusflow::config::loader::ConfigLoader;
use nexusflow::database::Database;
use nexusflow::error::NexusFlowError;
use nexusflow::protocol::AnthropicConverter;
use nexusflow::protocol::OpenAIConverter;
use nexusflow::router::Router as RouterStruct;
use nexusflow::vendor::VendorManager;

#[tokio::main]
async fn main() -> Result<(), NexusFlowError> {
    println!("Starting NexusFlow with sdforge...");
    let _logger = inklog::LoggerManager::builder()
        .level("info")
        .console(true)
        .build()
        .await
        .map_err(|e| NexusFlowError::Config {
            message: format!("Logger init failed: {}", e),
        })?;

    let config_path = std::env::args().nth(1).unwrap_or("config.toml".to_string());
    tracing::info!("Loading config from: {}", config_path);
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    tracing::info!("Config loaded successfully");

    let db_config = {
        let config = config_manager.read();
        config.database.clone()
    };
    tracing::info!("Initializing database at: {}", db_config.path);
    let database = Arc::new(Database::new(&db_config.path, db_config.pool_size).await?);
    tracing::info!("Database initialized");

    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await?;
    tracing::info!("Vendor manager initialized");

    // Build trait-kit AsyncKit DI container: oxcache → dbnexus → inklog
    // chain + limiteron governor. Proves all 4 brother libs are DI-compatible.
    #[cfg(all(
        feature = "infrastructure",
        feature = "database",
        feature = "vendor-management",
        feature = "logging"
    ))]
    {
        tracing::info!("Building trait-kit AsyncKit DI container...");
        let di_db_config = dbnexus::foundation::config::DbConfig {
            url: format!("sqlite:{}", db_config.path),
            max_connections: db_config.pool_size as u32,
            min_connections: 1,
            ..Default::default()
        };
        let kit = nexusflow::infrastructure::di::build_kit_with_config(di_db_config).await?;
        nexusflow::infrastructure::di::verify_kit(&kit)?;
        tracing::info!("AsyncKit DI container built: 4/4 modules resolved");
    }

    let router = RouterStruct::new(config_manager.clone(), vendor_manager.clone());

    let api_state = Arc::new(ApiState {
        config: config_manager.clone(),
        vendor_manager: vendor_manager.clone(),
        router: Arc::new(router),
        client: reqwest::Client::new(),
        openai_converter: Arc::new(OpenAIConverter::new()),
        anthropic_converter: Arc::new(AnthropicConverter::new()),
        database: database.clone(),
    });

    init_global_state(api_state);
    tracing::info!("Global state initialized");

    // Build app with sdforge's configuration
    let sdforge_config = {
        let config_ref = config_manager.read();

        // Build basic config with all required fields
        let config = sdforge::config::AppConfig {
            // Override server config with required fields
            server: sdforge::config::ServerConfig {
                host: config_ref.server.host.clone(),
                port: config_ref.server.port,
                request_timeout_secs: 300,
                cors: Some(sdforge::config::CorsConfig {
                    allowed_origins: vec!["*".to_string()],
                    allowed_methods: vec!["GET".to_string(), "POST".to_string()],
                    allowed_headers: vec!["*".to_string()],
                }),
            },
            // Enable authentication
            authentication: sdforge::config::AuthConfig::ApiKey {
                header_name: "Authorization".to_string(),
                prefix: "Bearer".to_string(),
            },
            timeout: Some(sdforge::config::TimeoutConfig::default()),
        };

        config
    };

    let app = sdforge::http::build_with_config(&sdforge_config)?;

    let server_config = {
        let config = config_manager.read();
        config.server.clone()
    };

    let ipv4: std::net::Ipv4Addr = server_config.host.parse().map_err(|e| {
        NexusFlowError::Config {
            message: format!("invalid server host '{}': {}", server_config.host, e),
        }
    })?;
    let addr = SocketAddr::from((ipv4, server_config.port));

    tracing::info!("Starting server: {}", addr);
    tracing::info!("Server listening on {} (sdforge powered)", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Press Ctrl+C to initiate graceful shutdown");

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    match result {
        Ok(_) => tracing::info!("Server stopped successfully"),
        Err(e) => {
            tracing::error!("Server stopped with error: {}", e);
            return Err(NexusFlowError::Config {
                message: format!("server error: {}", e),
            });
        }
    }

    Ok(())
}

/// 监听终止信号触发优雅停机：先收到 SIGTERM/Ctrl+C 后 axum 会停止接受新连接，
/// 等待 in-flight 请求完成再退出。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sig = signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        sig.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, initiating graceful shutdown"),
        _ = terminate => tracing::info!("Received SIGTERM, initiating graceful shutdown"),
    }
}
