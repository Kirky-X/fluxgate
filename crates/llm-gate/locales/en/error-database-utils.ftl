# en/error-database-utils: src/error.rs + src/{database,utils} 用户可见消息。
# error-*: NexusFlowError 变体本地化消息（IntoResponse 展示轨道，Display 恒英文规范串）。
error-config = Configuration error: { $message }
error-protocol = Protocol conversion error: { $source }
error-vendor-unavailable = Vendor unavailable: { $vendor }
error-timeout = Request timeout: { $vendor }
error-rate-limited = Rate limited: { $vendor }
error-database = Database error: { $message }
error-validation = Request validation failed: { $field }
error-routing = Routing error: { $message }
error-all-vendors-throttled = All candidate vendors rate limited: retry after { $retry_after_secs }s
# error-* -source: From 转换时写入 Database 变体 message 字段的前缀。
error-database-source = Database error: { $error }
error-io-source = IO error: { $error }
# database-*: database.rs 初始化路径与 async_logger/pre_aggregator 运维消息。
database-create-dir = Failed to create database directory: { $error }
database-connect = Failed to connect to database: { $error }
database-session = Failed to acquire database session: { $error }
database-init-schema = Failed to initialize schema: { $error }
database-logger-shutdown = Logger already shut down
database-logger-channel-closed = Async logger channel closed
database-logger-flush-failed = Failed to write request logs (batch of { $count }): { $error }
database-logger-secret-scan-dropped = Log dropped by outbound secret scan (total { $total }): { $error }
database-preagg-flush-failed = Failed to flush token aggregation: { $error }
database-preagg-final-flush-failed = Final token aggregation flush failed: { $error }
# utils-*: metrics.rs 运维告警。
utils-metrics-concentration = traffic concentration alert: routing may have collapsed onto a single vendor/model
database-backend-missing = database.url is empty and the sqlite fallback for path { $path } requires the db-sqlite feature: rebuild with --features "full,db-sqlite", or set [database] url to a full connection URL
