# zh/main: src/main.rs 用户可见消息(CLI 启动输出 / 运维日志 / HTTP 错误文本 / 启动期 panic)

main-starting = 正在使用 sdforge 启动 NexusFlow……
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
main-received-ctrl-c = 收到 Ctrl+C，开始优雅停机
main-received-sigterm = 收到 SIGTERM，开始优雅停机
main-ctrl-c-handler-failed = 安装 Ctrl+C 信号处理器失败
main-sigterm-handler-failed = 安装 SIGTERM 信号处理器失败
