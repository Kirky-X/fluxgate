//! 运维端点：health / vendors / models。

use nexusflow::sdk::{NexusFlowClientBuilder, NexusFlowSdkError};

fn require_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        Ok(_) => {
            eprintln!("环境变量 {name} 为空，拒绝发出无效请求");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("缺少环境变量 {name}（{err}），先执行 export {name}=<值>");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), NexusFlowSdkError> {
    let client = NexusFlowClientBuilder::new()
        .base_url(require_env("NEXUSFLOW_BASE_URL"))
        .api_key(require_env("NEXUSFLOW_API_KEY"))
        .build()?;

    let health = client.health().await?;
    println!(
        "health: {} | version: {} | vendors: {}",
        health.status, health.version, health.vendors
    );

    println!("--- vendors ---");
    for vendor in client.vendors().await? {
        println!(
            "{} | {} | enabled={}",
            vendor.id, vendor.name, vendor.enabled
        );
    }

    println!("--- models ---");
    for model in client.models().await? {
        println!("{} | vendor={}", model.id, model.vendor);
    }
    Ok(())
}
