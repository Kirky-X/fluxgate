-- 请求日志表
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
    cost_usd REAL DEFAULT 0.0
);

-- Token 消耗统计表
CREATE TABLE IF NOT EXISTS token_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    date TEXT NOT NULL,
    hour INTEGER NOT NULL,
    request_count INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    prompt_tokens INTEGER DEFAULT 0,
    completion_tokens INTEGER DEFAULT 0,
    cost_usd_sum REAL DEFAULT 0.0,
    UNIQUE(vendor, model, date, hour)
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
