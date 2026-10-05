-- PostgreSQL 方言投影（db-backend-selection T004）
-- 权威源：schema.sql（SQLite）。本文件为方言投影——schema.sql 加列/加表时
-- 必须同步本文件与 schema.mysql.sql。与权威源差异仅类型映射：
--   INTEGER PRIMARY KEY AUTOINCREMENT -> BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY
--   INTEGER（自增键以外） -> BIGINT（timestamp/status_code/token 计数/duration_ms）
--   REAL -> DOUBLE PRECISION

-- 请求日志表
CREATE TABLE IF NOT EXISTS request_logs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id TEXT UNIQUE NOT NULL,
    timestamp BIGINT NOT NULL,
    endpoint TEXT NOT NULL,
    input_format TEXT NOT NULL,
    abstract_model TEXT,
    messages TEXT NOT NULL,
    actual_vendor TEXT NOT NULL,
    actual_model TEXT NOT NULL,
    routing_strategy TEXT NOT NULL,
    status_code BIGINT,
    response TEXT,
    error_message TEXT,
    prompt_tokens BIGINT,
    completion_tokens BIGINT,
    total_tokens BIGINT,
    duration_ms BIGINT,
    cost_usd DOUBLE PRECISION DEFAULT 0.0
);

-- Token 消耗统计表
CREATE TABLE IF NOT EXISTS token_usage (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    date TEXT NOT NULL,
    hour INTEGER NOT NULL,
    request_count BIGINT DEFAULT 0,
    total_tokens BIGINT DEFAULT 0,
    prompt_tokens BIGINT DEFAULT 0,
    completion_tokens BIGINT DEFAULT 0,
    cost_usd_sum DOUBLE PRECISION DEFAULT 0.0,
    UNIQUE(vendor, model, date, hour)
);

-- 厂商状态表
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated BIGINT NOT NULL,
    window_used_tokens BIGINT DEFAULT 0,
    window_reset_at BIGINT,
    error_count BIGINT DEFAULT 0
);

-- 模型请求统计表
CREATE TABLE IF NOT EXISTS model_stats (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    model TEXT NOT NULL,
    vendor TEXT NOT NULL,
    date TEXT NOT NULL,
    request_count BIGINT DEFAULT 0,
    success_count BIGINT DEFAULT 0,
    error_count BIGINT DEFAULT 0,
    avg_duration_ms DOUBLE PRECISION,
    UNIQUE(model, vendor, date)
);

-- 索引（pg 支持 IF NOT EXISTS，与 sqlite 同语法）
CREATE INDEX IF NOT EXISTS idx_timestamp ON request_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_vendor ON request_logs(actual_vendor);
CREATE INDEX IF NOT EXISTS idx_model ON request_logs(abstract_model);
CREATE INDEX IF NOT EXISTS idx_request_id ON request_logs(request_id);
