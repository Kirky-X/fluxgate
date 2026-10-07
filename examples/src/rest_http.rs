// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 裸 HTTP 直连网关演示：不依赖 nexusflow SDK，直接对线级协议编程。
//!
//! 协议事实以本仓库源码为准：
//! - `GET /api/v1/health` 免认证；裸 `/health` 未注册（认证与限流白名单
//!   只认 `/api/v1/health`）
//! - `POST /v1/chat/completions`：`model` 必填；`X-Vendor`/`X-Model`
//!   路由提示仅此端点消费，值必须命中网关配置（未知厂商直接报错，
//!   router.rs `route_with_hints`）
//! - `POST /v1/messages`：Anthropic 线级格式；网关按目标厂商协议自动
//!   互转，回包恒为 Anthropic 形状（content 为 text block 数组）
//! - SSE：`data: {json}` 帧、`data: [DONE]` 终止；错误帧 `event: error`
//!   之后网关不再补 `[DONE]`，因此「没等到 `[DONE]` 就断流」必须按
//!   失败处理

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Client, Response};
use serde_json::{json, Value};

/// 与 config.toml `[server]` 段默认监听一致
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";

// 演示值须与网关 [vendors] 实际配置对齐；仓库自带 config.toml 的 mock
// 厂商（mock-openai / mock-anthropic，上游 127.0.0.1:8001）开箱即可命中。
// 换部署时改这三个常量。
const HINT_VENDOR: &str = "mock-openai";
const CHAT_MODEL: &str = "gpt-4";
const ANTHROPIC_MODEL: &str = "claude-3-opus";

/// bin 入口调用的顶层函数：把 `execute` 的 `Result` 映射为进程退出码。
pub async fn run() -> ExitCode {
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> Result<()> {
    let base_url = env::var("NEXUSFLOW_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    // API key 无默认值：网关 [auth].enabled 时强制校验，缺凭据必须显性失败
    let api_key = env::var("NEXUSFLOW_API_KEY")
        .map_err(|_| anyhow!("缺少环境变量 NEXUSFLOW_API_KEY（网关 [auth] 段启用的明文 key）"))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {api_key}")
            .parse::<HeaderValue>()
            .context("构造 Authorization 头失败")?,
    );
    let client = Client::builder().build().context("构建 HTTP 客户端失败")?;

    health_check(&client, &base_url).await?;
    chat_completions(&client, &base_url, &headers).await?;
    anthropic_messages(&client, &base_url, &headers).await?;
    chat_stream(&client, &base_url, &headers).await?;
    Ok(())
}

/// 非 2xx 一律显性失败：透出状态码与响应体原文，不做默认值兜底
async fn ensure_success(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow!("网关返回 HTTP {status}: {body}"))
}

async fn health_check(client: &Client, base_url: &str) -> Result<()> {
    let url = format!("{base_url}/api/v1/health");
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("请求 {url} 失败（网关未启动？）"))?;
    let payload: Value = ensure_success(resp)
        .await?
        .json()
        .await
        .context("健康检查响应不是合法 JSON")?;
    println!("== 健康检查 ==\n{payload:#}\n");
    Ok(())
}

/// temperature 缺省或 0 的确定性请求才会进响应缓存（api/response_cache.rs），
/// 借此让响应头的 `X-Cache: MISS→HIT` 可复现
async fn chat_completions(client: &Client, base_url: &str, headers: &HeaderMap) -> Result<()> {
    let body = json!({
        "model": CHAT_MODEL,
        "messages": [{"role": "user", "content": "用一句话介绍响应缓存"}],
        "stream": false,
        "temperature": 0,
    });
    let resp = ensure_success(
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .headers(headers.clone())
            // X-Vendor/X-Model 仅 /v1/chat/completions 消费；值未命中
            // [vendors] 配置时网关直接报错（router.rs route_with_hints）
            .header("X-Vendor", HINT_VENDOR)
            .header("X-Model", CHAT_MODEL)
            .json(&body)
            .send()
            .await
            .context("请求 /v1/chat/completions 失败")?,
    )
    .await?;
    if let Some(cache) = resp.headers().get("x-cache") {
        println!("X-Cache: {}", cache.to_str().unwrap_or("?"));
    }
    let payload: Value = resp.json().await.context("chat 响应不是合法 JSON")?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("响应缺 choices[0].message.content: {payload}"))?;
    println!("== 非流式 chat（X-Vendor={HINT_VENDOR}, X-Model={CHAT_MODEL}）==\n{content}\n");
    Ok(())
}

/// Anthropic 线级格式请求。该端点不消费 X-Vendor/X-Model（anthropic
/// handler 只读 X-Weight），路由只认 body.model；无论路由到的厂商是
/// 何协议，回包都是 Anthropic 形状
async fn anthropic_messages(client: &Client, base_url: &str, headers: &HeaderMap) -> Result<()> {
    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": 256,
        "messages": [{"role": "user", "content": "用一句话介绍协议互转"}],
    });
    let payload: Value = ensure_success(
        client
            .post(format!("{base_url}/v1/messages"))
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .context("请求 /v1/messages 失败")?,
    )
    .await?
    .json()
    .await
    .context("/v1/messages 响应不是合法 JSON")?;
    let text = payload
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("响应缺 content[0].text（Anthropic 形状）: {payload}"))?;
    let stop_reason = payload
        .pointer("/stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("(缺失)");
    println!("== Anthropic 格式 /v1/messages（网关自动协议互转）==\n{text}\nstop_reason: {stop_reason}\n");
    Ok(())
}

/// SSE 流式：`bytes_stream` 手工解析——跨 chunk 字节缓冲、按 `\n` 切行
///（只对完整行解码，多字节 UTF-8 不会被块边界劈坏）、`data: [DONE]`
/// 终止、`event: error` 帧按失败处理
async fn chat_stream(client: &Client, base_url: &str, headers: &HeaderMap) -> Result<()> {
    let body = json!({
        "model": CHAT_MODEL,
        "messages": [{"role": "user", "content": "从一数到五，用顿号分隔"}],
        "stream": true,
    });
    let resp = ensure_success(
        client
            .post(format!("{base_url}/v1/chat/completions"))
            .headers(headers.clone())
            .json(&body)
            .send()
            .await
            .context("请求流式 /v1/chat/completions 失败")?,
    )
    .await?;

    println!("== SSE 流式 ==");
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut in_error_event = false;
    let mut done = false;
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.context("流式传输中断")?);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\r', '\n']);
            // event: 行只记录类型，语义由紧随的 data: 行兑现（错误帧是
            // `event: error` + `data: <msg>`，此后不再发 [DONE]）
            if let Some(event) = line.strip_prefix("event:") {
                in_error_event = event.trim() == "error";
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                done = true;
                break;
            }
            let payload: Value = serde_json::from_str(data)
                .with_context(|| format!("SSE data 行不是合法 JSON: {data}"))?;
            // data 里内嵌 error 对象是上游透传错误的规范形状（sse_frame.rs）
            if in_error_event || payload.get("error").is_some() {
                return Err(anyhow!("网关流内错误: {data}"));
            }
            in_error_event = false;
            let delta = payload
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            print!("{delta}");
            io::stdout().flush().context("刷新 stdout 失败")?;
        }
        if done {
            break;
        }
    }
    if !done {
        // 成功流必然以 [DONE] 收尾；没等到即断流/异常，不得当正常结束
        return Err(anyhow!("流在 [DONE] 之前结束（网关断流或异常终止）"));
    }
    println!();
    Ok(())
}
