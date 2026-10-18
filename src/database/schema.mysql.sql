-- MySQL 方言投影（db-backend-selection T004）
-- 注（absorb-sibling-libs T005）：本文件现为可读文档。运行源是
-- src/database/migration.rs 的 FROZEN V1（经 MigrationExecutor 执行）；
-- 权威源：schema.sql（SQLite）。本文件为方言投影——schema.sql 加列/加表时
-- 必须同步本文件与 schema.postgres.sql。与权威源差异仅类型映射：
--   INTEGER PRIMARY KEY AUTOINCREMENT -> BIGINT AUTO_INCREMENT PRIMARY KEY
--   REAL -> DOUBLE
--   键规格列（PK/UNIQUE/索引覆盖）TEXT -> VARCHAR(255)：MySQL 要求
--   BLOB/TEXT 索引必须带前缀长度（ERROR 1170），utf8mb4 下 VARCHAR(255)
--   = 1020 字节 < 3072 上限；
--   token_usage 的 UNIQUE 键含 4 个变长列，宽度受 3072 字节键长上限约束：
--   api_key_hash（SHA256 hex，恒 64 字符）与 date（%Y-%m-%d）收窄为
--   VARCHAR(64)/VARCHAR(16)，键长 2360 字节（对齐 migration.rs 的 V2 UP）。
-- 注意：本文件只含建表——MySQL 8 不支持 CREATE INDEX IF NOT EXISTS，索引由
-- repository::ensure_indexes_mysql 按 information_schema.statistics 幂等创建
-- （TEXT 列 idx_model 带 (255) 前缀）。

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
    api_key_hash VARCHAR(64) NOT NULL DEFAULT '',
    date VARCHAR(16) NOT NULL,
    hour INTEGER NOT NULL,
    request_count BIGINT DEFAULT 0,
    input_tokens BIGINT DEFAULT 0,
    output_tokens BIGINT DEFAULT 0,
    cache_read_tokens BIGINT DEFAULT 0,
    cache_creation_tokens BIGINT DEFAULT 0,
    input_cost DOUBLE DEFAULT 0.0,
    output_cost DOUBLE DEFAULT 0.0,
    cache_read_cost DOUBLE DEFAULT 0.0,
    cache_creation_cost DOUBLE DEFAULT 0.0,
    UNIQUE(vendor, model, api_key_hash, date, hour)
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

-- 密钥保密柜（key-vault，V3）：payload VARBINARY(8192) 存 cocoon 密文，
-- TEXT 不能作 MySQL 主键，id 走 VARCHAR(64)。
CREATE TABLE IF NOT EXISTS vault_keys (
    id VARCHAR(64) PRIMARY KEY,
    vendor_id VARCHAR(255) NOT NULL,
    label VARCHAR(255),
    payload VARBINARY(8192) NOT NULL,
    mask VARCHAR(64) NOT NULL,
    fingerprint VARCHAR(16) NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
-- 索引（idx_vault_keys_vendor_fingerprint 唯一 / idx_vault_keys_vendor）
-- 不落本文档：MySQL 投影将索引推迟到 ensure_indexes_mysql 运行时探针
-- （与 V3 迁移内联索引一致），见 repository.rs。

-- Responses 有状态表（V4）
CREATE TABLE IF NOT EXISTS response_store (
    id VARCHAR(64) PRIMARY KEY,
    tenant_hash VARCHAR(64) NOT NULL,
    conversation_id VARCHAR(128),
    parent_response_id VARCHAR(64),
    model VARCHAR(255) NOT NULL,
    status VARCHAR(24) NOT NULL,
    store_flag BIGINT NOT NULL DEFAULT 1,
    background BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    input_items LONGTEXT NOT NULL,
    output_items LONGTEXT NOT NULL,
    response_json LONGTEXT NOT NULL
) ENGINE = InnoDB;
