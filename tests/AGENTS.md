# Tests Directory

**Parent:** `../AGENTS.md`

## OVERVIEW

Rust 集成测试目录，覆盖 API 端点、数据库、协议转换、路由、厂商管理、熔断/重试/准入、SSE、成本追踪、SDK 客户端、server/config 装配与 DI、i18n 键位守卫等功能域。当前 42 个测试文件、765 个集成用例（`cargo test --workspace` 实测，default 口径），加上 `src/` 内联单元测试 367 个，合计 1132 个，0 ignored。整体行覆盖率以 CI 口径 `cargo llvm-cov --workspace` 实测为准。

## STRUCTURE

按功能域目录化组织（2026-10 隔离重构）；每个文件仍是一个独立测试二进制，由根 `Cargo.toml` 的 `[[test]]` 显式注册（`autotests = false`）：

```
tests/
├── common/mod.rs                 # 共享 helper（Config/VendorConfig 装配基座，各二进制独立编译）
├── api/                          # 端点集成 / 健康与指标 / 模型目录 admin / OpenAPI / 请求追踪
│   ├── api_test.rs               # API 端点集成测试
│   ├── health_metrics_test.rs    # /health 与 /admin/metrics
│   ├── model_catalog_admin_test.rs # /admin/models CRUD 与全局策略
│   ├── openapi_docs_test.rs      # OpenAPI 文档面
│   └── request_trace_test.rs     # 请求追踪头
├── routing/                      # 路由决策 / bandit / 价格因子 / 提示头与会话亲和
│   ├── gate_bandit_test.rs       # LinUCB bandit 路由（求逆/特征/探索）
│   ├── gate_price_factor_routing_test.rs # 价格因子路由
│   ├── gate_router_test.rs       # 路由决策测试
│   └── routing_hints_affinity_test.rs # 路由提示头与会话亲和
├── resilience/                   # 准入 / 换路 / 熔断 / 重试 / 限流
│   ├── admission_test.rs         # 拥塞准入中间件
│   ├── fallback_test.rs          # Fallback 换路链
│   ├── gate_circuit_breaker_test.rs # 熔断器（limiteron circuit）
│   ├── gate_retry_test.rs        # 重试分类与延迟（MockClock paused clock）
│   ├── gate_vendor_circuit_breaker_test.rs # vendor 侧熔断
│   ├── rate_limit_test.rs        # 入站限流
│   └── retry_test.rs             # 端到端重试语义
├── config/                       # 配置加载 / 示例锁定
│   ├── config_toml_example_test.rs # config.example.toml 可解析性与键存在性
│   ├── gate_config_test.rs       # 配置加载测试
│   └── gate_config_coverage_test.rs # config loader/manager 分支覆盖
├── database/                     # 数据库操作
│   └── database_test.rs          # 数据库操作测试（SQLite 写入/查询/迁移）
├── protocol/                     # 协议转换 / Anthropic 缓存块 / SSE 分帧
│   ├── gate_anthropic_cache_test.rs # Anthropic 缓存块转换
│   ├── gate_protocol_test.rs     # 协议转换测试
│   └── sse_test.rs               # SSE 分帧 + SDK 客户端
├── vendor/                       # 厂商管理 / key 池 / 出站连接池
│   ├── gate_pool_test.rs         # 出站连接池
│   ├── gate_vendor_key_pool_test.rs # vendor key 池
│   └── gate_vendor_test.rs       # 厂商管理测试
├── cost/                         # 定价 / 累积 / 集成
│   ├── gate_cost_test.rs         # CostCalculator 定价计算
│   ├── gate_cost_tracker_test.rs # CostTracker 分片累积
│   └── gate_vendor_cost_integration_test.rs # 成本集成
├── server/                       # 装配冒烟 / DI / gRPC / 真实起服（后四个依赖进程独占）
│   ├── assembly_smoke_test.rs    # DI 装配冒烟（trait-kit 4 module）
│   ├── di_test.rs                # infrastructure/di 装配与错误面
│   ├── grpc_coverage_test.rs     # gRPC 门面 serve/handle 语义（grpc feature）
│   ├── grpc_smoke_test.rs        # gRPC 门面冒烟（真实 tonic 链路，OnceCell 独占进程）
│   ├── server_coverage_test.rs   # server::run 真实起服 + SIGTERM 优雅停机
│   ├── server_run_bind_error_test.rs # run 端口占用错误映射（独占二进制）
│   ├── server_run_invalid_host_test.rs # run 非法 host 错误映射（独占二进制）
│   └── server_smoke_test.rs      # build_app 装配冒烟
├── i18n/                         # i18n 键位守卫
│   └── i18n_parity_test.rs       # en/zh 对齐 + 引用存在性 + 零 Junk
├── matrix/                       # feature 门控矩阵
│   ├── feature_matrix.rs         # 应用面组合矩阵
│   └── gate_feature_matrix.rs    # 路由半边组合矩阵
├── utils/                        # 边界覆盖
│   └── gate_utils_coverage_test.rs # metrics/rate_limiter 边界覆盖
└── guard/                        # 仓库级防回归守卫（扫描 src/，与文件位置无关）
    └── dead_code_guard.rs        # src/ 裸 #[allow(dead_code)]/#[allow(unused…)] 拦截
```

`gate_` 前缀文件源自并入主 crate 的 llm-gate 路由核心测试（并仓历史见 `docs/CHANGELOG.md`），迁移后前缀保留不变——二进制名 = 原文件 stem，`cargo test --test <name>` 过滤器口径零变化。

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| API 流程 | `api/api_test.rs` | 端点到响应的完整链路 |
| 数据库 | `database/database_test.rs` | SQLite 写入/查询/迁移 |
| 路由 | `routing/gate_router_test.rs`, `routing/routing_hints_affinity_test.rs` | 加权选择/亲和/提示路由 |
| 协议 | `protocol/gate_protocol_test.rs`, `protocol/sse_test.rs` | OpenAI/Anthropic 转换与流分帧 |
| 韧性 | `resilience/gate_circuit_breaker_test.rs`, `resilience/fallback_test.rs`, `resilience/gate_retry_test.rs`, `resilience/rate_limit_test.rs`, `resilience/admission_test.rs` | 熔断/换路/重试/限流/准入 |
| 成本 | `cost/gate_cost_test.rs`, `cost/gate_cost_tracker_test.rs`, `cost/gate_vendor_cost_integration_test.rs` | 定价/累积/集成 |
| i18n | `i18n/i18n_parity_test.rs` | 新增用户可见串必须先登记 en 键（zh 同步） |

## CONVENTIONS

- 使用 `cargo test --workspace` 运行所有测试（与 CI 逐字一致；`--all-features` 会打开互斥 db-* 后端必编译失败）
- 测试文件按功能域位于 `tests/<domain>/`（集成测试），单元测试在源码内 `src/` 文件内 `#[cfg(test)]`；`autotests = false`，**新增集成测试文件必须同步在根 `Cargo.toml` 登记 `[[test]]`**（name 取文件 stem、path 指向实际位置），否则 cargo 静默不编译不运行；**禁止给 [[test]] 加 `required-features`**（特性门控留在文件顶 `#![cfg(feature = ...)]`）
- 共享 helper 在 `common/mod.rs`：域内文件以 `#[path = "../common/mod.rs"] mod common;` 显式引入（内容同一份，各测试二进制独立编译）；`gate_*` 与其余消费方同款，不得拷袭基座字面量
- 进程独占语义：`server_run_*` / `grpc_smoke` / `server_coverage` 依赖 OnceCell 全局态同进程仅消费一次，**禁止合并二进制**（域目录仅物理分组，一文件一二进制不变）
- Mock 依赖使用 `mockall` / `mockito`
- 时间敏感测试注入 limiteron `MockClock`（需 dev-dependencies 的 `test-clock` feature）
