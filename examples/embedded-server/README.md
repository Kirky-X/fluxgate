# embedded-server：把 NexusFlow 网关嵌入自己的 Rust 应用

`nexusflow` 不仅能作为独立网关进程部署，也能作为库依赖装配进宿主程序。本示例 crate 演示库形态的两种用法：

| bin | 形态 | 适用场景 |
| --- | --- | --- |
| `standalone` | `server::run(Some("config.toml".into()))` 一行起服，阻塞到停机 | 宿主只需要网关本身，不挂自己的路由 |
| `mount_router` | `build_app` 拿到 `Router` 与 `ServerHandle`，merge 宿主路由（`GET /examples-info`）后自行 bind/serve | 网关路由与业务路由共存于同一端口 |

## 运行

```bash
# 仓库根执行（本 crate 是 workspace 成员，依赖走 path = "../.."）
cargo run -p nexusflow-example-embedded-server --bin standalone
cargo run -p nexusflow-example-embedded-server --bin mount_router
```

`mount_router` 启动后验证宿主路由与网关共存：

```bash
curl http://127.0.0.1:8080/examples-info
```

## 配置准备

- 两个 bin 都从**运行目录**读取 `config.toml`；可从仓库根 `config.example.toml` 复制为起点。
- 缺少配置文件或配置非法时直接报错退出（非零退出码），没有默认配置回退。
- 监听地址取 `[server]` 段的 `host` / `port`（示例配置为 `0.0.0.0:8080`）。

## 与独立部署（nexusflow 二进制 / docker）的差异

- **进程形态**：独立部署里网关 main 二进制自带参数解析、日志初始化、Ctrl+C/SIGTERM 监听与 flush 收尾，进程即网关；嵌入形态下进程生命周期由宿主持有——`standalone` 把这些职责整体交给 `run()`，`mount_router` 则由宿主自行 serve 并负责收尾。
- **两条进程级约束**（嵌入形态必须遵守，独立部署由 main 二进制代为保证）：
  1. 进程退出前必须调用 `ServerHandle::shutdown().await`，否则日志缓冲丢失、flush worker 泄漏（`standalone` 的 `run()` 内部已处理）。
  2. 同一进程 `run` / `build_app` 合计只能调用一次——内部全局态（OnceCell）二次写入即 panic。
- **依赖形态**：嵌入用 `nexusflow = { path = "../.." }`（默认 `full` feature）随源码编译；独立部署用发布产物/镜像。默认 `full` feature 不含 gRPC 服务器，需要 gRPC 时须自行启用 `grpc` feature 并经 `ServerHandle::take_grpc_server` 取出启动。
