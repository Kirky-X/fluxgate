# en/error-database-utils: src/error.rs + src/{database,utils} 用户可见消息。
# error-*: FluxgateError 变体本地化消息（IntoResponse 展示轨道，Display 恒英文规范串）。
error-config = Configuration error: { $message }
error-io = I/O error: { $message }
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
# vault 载荷键：经 error-vault = Vault error: { $message } 插值展示，展示轨本地化。
# file_store.rs
vault-home-not-found = home directory not found
vault-read-file-failed = read vault file failed: { $error }
vault-file-magic-mismatch = vault file magic mismatch
vault-decode-entries-failed = decode vault entries failed: { $error }
vault-encode-entries-failed = encode vault entries failed: { $error }
vault-seal-file-failed = seal vault file failed: { $error }
vault-create-dir-failed = create vault dir failed: { $error }
vault-create-tmp-failed = create vault temp file failed: { $error }
vault-write-file-failed = write vault file failed: { $error }
vault-sync-file-failed = sync vault file failed: { $error }
vault-replace-file-failed = replace vault file failed: { $error }
# crypto.rs
vault-seal-blob-failed = seal failed: { $error }
vault-open-blob-failed = open failed: { $error } — the stored entry cannot be decrypted with the current master key (FLUXGATE_MASTER_KEY / master key file changed since it was stored); entries are unrecoverable after a master key change, re-enter vendor keys
vault-decrypted-not-utf8 = decrypted bytes are not utf-8: { $error }
# db_store.rs
vault-db-access-failed = vault db access failed: { $error }
# master_key.rs
vault-master-key-hex-len = { $env } must be exactly { $expected } hex chars (32 bytes), got { $actual } chars
vault-master-key-hex-decode-failed = hex decode failed: { $error }
vault-master-key-create-dir-failed = create key dir failed: { $error }
vault-master-key-write-failed = write key file failed: { $error }
vault-master-key-restrict-failed = restrict key file permissions failed: { $error }
storage-affinity-sticks-unsupported = this AffinityStickSink implementation is a data-plane write front and does not support reads (boot restore goes through the storage backend implementation)
database-affinity-persist-flush-failed = affinity stick batch persist failed (dropped { $count } sticks): { $error }
database-affinity-persist-channel-closed = affinity stick persist channel closed: { $error }
database-affinity-sticks-read-failed = affinity stick read failed: { $error }

# ---- masking: 出站掩码（feature masking；错误面 + config 校验面）----
masking-unavailable = Request refused: outbound masking is unavailable (fail-closed, never send raw)
masking-blocked = Request blocked: sensitive data of kind { $kind } is not allowed to leave
masking-pepper-degraded = Mask pepper degraded to in-process random ({ $stage }: { $detail }); placeholder determinism across processes/restarts does not hold
masking-ledger-io-failed = Masking ledger IO failed ({ $op }: { $detail }); observability side-path degraded without affecting requests

# ---- cost-replay: 离线成本回放 CLI（bin fluxgate-replay）----
replay-cli-usage = usage: fluxgate-replay --db <sqlite-path|url> --from <unix|RFC3339> --to <unix|RFC3339> [--config <path>] [--scenario affinity|cache_ttl|all] [--max-rows <n>] [--json]
replay-cli-missing-arg = missing required argument: { $arg }
replay-cli-missing-value = argument { $arg } requires a value
replay-cli-unknown-arg = unknown argument: { $arg }
replay-cli-bad-time = invalid time value: { $value } (expected unix seconds or RFC3339)
replay-cli-bad-scenario = invalid scenario: { $value } (expected affinity|cache_ttl|all)
replay-cli-bad-max-rows = invalid --max-rows value: { $value } (expected a positive integer)
replay-cli-bad-window = --to must be greater than or equal to --from
replay-cli-db-not-found = replay database file does not exist: { $path } (the tool never creates a database; check the --db path)
replay-cli-config-load-failed = failed to load config for model_catalog prices: { $error }
replay-cli-db-open-failed = failed to open replay database: { $error }
replay-cli-load-failed = failed to load request_logs for replay: { $error }

# ---- cost-replay: 回放引擎（库级错误）----
replay-window-too-large = replay window has { $rows } row(s), exceeding the --max-rows limit of { $limit }; narrow --from/--to or raise --max-rows
replay-row-decode-failed = failed to decode request_logs column { $column }: { $error }
vault-state-unrecoverable = Vault file and backup are both unusable: { $reason } — refusing to start with an empty vault (data safety first); inspect the quarantined .bad-* file or restore from an external backup
