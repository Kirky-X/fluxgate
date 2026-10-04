# en/main: src/main.rs 用户可见消息(CLI 启动输出 / 运维日志 / HTTP 错误文本 / 启动期 panic)

main-starting = Starting NexusFlow with sdforge...
main-logger-init-failed = Logger init failed: { $error }
main-loading-config = Loading config from: { $path }
main-config-loaded = Config loaded successfully
main-initializing-database = Initializing database at: { $path }
main-database-initialized = Database initialized
main-vendor-manager-initialized = Vendor manager initialized
main-log-pipeline-initialized = Log pipeline initialized (async logger + token pre-aggregator)
main-building-di-container = Building trait-kit AsyncKit DI container...
main-di-container-built = AsyncKit DI container built: 4/4 modules resolved
main-response-cache-enabled = Response cache enabled (recId=12, [cache].enabled=true)
main-response-cache-disabled = Response cache disabled (recId=12, [cache].enabled=false)
main-global-state-initialized = Global state initialized
main-starting-server = Starting server: { $addr }
main-server-listening = Server listening on { $addr } (sdforge powered)
main-press-ctrl-c = Press Ctrl+C to initiate graceful shutdown
main-server-stopped = Server stopped successfully
main-server-stopped-with-error = Server stopped with error: { $error }
main-server-error = server error: { $error }
main-invalid-server-host = invalid server host '{ $host }': { $error }
main-flushing-log-pipeline = Flushing log pipeline...
main-async-logger-shutdown = Async logger shutdown: { $error }
main-pre-aggregator-flush = Token pre-aggregator final flush: { $error }
main-log-pipeline-flushed = Log pipeline flushed (dropped_logs={ $dropped })
main-received-ctrl-c = Received Ctrl+C, initiating graceful shutdown
main-received-sigterm = Received SIGTERM, initiating graceful shutdown
main-ctrl-c-handler-failed = failed to install Ctrl+C handler
main-sigterm-handler-failed = failed to install SIGTERM handler
main-grpc-invalid-host = invalid gRPC host '{ $host }': { $error }
main-grpc-starting = Starting gRPC server: { $addr } (sdforge powered)
main-grpc-stopped = gRPC server stopped gracefully at { $addr }
main-grpc-server-error = gRPC server error: { $error }
main-grpc-drain-timeout = gRPC drain timeout after { $secs }s; in-flight requests may have been dropped
