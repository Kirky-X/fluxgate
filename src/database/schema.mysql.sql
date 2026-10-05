-- MySQL 方言投影（db-backend-selection T004）
-- 权威源：schema.sql（SQLite）。本文件为方言投影——schema.sql 加列/加表时
-- 必须同步本文件与 schema.postgres.sql。与权威源差异仅类型映射：
--   INTEGER PRIMARY KEY AUTOINCREMENT -> BIGINT AUTO_INCREMENT PRIMARY KEY
--   REAL -> DOUBLE
--   键规格列（PK/UNIQUE/索引覆盖）TEXT -> VARCHAR(255)：MySQL 要求
--   BLOB/TEXT 索引必须带前缀长度（ERROR 1170），utf8mb4 下 VARCHAR(255)
--   = 1020 字节 < 3072 上限
-- 注意：本文件只含建表——MySQL 8 不支持 CREATE INDEX IF NOT EXISTS，索引由
-- repository::ensure_indexes_mysql 按 information_schema.statistics 幂等创建。

-- 请求日志表
CREATE TABLE IF NOT EXISTS request_logs (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    request_id VARCHAR(255) UNIQUE NOT NULL,
    timestamp BIGINT NOT NULL,
    endpoint TEXT NOT NULL,
    input_format TEXT NOT NULL,
    abstract_model TEXT,
    messages TEXT NOT NULL,
    actual_vendor VARCHAR(255) NOT NULL,
    actual_model VARCHAR(255) NOT NULL,
    routing_strategy TEXT NOT NULL,
    status_code BIGINT,
    response TEXT,
    error_message TEXT,
    prompt_tokens BIGINT,
    completion_tokens BIGINT,
    total_tokens BIGINT,
    duration_ms BIGINT,
    cost_usd DOUBLE DEFAULT 0.0
) ENGINE = InnoDB;

-- Token 消耗统计表
CREATE TABLE IF NOT EXISTS token_usage (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    vendor VARCHAR(255) NOT NULL,
    model VARCHAR(255) NOT NULL,
    date VARCHAR(255) NOT NULL,
    hour INTEGER NOT NULL,
    request_count BIGINT DEFAULT 0,
    total_tokens BIGINT DEFAULT 0,
    prompt_tokens BIGINT DEFAULT 0,
    completion_tokens BIGINT DEFAULT 0,
    cost_usd_sum DOUBLE DEFAULT 0.0,
    UNIQUE(vendor, model, date, hour)
) ENGINE = InnoDB;

-- 厂商状态表
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor VARCHAR(255) PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated BIGINT NOT NULL,
    window_used_tokens BIGINT DEFAULT 0,
    window_reset_at BIGINT,
    error_count BIGINT DEFAULT 0
) ENGINE = InnoDB;

-- 模型请求统计表
CREATE TABLE IF NOT EXISTS model_stats (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    model VARCHAR(255) NOT NULL,
    vendor VARCHAR(255) NOT NULL,
    date VARCHAR(255) NOT NULL,
    request_count BIGINT DEFAULT 0,
    success_count BIGINT DEFAULT 0,
    error_count BIGINT DEFAULT 0,
    avg_duration_ms DOUBLE,
    UNIQUE(model, vendor, date)
) ENGINE = InnoDB;
