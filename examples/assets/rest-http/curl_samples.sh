#!/usr/bin/env bash
# Copyright (c) 2026 Kirky.X🌠
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 NexusFlow Contributors
# Licensed under the MIT License. See LICENSE file for details.
#
# NexusFlow 网关 curl 样例：与 src/rest_http.rs 一一对应的线级协议演示。
# 用法: NEXUSFLOW_API_KEY=<明文key> bash curl_samples.sh

set -euo pipefail

BASE_URL="${NEXUSFLOW_BASE_URL:-http://127.0.0.1:8080}"
# :? 使 key 缺失时立即带提示退出，不做静默默认
: "${NEXUSFLOW_API_KEY:?错误: 缺少环境变量 NEXUSFLOW_API_KEY}"
# 演示值须与网关 [vendors] 配置对齐（本地运行配置 config.toml 的 mock 厂商；
# 该文件不入库，随仓全量参考见 config.example.toml）
MODEL="gpt-4"
VENDOR="mock-openai"
ANTHROPIC_MODEL="claude-3-opus"

chat_body() {
  # $1=提问 $2=stream 布尔值；temperature=0 的确定性请求才可能命中响应缓存
  printf '{"model":"%s","messages":[{"role":"user","content":"%s"}],"stream":%s,"temperature":0}' \
    "$MODEL" "$1" "$2"
}

# 非 2xx 时 -f 让 curl 显性失败（set -e 接管），不吞错误
req() {
  curl -sS -f "$BASE_URL$1" -H "Authorization: Bearer ${NEXUSFLOW_API_KEY}" "${@:2}"
}

echo "== 健康检查（免认证；裸 /health 未注册，会 404）=="
req /api/v1/health
printf '\n\n'

echo "== 非流式 chat（X-Vendor/X-Model 仅此端点生效，值须命中 [vendors] 配置）=="
req /v1/chat/completions -H "Content-Type: application/json" \
  -H "X-Vendor: $VENDOR" -H "X-Model: $MODEL" \
  -d "$(chat_body "用一句话介绍响应缓存" false)"
printf '\n\n'

echo "== 响应缓存：同一请求连打两次，X-Cache 应 MISS → HIT =="
dump_cache_headers() {
  # 头转存到变量而非直接进管道：curl 失败不会被 grep 的空匹配掩盖
  curl -sS -f -D - -o /dev/null "$BASE_URL/v1/chat/completions" \
    -H "Authorization: Bearer ${NEXUSFLOW_API_KEY}" \
    -H "Content-Type: application/json" \
    -d "$(chat_body "用一句话介绍响应缓存" false)"
}
headers="$(dump_cache_headers)"
grep -i '^x-cache' <<<"$headers" || echo '（无 X-Cache 头：网关未启用 [cache] 或请求未参与缓存）'
headers="$(dump_cache_headers)"
grep -i '^x-cache' <<<"$headers" || echo '（第二次仍无 X-Cache 头：检查 [cache] 配置）'
printf '\n'

echo "== X-No-Cache: 1 强制回源（缓存读/写都跳过）=="
headers="$(curl -sS -f -D - -o /dev/null "$BASE_URL/v1/chat/completions" \
  -H "Authorization: Bearer ${NEXUSFLOW_API_KEY}" \
  -H "Content-Type: application/json" -H "X-No-Cache: 1" \
  -d "$(chat_body "用一句话介绍响应缓存" false)")"
if grep -qi '^x-cache' <<<"$headers"; then
  echo '异常：X-No-Cache: 1 之下仍出现 X-Cache 头' >&2
  exit 1
fi
echo '（符合预期：无 X-Cache 头）'
printf '\n'

echo "== SSE 流式（-N 关闭 curl 缓冲；结束标记为 data: [DONE]，错误帧为 event: error）=="
curl -sS -N "$BASE_URL/v1/chat/completions" \
  -H "Authorization: Bearer ${NEXUSFLOW_API_KEY}" \
  -H "Content-Type: application/json" \
  -d "$(chat_body "从一数到五，用顿号分隔" true)"
printf '\n\n'

echo "== Anthropic 格式 /v1/messages（网关自动协议互转；不消费 X-Vendor/X-Model）=="
req /v1/messages -H "Content-Type: application/json" \
  -d "{\"model\":\"$ANTHROPIC_MODEL\",\"max_tokens\":256,\"messages\":[{\"role\":\"user\",\"content\":\"用一句话介绍协议互转\"}]}"
printf '\n\n'

echo "== 模型列表：GET /admin/models（需认证）=="
echo "（注意：/v1/models 未在网关注册，会 404，见 README 勘误一节）"
req /admin/models
printf '\n'
