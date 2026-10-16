# zh/main: src/main.rs 用户可见消息(CLI 启动输出 / 运维日志 / HTTP 错误文本 / 启动期 panic)

main-starting = 正在使用 sdforge 启动 Fluxgate……
main-logger-init-failed = 日志系统初始化失败：{ $error }
main-loading-config = 正在从 { $path } 加载配置
main-config-loaded = 配置加载成功
main-initializing-database = 正在初始化数据库：{ $path }
main-database-initialized = 数据库初始化完成
main-vendor-manager-initialized = 厂商管理器初始化完成
main-log-pipeline-initialized = 日志管道初始化完成（异步日志器 + token 预聚合器）
main-building-di-container = 正在构建 trait-kit AsyncKit DI 容器……
main-di-container-built = AsyncKit DI 容器构建完成：4/4 个模块全部解析
main-response-cache-enabled = 响应缓存已启用（recId=12，[cache].enabled=true）
main-response-cache-disabled = 响应缓存已停用（recId=12，[cache].enabled=false）
main-global-state-initialized = 全局状态初始化完成
main-starting-server = 正在启动服务器：{ $addr }
main-server-listening = 服务器监听于 { $addr }（由 sdforge 驱动）
main-press-ctrl-c = 按 Ctrl+C 触发优雅停机
main-server-stopped = 服务器已成功停止
main-server-stopped-with-error = 服务器停止时出错：{ $error }
main-server-error = 服务器错误：{ $error }
main-invalid-server-host = 无效的服务器主机地址 '{ $host }'：{ $error }
main-flushing-log-pipeline = 正在刷新日志管道……
main-async-logger-shutdown = 异步日志器停机：{ $error }
main-pre-aggregator-flush = Token 预聚合器最终刷新：{ $error }
main-log-pipeline-flushed = 日志管道已刷新（dropped_logs={ $dropped }）
main-teardown-timeout = 收尾阶段超时：{ $phases }
main-teardown-error = 收尾协调器错误：{ $error }
main-inklog-shutdown = Inklog 关闭：{ $error }
main-inklog-shutdown-skipped = 跳过 Inklog 关闭（未登记 manager 钩子）
main-received-ctrl-c = 收到 Ctrl+C，开始优雅停机
main-received-sigterm = 收到 SIGTERM，开始优雅停机
main-ctrl-c-handler-failed = 安装 Ctrl+C 信号处理器失败
main-sigterm-handler-failed = 安装 SIGTERM 信号处理器失败
main-grpc-invalid-host = 无效的 gRPC 主机地址 '{ $host }'：{ $error }
main-grpc-starting = 正在启动 gRPC 服务器：{ $addr }（由 sdforge 驱动）
main-grpc-stopped = gRPC 服务器已在 { $addr } 优雅停机
main-grpc-server-error = gRPC 服务器错误：{ $error }
main-grpc-drain-timeout = gRPC 排空超时（{ $secs } 秒）；进行中的请求可能已被丢弃
# hot_reload.rs 运维可见 INFO。
main-hot-reload-committed = 配置热重载已生效；变更段：{ $sections }
main-hot-reload-watching = 配置热重载已监听文件：{ $path }
main-hot-reload-watcher-unavailable = 热重载监听启动失败；已继续运行但热重载不可用
main-hot-reload-swap-failed = 配置热重载切换失败：{ $error }
main-hot-reload-baseline-not-adopted = 热重载等价候选未采纳基线（重读内容与内存态不等价或不可读）；外部编辑保护保持生效

# ---- server/{bootstrap,shutdown}.rs 运维日志 ----
main-log-bridge-already-installed = 日志门面桥已安装：{ $error }
# $phase 为固定序收尾阶段名（ShutdownPhase 变体），不本地化。
main-teardown-hook-timeout = 收尾 { $phase } 钩子超时
main-teardown-hook-registration-failed = 收尾钩子登记失败：{ $error }
# vault（key-vault）：env 吸收计数（有吸收才 INFO，零吸收不打印）。
main-vault-absorbed-keys = 保密柜：已从环境变量吸收 { $absorbed } 个密钥
# 语义缓存（[cache.semantic]）：embedder 加载失败 → 旁路 + ERROR（精确缓存不受影响）。
main-semantic-cache-disabled = 语义缓存已停用：嵌入器加载失败
# 同样旁路，但成因是构建特性缺失（无 $error 字段）。
main-semantic-cache-feature-missing = [cache.semantic].enabled 需要 semantic-cache feature；已旁路该层
# 难度定档分类器（[routing.difficulty]）：laya 加载失败 → F3 常驻纯规则降级。
main-difficulty-classifier-disabled = 定档分类器已停用：laya 加载失败；已进入纯规则模式
