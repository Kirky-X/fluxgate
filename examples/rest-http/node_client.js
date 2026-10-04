// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

/**
 * 官方 openai npm 包直连网关：baseURL 指向网关即可复用认证与路由语义。
 * 依赖：npm install openai（Node ≥ 18）；本目录 package.json 已声明 type: module。
 * 运行：NEXUSFLOW_API_KEY=<明文key> node node_client.js
 */
import OpenAI from "openai";

const baseUrl = process.env.NEXUSFLOW_BASE_URL ?? "http://127.0.0.1:8080";
// 演示值须与网关 [vendors] 配置对齐（仓库自带 config.toml 的 mock 厂商）
const MODEL = "gpt-4";
const VENDOR = "mock-openai";

// key 无默认值：缺凭据显性退出，不做静默降级
const apiKey = process.env.NEXUSFLOW_API_KEY;
if (!apiKey) {
  console.error("错误: 缺少环境变量 NEXUSFLOW_API_KEY");
  process.exit(1);
}

// 网关的 OpenAI 兼容面挂在 /v1 前缀下
const client = new OpenAI({ baseURL: `${baseUrl}/v1`, apiKey });

async function nonStream() {
  const resp = await client.chat.completions.create(
    {
      model: MODEL,
      messages: [{ role: "user", content: "用一句话介绍响应缓存" }],
      temperature: 0,
    },
    // X-Vendor/X-Model 经请求级 headers 下发，仅 /v1/chat/completions 消费
    { headers: { "X-Vendor": VENDOR, "X-Model": MODEL } },
  );
  const content = resp.choices[0]?.message?.content;
  if (!content) {
    throw new Error(`响应缺 choices[0].message.content: ${JSON.stringify(resp)}`);
  }
  console.log("== 非流式 chat ==");
  console.log(content);
}

async function streamChat() {
  console.log("== SSE 流式 ==");
  const stream = await client.chat.completions.create({
    model: MODEL,
    messages: [{ role: "user", content: "从一数到五，用顿号分隔" }],
    stream: true,
  });
  for await (const chunk of stream) {
    // 终止前的收尾帧 choices 可能为空，用可选链兜底
    process.stdout.write(chunk.choices[0]?.delta?.content ?? "");
  }
  process.stdout.write("\n");
}

nonStream()
  .then(streamChat)
  .catch((err) => {
    const status = err?.status !== undefined ? `HTTP ${err.status}: ` : "";
    console.error(`错误: ${status}${err?.message ?? err}`);
    process.exit(1);
  });
