-- 请求日志表
-- 注（absorb-sibling-libs T005）：本文件现为可读文档。运行源是
-- src/database/migration.rs 的 FROZEN V1（经 MigrationExecutor 执行）；
-- 新表/新列走新迁移版本，禁止改动已冻结的 V1 语义。
CREATE TABLE IF NOT EXISTS request_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id TEXT UNIQUE NOT NULL,
    timestamp INTEGER NOT NULL,
    endpoint TEXT NOT NULL,
    input_format TEXT NOT NULL,
    abstract_model TEXT,
    messages TEXT NOT NULL,
    actual_vendor TEXT NOT NULL,
    actual_model TEXT NOT NULL,
    routing_strategy TEXT NOT NULL,
    status_code INTEGER,
    response TEXT,
    error_message TEXT,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER,
    duration_ms INTEGER,
    cost_usd REAL DEFAULT 0.0,
    session_key TEXT,
    context_window_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER
);

-- Token 消耗统计表
CREATE TABLE IF NOT EXISTS token_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    api_key_hash TEXT NOT NULL DEFAULT '',
    date TEXT NOT NULL,
    hour INTEGER NOT NULL,
    request_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_creation_tokens INTEGER DEFAULT 0,
    input_cost REAL DEFAULT 0.0,
    output_cost REAL DEFAULT 0.0,
    cache_read_cost REAL DEFAULT 0.0,
    cache_creation_cost REAL DEFAULT 0.0,
    UNIQUE(vendor, model, api_key_hash, date, hour)
);

-- 厂商状态表
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated INTEGER NOT NULL,
    window_used_tokens INTEGER DEFAULT 0,
    window_reset_at INTEGER,
    error_count INTEGER DEFAULT 0
);

-- 模型请求统计表
CREATE TABLE IF NOT EXISTS model_stats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model TEXT NOT NULL,
    vendor TEXT NOT NULL,
    date TEXT NOT NULL,
    request_count INTEGER DEFAULT 0,
    success_count INTEGER DEFAULT 0,
    error_count INTEGER DEFAULT 0,
    avg_duration_ms REAL,
    UNIQUE(model, vendor, date)
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_timestamp ON request_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_vendor ON request_logs(actual_vendor);
CREATE INDEX IF NOT EXISTS idx_model ON request_logs(abstract_model);
CREATE INDEX IF NOT EXISTS idx_request_id ON request_logs(request_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_session ON request_logs(session_key, timestamp);

-- 密钥保密柜（key-vault，V3）：payload 为 cocoon self-contained 密文，
-- mask/fingerprint 为明文衍生的展示/去重面（零明文）。
CREATE TABLE IF NOT EXISTS vault_keys (
    id TEXT PRIMARY KEY,
    vendor_id TEXT NOT NULL,
    label TEXT,
    payload BLOB NOT NULL,
    mask TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_vault_keys_vendor_fingerprint ON vault_keys (vendor_id, fingerprint);
CREATE INDEX IF NOT EXISTS idx_vault_keys_vendor ON vault_keys (vendor_id);

-- Responses 有状态表（V4）：input/output items + 父子链 + 会话分组 + 过期时间
CREATE TABLE IF NOT EXISTS response_store (
    id TEXT PRIMARY KEY,
    tenant_hash TEXT NOT NULL,
    conversation_id TEXT,
    parent_response_id TEXT,
    model TEXT NOT NULL,
    status TEXT NOT NULL,
    store_flag INTEGER NOT NULL DEFAULT 1,
    background INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    input_items TEXT NOT NULL DEFAULT '[]',
    output_items TEXT NOT NULL DEFAULT '[]',
    response_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_response_store_tenant ON response_store(tenant_hash, created_at);
CREATE INDEX IF NOT EXISTS idx_response_store_conversation ON response_store(conversation_id, created_at);
CREATE INDEX IF NOT EXISTS idx_response_store_expires ON response_store(expires_at);

-- 会话粘性落盘表（V6，cache-economy-affinity）：[routing.affinity] 为
-- auto/turn 时，resp usage 结算后把「会话 → vendor」的粘性 write-behind
-- 落库，重启按 expires_at 恢复未过期的前 512 条。
-- 复合主键 (tenant, session_key) 与亲和键的租户隔离同构：同 session id
-- 在不同租户下是两条独立粘性。
CREATE TABLE IF NOT EXISTS affinity_sticks (
    tenant TEXT NOT NULL DEFAULT '',
    session_key TEXT NOT NULL,
    vendor_id TEXT NOT NULL,
    worth_tokens INTEGER NOT NULL DEFAULT 0,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (tenant, session_key)
);
CREATE INDEX IF NOT EXISTS idx_affinity_sticks_expires ON affinity_sticks(expires_at);

-- 多窗预算账本（V7，quota-aware-routing）：[[quota.windows]] 多窗判定与
-- [[vendor_budgets]] 记账的持久化面。结算 UPSERT + 30s 周期 flush，重启
-- 仅恢复未过期窗（window_start + 周期 ≥ now）。api_key_hash 承载三类键：
-- 租户摘要 / __global__（scope=global 窗）/ __vendor__:{id}（vendor 预算）。
CREATE TABLE IF NOT EXISTS quota_ledger (
    api_key_hash TEXT NOT NULL,
    window_id TEXT NOT NULL,
    window_start INTEGER NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    cost_used_millicents INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (api_key_hash, window_id, window_start)
);
