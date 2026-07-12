// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use std::net::SocketAddr;
use std::sync::Arc;

use nexusflow::api::{init_global_state, ApiState};
use nexusflow::config::loader::ConfigLoader;
use nexusflow::config::ConfigManager;
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
    println!("Loading config from: {}", config_path);
    let config_manager = Arc::new(ConfigLoader::new(Some(config_path)).load()?);
    println!("Config loaded successfully");

    let db_config = {
        let config = config_manager.read();
        config.database.clone()
    };
    println!("Initializing database at: {}", db_config.path);
    let database = Arc::new(Database::new(&db_config.path, db_config.pool_size).await?);
    println!("Database initialized");

    let vendor_manager = Arc::new(VendorManager::new(config_manager.clone()));
    vendor_manager.initialize().await?;
    println!("Vendor manager initialized");

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
    println!("Global state initialized");

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

    let addr = SocketAddr::from((
        server_config.host.parse::<std::net::Ipv4Addr>().unwrap(),
        server_config.port,
    ));

    tracing::info!("Starting server: {}", addr);
    println!("Server listening on {} (sdforge powered)", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let result = axum::serve(listener, app).await;

    match result {
        Ok(_) => println!("Server stopped successfully"),
        Err(e) => println!("Server stopped with error: {}", e),
    }

    Ok(())
}
