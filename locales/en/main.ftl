# en/main: src/main.rs 用户可见消息(CLI 启动输出 / 运维日志 / HTTP 错误文本 / 启动期 panic)

main-starting = Starting Fluxgate with sdforge...
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
main-teardown-timeout = Teardown phase timeout: { $phases }
main-teardown-error = Teardown coordinator error: { $error }
main-inklog-shutdown = Inklog shutdown: { $error }
main-inklog-shutdown-skipped = Inklog shutdown skipped (no manager hook registered)
main-received-ctrl-c = Received Ctrl+C, initiating graceful shutdown
main-received-sigterm = Received SIGTERM, initiating graceful shutdown
main-ctrl-c-handler-failed = failed to install Ctrl+C handler
main-sigterm-handler-failed = failed to install SIGTERM handler
main-grpc-invalid-host = invalid gRPC host '{ $host }': { $error }
main-grpc-starting = Starting gRPC server: { $addr } (sdforge powered)
main-grpc-stopped = gRPC server stopped gracefully at { $addr }
main-grpc-server-error = gRPC server error: { $error }
main-grpc-drain-timeout = gRPC drain timeout after { $secs }s; in-flight requests may have been dropped
# hot_reload.rs 运维可见 INFO。
main-hot-reload-committed = Hot reload committed; changed sections: { $sections }
main-hot-reload-watching = Hot reload watching config file: { $path }
main-hot-reload-watcher-unavailable = Hot reload watcher failed to start; continuing without hot reload
main-hot-reload-swap-failed = Hot reload swap failed: { $error }
main-hot-reload-baseline-not-adopted = Hot reload baseline not adopted for an equivalent candidate (re-read content not equivalent to memory state, or unreadable); external-edit protection stays active

# ---- server/{bootstrap,shutdown}.rs 运维日志 ----
main-log-bridge-already-installed = log facade bridge already installed: { $error }
# $phase 为固定序收尾阶段名（ShutdownPhase 变体），不本地化。
main-teardown-hook-timeout = teardown { $phase } hook timed out
main-teardown-hook-registration-failed = teardown hook registration failed: { $error }
# vault（key-vault）：env 吸收计数（有吸收才 INFO，零吸收不打印）。
main-vault-absorbed-keys = vault: absorbed { $absorbed } key(s) from env
# 语义缓存（[cache.semantic]）：embedder 加载失败 → 旁路 + ERROR（精确缓存不受影响）。
main-semantic-cache-disabled = semantic cache disabled: embedder load failed
# 同样旁路，但成因是构建特性缺失（无 $error 字段）。
main-semantic-cache-feature-missing = [cache.semantic].enabled requires the semantic-cache feature; layer bypassed
# 难度定档分类器（[routing.difficulty]）：laya 加载失败 → F3 常驻纯规则降级。
main-difficulty-classifier-disabled = difficulty classifier disabled: laya load failed; rule-only mode engaged
