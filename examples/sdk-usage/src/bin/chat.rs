//! 非流式 chat：完整 `ChatRequest` + `RequestConfig` 路由提示头，打印用量与实际 vendor。

use std::time::Duration;

use nexusflow::sdk::{
    ChatMessage, ChatRequest, NexusFlowClientBuilder, NexusFlowSdkError, RequestConfig,
};

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
    // timeout() 只约束非流式请求的总时长；流式行为见 stream_chat.rs 的 read_timeout
    let client = NexusFlowClientBuilder::new()
        .base_url(require_env("NEXUSFLOW_BASE_URL"))
        .api_key(require_env("NEXUSFLOW_API_KEY"))
        .timeout(Duration::from_secs(60))
        .build()?;

    // vendor_hint / model_hint 映射 X-Vendor / X-Model 路由头（不进请求 body），
    // 取值须与网关侧 vendor/model 配置一致（样例值来自 config.example.toml）
    let req = ChatRequest {
        model: "gpt-4".into(),
        messages: vec![
            ChatMessage::system("用一句中文回答。"),
            ChatMessage::user("一句话说明什么是熔断器。"),
        ],
        config: RequestConfig {
            temperature: Some(0.2),
            top_p: Some(0.9),
            max_tokens: Some(256),
            stop: Some(vec!["###".into()]),
            user: Some("sdk-usage-example".into()),
            vendor_hint: Some("mock-openai".into()),
            model_hint: Some("gpt-4".into()),
            ..RequestConfig::default()
        },
        tools: Vec::new(),
        tool_choice: None,
        vendor_extensions: Default::default(),
    };

    // 认证失败单列提示（最常见故障是 key 配错），其余错误原样上抛
    let resp = match client.chat(&req).await {
        Ok(resp) => resp,
        Err(NexusFlowSdkError::Auth { message }) => {
            eprintln!("认证失败（{message}）：检查 NEXUSFLOW_API_KEY 是否为网关签发的 key");
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    println!("{}", resp.content);
    println!("---");
    println!(
        "model: {} | finish_reason: {}",
        resp.model,
        resp.finish_reason.as_wire()
    );
    println!(
        "tokens: prompt={} completion={} (cache_read={})",
        resp.usage.prompt_tokens, resp.usage.completion_tokens, resp.usage.cache_read_tokens
    );
    Ok(())
}
