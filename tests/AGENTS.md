# Tests Directory

**Parent:** `../AGENTS.md`

## OVERVIEW

Rust 集成测试目录，覆盖 API 端点、数据库、协议转换、路由、厂商管理、熔断/重试/准入、SSE、成本追踪、SDK 客户端、server/config 装配与 DI、i18n 键位守卫等功能域。当前 41 个测试文件、762 个集成用例（`cargo test --workspace` 实测；静态 `grep` 计 772，含 grpc feature 门控下不编译的用例），加上 `src/` 内联单元测试 367 个，合计 1129 个，0 ignored。整体行覆盖率以 CI 口径 `cargo llvm-cov --workspace` 实测为准。

## STRUCTURE

```
tests/
├── admission_test.rs               # 拥塞准入中间件
├── api_test.rs                     # API 端点集成测试
├── assembly_smoke_test.rs          # DI 装配冒烟（trait-kit 4 module）
├── config_toml_example_test.rs     # config.example.toml 可解析性
├── database_test.rs                # 数据库操作测试
├── di_test.rs                      # infrastructure/di 装配与错误面
├── fallback_test.rs                # Fallback 换路链
├── feature_matrix.rs               # feature 门控矩阵（应用面组合）
├── grpc_coverage_test.rs           # gRPC 门面 serve/handle 语义（grpc feature）
├── grpc_smoke_test.rs              # gRPC 门面冒烟（真实 tonic 链路）
├── health_metrics_test.rs          # /health 与 /admin/metrics
├── i18n_parity_test.rs             # i18n 键位守卫（en/zh 对齐 + 引用存在性 + 零 Junk）
├── model_catalog_admin_test.rs     # /admin/models CRUD 与全局策略
├── openapi_docs_test.rs            # OpenAPI 文档面
├── rate_limit_test.rs              # 入站限流
├── request_trace_test.rs           # 请求追踪头
├── retry_test.rs                   # 端到端重试语义
├── routing_hints_affinity_test.rs  # 路由提示头与会话亲和
├── server_coverage_test.rs         # server::run 真实起服 + SIGTERM 优雅停机
├── server_run_bind_error_test.rs   # run 端口占用错误映射（独占二进制）
├── server_run_invalid_host_test.rs # run 非法 host 错误映射（独占二进制）
├── server_smoke_test.rs            # build_app 装配冒烟
├── sse_test.rs                     # SSE 分帧 + SDK 客户端
├── common/                         # 共享 helper（全部集成测试同名 mod common 引入）
└── gate_*.rs（18 个）              # 路由半边测试（原 crates/llm-gate/tests/，并入时加 gate_ 前缀）：
    gate_anthropic_cache_test.rs    # Anthropic 缓存块转换
    gate_bandit_test.rs             # LinUCB bandit 路由（求逆/特征/探索）
    gate_circuit_breaker_test.rs    # 熔断器（limiteron circuit）
    gate_config_coverage_test.rs    # config loader/manager 分支覆盖
    gate_config_test.rs             # 配置加载测试
    gate_cost_test.rs               # CostCalculator 定价计算
    gate_cost_tracker_test.rs       # CostTracker 分片累积
    gate_feature_matrix.rs          # feature 门控矩阵（路由半边组合）
    gate_pool_test.rs               # 出站连接池
    gate_price_factor_routing_test.rs # 价格因子路由
    gate_protocol_test.rs           # 协议转换测试
    gate_retry_test.rs              # 重试分类与延迟（MockClock paused clock）
    gate_router_test.rs             # 路由决策测试
    gate_utils_coverage_test.rs     # metrics/rate_limiter 边界覆盖
    gate_vendor_circuit_breaker_test.rs # vendor 侧熔断
    gate_vendor_cost_integration_test.rs # 成本集成
    gate_vendor_key_pool_test.rs    # vendor key 池
    gate_vendor_test.rs             # 厂商管理测试
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| API 流程 | `api_test.rs` | 端点到响应的完整链路 |
| 数据库 | `database_test.rs` | SQLite 写入/查询/迁移 |
| 路由 | `gate_router_test.rs`, `routing_hints_affinity_test.rs` | 加权选择/亲和/提示路由 |
| 协议 | `gate_protocol_test.rs`, `sse_test.rs` | OpenAI/Anthropic 转换与流分帧 |
| 韧性 | `gate_circuit_breaker_test.rs`, `fallback_test.rs`, `gate_retry_test.rs`, `rate_limit_test.rs`, `admission_test.rs` | 熔断/换路/重试/限流/准入 |
| 成本 | `gate_cost_test.rs`, `gate_cost_tracker_test.rs`, `gate_vendor_cost_integration_test.rs` | 定价/累积/集成 |
| i18n | `i18n_parity_test.rs` | 新增用户可见串必须先登记 en 键（zh 同步） |

## CONVENTIONS

- 使用 `cargo test --workspace` 运行所有测试（与 CI 逐字一致；`--all-features` 会打开互斥 db-* 后端必编译失败）
- 测试文件位于 `tests/` 目录（集成测试），单元测试在源码文件内 `#[cfg(test)]`；`gate_*` 测试与 `tests/common/mod.rs` 同名引入共享 helper（内容同一份，各测试二进制独立编译）
- Mock 依赖使用 `mockall` / `mockito`
- 时间敏感测试注入 limiteron `MockClock`（需 dev-dependencies 的 `test-clock` feature）
