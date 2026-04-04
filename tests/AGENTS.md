# Tests Directory

**Parent:** `../AGENTS.md`

## OVERVIEW

Rust 集成测试目录，包含 API 端点、数据库、协议转换、路由等核心功能的测试。

## STRUCTURE

```
tests/
├── api_endpoints_test.rs       # API 端点测试
├── api_integration_test.rs     # API 集成测试
├── config_test.rs              # 配置加载测试
├── database_test.rs            # 数据库操作测试
├── embedding_test.rs           # Embedding 相关测试
├── protocol_converter_test.rs  # 协议转换测试
├── rate_limiter_test.rs        # 速率限制测试
├── retry_test.rs               # 重试机制测试
├── router_test.rs              # 路由逻辑测试
└── vendor_status_test.rs       # 供应商状态测试
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| API 测试 | `api_endpoints_test.rs`, `api_integration_test.rs` | 完整的 API 流程测试 |
| 数据库测试 | `database_test.rs` | SQLite 操作验证 |
| 路由测试 | `router_test.rs` | 供应商选择逻辑 |
| 协议测试 | `protocol_converter_test.rs` | OpenAI/Anthropic 格式转换 |

## CONVENTIONS

- 使用 `cargo test` 运行所有测试
- 测试文件位于 `tests/` 目录（集成测试）
- 单元测试在源码文件内 `#[cfg(test)]`
- Mock 依赖使用 `mockall` 库
