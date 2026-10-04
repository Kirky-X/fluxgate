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
        "health: {} | version: {} | timestamp: {}",
        health.status, health.version, health.timestamp
    );

    println!("--- vendors（运行面状态）---");
    for vendor in client.vendors().await? {
        println!(
            "{} | {} | queue={} | key_pool={}",
            vendor.id,
            vendor.status,
            vendor.queue_length,
            vendor
                .key_pool
                .as_ref()
                .map(|p| format!("{}/{}", p.current_index + 1, p.size))
                .unwrap_or_else(|| "n/a".into())
        );
    }

    println!("--- models（能力目录）---");
    for model in client.models().await? {
        println!(
            "{} | ctx={} | in={} out={} {}/1M | tools={}",
            model.model_id,
            model.context_window,
            model.input_price,
            model.output_price,
            model.currency,
            model.supports_tools
        );
    }
    Ok(())
}
