# Tests Directory

**Parent:** `../AGENTS.md`

## OVERVIEW

Rust 集成测试目录，覆盖 API 端点、数据库、协议转换、路由、厂商管理、熔断/重试/准入、SSE、成本追踪、SDK 客户端、server/config 装配与 DI 等功能域。当前 30 个文件、700 个集成用例（`grep -rE -c '#\[(tokio::)?test\b' .` 实测），加上 `src/` 内联单元测试 235 个，合计 935 个，0 ignored。整体行覆盖率 97%+（CI 口径 `cargo llvm-cov --workspace --all-features`）。

## STRUCTURE

```
tests/
├── admission_test.rs              # 拥塞准入中间件
├── api_test.rs                    # API 端点集成测试
├── assembly_smoke_test.rs         # DI 装配冒烟（trait-kit 4 module）
├── bandit_test.rs                 # LinUCB 波段it 路由（求逆/特征/探索）
├── circuit_breaker_test.rs        # 熔断器（limiteron circuit）
├── config_coverage_test.rs        # config loader/manager 分支覆盖
├── config_test.rs                 # 配置加载测试
├── config_toml_example_test.rs    # config.example.toml 可解析性
├── cost_test.rs                   # CostCalculator 定价计算
├── cost_tracker_test.rs           # CostTracker 分片累积
├── database_test.rs               # 数据库操作测试
├── di_test.rs                     # infrastructure/di 装配与错误面
├── fallback_test.rs               # Fallback 换路链
├── feature_matrix.rs              # feature 门控矩阵
├── grpc_coverage_test.rs          # gRPC 门面 serve/handle 语义（grpc feature）
├── grpc_smoke_test.rs             # gRPC 门面冒烟（真实 tonic 链路）
├── pool_test.rs                   # 出站连接池
├── protocol_test.rs               # 协议转换测试
├── rate_limit_test.rs             # 入站限流
├── retry_test.rs                  # 重试分类与延迟
├── router_test.rs                 # 路由决策测试
├── server_coverage_test.rs        # server::run 真实起服 + SIGTERM 优雅停机
├── server_run_bind_error_test.rs  # run 端口占用错误映射（独占二进制）
├── server_run_invalid_host_test.rs# run 非法 host 错误映射（独占二进制）
├── server_smoke_test.rs           # build_app 装配冒烟
├── sse_test.rs                    # SSE 分帧 + SDK 客户端
├── utils_coverage_test.rs         # metrics/rate_limiter 边界覆盖
├── vendor_circuit_breaker_test.rs # vendor 侧熔断
├── vendor_cost_integration_test.rs# 成本集成
└── vendor_test.rs                 # 厂商管理测试
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| API 流程 | `api_test.rs` | 端点到响应的完整链路 |
| 数据库 | `database_test.rs` | SQLite 写入/查询/迁移 |
| 路由 | `router_test.rs` | 加权选择/亲和/提示路由 |
| 协议 | `protocol_test.rs`, `sse_test.rs` | OpenAI/Anthropic 转换与流分帧 |
| 韧性 | `circuit_breaker_test.rs`, `fallback_test.rs`, `retry_test.rs`, `rate_limit_test.rs`, `admission_test.rs` | 熔断/换路/重试/限流/准入 |
| 成本 | `cost_test.rs`, `cost_tracker_test.rs`, `vendor_cost_integration_test.rs` | 定价/累积/集成 |

## CONVENTIONS

- 使用 `cargo test` 运行所有测试（CI 口径：`cargo test --all-features --all`）
- 测试文件位于 `tests/` 目录（集成测试），单元测试在源码文件内 `#[cfg(test)]`
- Mock 依赖使用 `mockall` / `mockito`
- 时间敏感测试注入 limiteron `MockClock`（需 dev-dependencies 的 `test-clock` feature）
