// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Anthropic 自动缓存断点注入：位置/数量与超限回退（保留锚序尾部）。

use nexusflow::config::model_catalog::{cache_policy_for, CacheBreakpointPolicy};
use nexusflow::config::{ModelCatalogEntry, PricingCurrency};
use nexusflow::protocol::{
    AnthropicConverter, Message, MessageContent, ProtocolConverter, Role, UnifiedRequest,
};
use serde_json::{json, Value};

fn policy(max: u8, anchors: &[&str]) -> CacheBreakpointPolicy {
    CacheBreakpointPolicy {
        enabled: true,
        max_breakpoints: max,
        anchors: anchors.iter().map(|s| s.to_string()).collect(),
    }
}

fn request(system: &str, turns: &[(&str, &str)]) -> UnifiedRequest {
    UnifiedRequest {
        model: "claude-sonnet-4-20250514".into(),
        system_prompt: if system.is_empty() {
            None
        } else {
            Some(system.into())
        },
        messages: turns
            .iter()
            .map(|(role, text)| Message {
                role: if *role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: MessageContent::Text(text.to_string()),
            })
            .collect(),
        ..Default::default()
    }
}

fn cache_markers(body: &Value) -> Vec<String> {
    // 收集全部 cache_control 出现位置（路径描摹，供断言可读性）
    let mut out = Vec::new();
    if let Some(system) = body.get("system") {
        if system
            .as_array()
            .is_some_and(|a| a.iter().any(|b| b.get("cache_control").is_some()))
        {
            out.push("system".into());
        }
    }
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        if tools.iter().any(|t| t.get("cache_control").is_some()) {
            out.push("tools".into());
        }
    }
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for (i, m) in msgs.iter().enumerate() {
            if m.get("content").is_some_and(|c| {
                c.as_array()
                    .is_some_and(|a| a.iter().any(|b| b.get("cache_control").is_some()))
            }) {
                out.push(format!("messages[{i}]"));
            }
        }
    }
    out
}

fn anthropic_entry() -> ModelCatalogEntry {
    ModelCatalogEntry {
        model_id: "claude-sonnet-4-20250514".into(),
        context_window: 200_000,
        currency: PricingCurrency::Usd,
        supports_tools: true,
        ..Default::default()
    }
}

/// 全锚点：tools + system + last_user 各标记一处；last_user 选**最后一条**
/// user 消息（多轮对话）
#[tokio::test]
async fn injects_all_three_anchors_with_last_user_selection() {
    let converter = AnthropicConverter::new();
    let request = request(
        "You are helpful.",
        &[("user", "q1"), ("assistant", "a1"), ("user", "q2")],
    );
    let mut tools_request = request;
    tools_request.tools = vec![Default::default()];

    let body = converter
        .request_to_external_with_cache_policy(
            tools_request,
            &policy(4, &["tools", "system", "last_user"]),
        )
        .unwrap();

    let markers = cache_markers(&body);
    assert_eq!(markers.len(), 3, "三锚点各一处: {markers:?}");

    // system：字符串提升为单块数组 + cache_control
    let system = body.get("system").unwrap().as_array().unwrap();
    assert_eq!(system.len(), 1);
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(system[0]["text"], "You are helpful.");

    // tools：末块标记
    assert_eq!(
        body["tools"].as_array().unwrap()[0]["cache_control"]["type"],
        "ephemeral"
    );

    // last_user：messages[2]（最后的 user turn）
    assert_eq!(markers.last().unwrap(), "messages[2]");
    let blocks = body["messages"][2]["content"].as_array().unwrap();
    assert_eq!(blocks.last().unwrap()["cache_control"]["type"], "ephemeral");
    assert_eq!(blocks.last().unwrap()["text"], "q2");

    // assistant 消息不标记
    assert!(
        !cache_markers(&body).iter().any(|m| m == "messages[1]"),
        "assistant turn 不得被标记"
    );
    let _ = json!(null); // json! 导入占用（锚点断言以路径读取为主）
}

/// 超限回退：候选锚 3 个、上限 2 → 保留锚序尾部（丢弃头部 tools）；
/// 上限 1 → 仅保留 last_user
#[tokio::test]
async fn overflow_keeps_anchor_order_tail() {
    let converter = AnthropicConverter::new();
    let mut request = request("sys", &[("user", "q")]);
    request.tools = vec![Default::default()];

    let body = converter
        .request_to_external_with_cache_policy(
            request.clone(),
            &policy(2, &["tools", "system", "last_user"]),
        )
        .unwrap();
    let markers = cache_markers(&body);
    assert_eq!(markers.len(), 2, "上限 2：只注入 2 处");
    assert!(
        !markers.iter().any(|m| m == "system") || !body["tools"][0].get("cache_control").is_some(),
        "头部锚（按序 tools 在 system 前）被丢弃——tools 无标记"
    );
    assert!(
        body.get("system")
            .and_then(|s| s.as_array())
            .is_some_and(|a| a[0].get("cache_control").is_some()),
        "锚序尾部保留 system"
    );

    let body = converter
        .request_to_external_with_cache_policy(
            request,
            &policy(1, &["tools", "system", "last_user"]),
        )
        .unwrap();
    let markers = cache_markers(&body);
    assert_eq!(markers, vec!["messages[0]"], "上限 1：仅保留尾部 last_user");
}

/// 开关关闭 / 上限 0 → 零注入（出站形状与 request_to_external 逐位一致）
#[tokio::test]
async fn disabled_or_zero_cap_injects_nothing() {
    let converter = AnthropicConverter::new();
    let mut request = request("sys", &[("user", "q")]);
    request.tools = vec![Default::default()];
    let baseline = converter.request_to_external(request.clone()).unwrap();

    let mut off = policy(4, &["tools", "system", "last_user"]);
    off.enabled = false;
    let body = converter
        .request_to_external_with_cache_policy(request.clone(), &off)
        .unwrap();
    assert_eq!(body, baseline, "关闭时出站形状逐位一致");

    let body = converter
        .request_to_external_with_cache_policy(
            request.clone(),
            &policy(0, &["tools", "system", "last_user"]),
        )
        .unwrap();
    assert_eq!(body, baseline, "上限 0 零注入");

    // 空锚点表同样零注入
    let body = converter
        .request_to_external_with_cache_policy(request.clone(), &policy(4, &[]))
        .unwrap();
    assert_eq!(body, baseline);

    // 未知锚名跳过不 panic
    let body = converter
        .request_to_external_with_cache_policy(request, &policy(4, &["crystal_ball"]))
        .unwrap();
    assert_eq!(body, baseline);
}

/// 缺失锚点跳过：无 tools / 无 system 时仅标记存在的锚
#[tokio::test]
async fn missing_anchors_are_skipped() {
    let converter = AnthropicConverter::new();
    let request = request("", &[("user", "q")]);
    let body = converter
        .request_to_external_with_cache_policy(
            request,
            &policy(4, &["tools", "system", "last_user"]),
        )
        .unwrap();
    let markers = cache_markers(&body);
    assert_eq!(markers, vec!["messages[0]"], "仅 last_user 可标记");
    assert!(body.get("system").is_none(), "无 system 不凭空造段");
    assert!(body.get("tools").is_none());
}

/// 策略解析接线：条目覆盖优先于内置默认；未登记回落默认
#[test]
fn policy_resolution_entry_override_wins() {
    let mut catalog = vec![anthropic_entry()];
    catalog[0].cache_breakpoints = Some(policy(1, &["last_user"]));
    assert_eq!(
        cache_policy_for(&catalog, "claude-sonnet-4-20250514").max_breakpoints,
        1
    );
    assert_eq!(
        cache_policy_for(&catalog, "unregistered").max_breakpoints,
        4,
        "未登记回落内置默认"
    );
}
