---
name: Bug 报告
about: 报告网关功能缺陷(非安全漏洞请勿在此提交安全问题)
title: "[bug] "
labels: ["bug"]
assignees: []
---

**描述**

<!-- 简明描述遇到的问题 -->

**复现步骤**

```bash
# 请求示例（curl / 客户端调用），或触发问题的操作顺序
curl -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model": "...", "messages": [...]}'
```

**期望行为 / 实际行为**

<!-- 期望发生什么 / 实际发生了什么(含完整错误响应与网关日志片段) -->

**环境**

- 部署方式（二进制 / docker-compose）:
- 版本或 commit:
- OS:
- 配置要点（`config.toml` 相关段，**脱敏**：删除 api_keys 摘要与上游地址）:

<!-- 安全漏洞请勿公开发布，使用 GitHub Security Advisories 私密披露 -->
