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
database-logger-console-secret-gate-dropped = console log rejected by outbound secret gate (dropped, not printed)
database-logger-join-failed = Failed to await async logger writer exit: { $error }
database-preagg-flush-failed = Failed to flush token aggregation: { $error }
database-preagg-final-flush-failed = Final token aggregation flush failed: { $error }
database-cost-usage-query-failed = Failed to query cost usage: { $error }
# utils-*: metrics.rs 运维告警。
utils-metrics-concentration = traffic concentration alert: routing may have collapsed onto a single vendor/model
database-backend-missing = database.url is empty and the sqlite fallback for path { $path } requires the db-sqlite feature: rebuild with --features "full,db-sqlite", or set [database] url to a full connection URL
# repository.rs 缺列补列（V0.1 旧库迁移）。
database-repo-column-migrated = migrated: added missing column { $column } to table { $table }

# storage.rs 契约默认实现：数据面写前沿不承担 admin 查询，显式拒绝。
storage-log-query-unsupported = this RequestLogSink implementation is a data-plane write front and does not support queries (admin reads go through the storage backend implementation)
storage-usage-stats-unsupported = this UsageSink implementation does not provide stats queries (admin reads go through the storage backend implementation)

# vault (key-vault)：保密柜用户可见消息（Display 轨恒英文规范串，此处为展示轨）。
error-vault = Vault error: { $message }
error-vault-not-found = Vault key not found: { $id }
error-vault-unknown-vendor = Unknown vendor: { $vendor }
