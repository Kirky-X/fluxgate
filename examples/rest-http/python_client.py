#!/usr/bin/env python3
# Copyright (c) 2026 Kirky.X🌠
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 NexusFlow Contributors
# Licensed under the MIT License. See LICENSE file for details.

"""官方 openai 库直连网关：base_url 指向网关即可复用认证与路由语义。

依赖：pip install openai（v1.x）
运行：NEXUSFLOW_API_KEY=<明文key> python3 python_client.py
"""

import os
import sys

import openai
from openai import OpenAI

BASE_URL = os.environ.get("NEXUSFLOW_BASE_URL", "http://127.0.0.1:8080")
# 演示值须与网关 [vendors] 配置对齐（仓库自带 config.toml 的 mock 厂商）
MODEL = "gpt-4"
VENDOR = "mock-openai"

# key 无默认值：缺凭据显性退出，不做静默降级
API_KEY = os.environ.get("NEXUSFLOW_API_KEY")
if not API_KEY:
    sys.exit("错误: 缺少环境变量 NEXUSFLOW_API_KEY")

# 网关的 OpenAI 兼容面挂在 /v1 前缀下
client = OpenAI(base_url=f"{BASE_URL}/v1", api_key=API_KEY)


def non_stream() -> None:
    # X-Vendor/X-Model 经 extra_headers 下发，仅 /v1/chat/completions 消费；
    # temperature=0 的确定性请求才可能命中网关响应缓存（响应可见 X-Cache 头）
    resp = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "用一句话介绍响应缓存"}],
        temperature=0,
        extra_headers={"X-Vendor": VENDOR, "X-Model": MODEL},
    )
    content = resp.choices[0].message.content
    if content is None:
        sys.exit(f"错误: 响应缺 choices[0].message.content: {resp.model_dump_json()}")
    print("== 非流式 chat ==")
    print(content)


def stream_chat() -> None:
    print("== SSE 流式 ==")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "从一数到五，用顿号分隔"}],
        stream=True,
    )
    for chunk in stream:
        # 终止前的收尾帧 choices 可能为空，不能直接下标 [0]
        delta = chunk.choices[0].delta.content if chunk.choices else None
        if delta:
            print(delta, end="", flush=True)
    print()


def main() -> None:
    try:
        non_stream()
        stream_chat()
    except openai.APIStatusError as exc:
        # 非 2xx 一律显性失败：透出网关状态码与错误消息
        sys.exit(f"错误: 网关返回 HTTP {exc.status_code}: {exc.message}")
    except openai.APIConnectionError as exc:
        sys.exit(f"错误: 连接网关失败: {exc}")


if __name__ == "__main__":
    main()
