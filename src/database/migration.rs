// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 核心表版本化迁移（V1..=V3）——sqlite/postgres/mysql 启动建表唯一入口。
//!
//! V1（FROZEN）建四张初版表；V2 把 token_usage 原地升级为分账形态（新增
//! api_key_hash 与四类 token/四项分项成本列，UNIQUE 键扩为 (vendor, model,
//! api_key_hash, date, hour)，旧行映射 prompt→input、cost_usd_sum 整额入 input 档）。
//! V3 建密钥保密柜表 vault_keys（key-vault：cocoon 加密 blob + 掩码/指纹列）。
//! 已发布的迁移常量禁止改动（已部署库的历史行指向它们），新变更走新版本。
//! 存量库基线 = 幂等重放（V1 UP 全 `IF NOT EXISTS`）；迁移失败直接上浮（无静默降级）。

use dbnexus::{DatabaseType, DbError, DbPool, MigrationExecutor, MigrationFile};
use sea_orm::DatabaseBackend;
use std::path::PathBuf;

/// 核心表迁移最新版本（V3：vault_keys 密钥保密柜，见模块文档）。
pub(crate) const CORE_SCHEMA_VERSION: u32 = 3;

/// V1 UP（sqlite，FROZEN；含 cost 列与 4 索引）。
const UP_SQLITE: &str = r#"
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
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated INTEGER NOT NULL,
    window_used_tokens INTEGER DEFAULT 0,
    window_reset_at INTEGER,
    error_count INTEGER DEFAULT 0
);
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
CREATE INDEX IF NOT EXISTS idx_timestamp ON request_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_vendor ON request_logs(actual_vendor);
CREATE INDEX IF NOT EXISTS idx_model ON request_logs(abstract_model);
CREATE INDEX IF NOT EXISTS idx_request_id ON request_logs(request_id);
"#;

/// V1 UP（postgres，FROZEN）。
const UP_POSTGRES: &str = r#"
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
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated BIGINT NOT NULL,
    window_used_tokens BIGINT DEFAULT 0,
    window_reset_at BIGINT,
    error_count BIGINT DEFAULT 0
);
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
CREATE INDEX IF NOT EXISTS idx_timestamp ON request_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_vendor ON request_logs(actual_vendor);
CREATE INDEX IF NOT EXISTS idx_model ON request_logs(abstract_model);
CREATE INDEX IF NOT EXISTS idx_request_id ON request_logs(request_id);
"#;

/// V1 UP（mysql，FROZEN；只含建表，索引走 `ensure_indexes_mysql`）。
const UP_MYSQL: &str = r#"
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
CREATE TABLE IF NOT EXISTS vendor_status (
    vendor VARCHAR(255) PRIMARY KEY,
    status TEXT NOT NULL,
    last_updated BIGINT NOT NULL,
    window_used_tokens BIGINT DEFAULT 0,
    window_reset_at BIGINT,
    error_count BIGINT DEFAULT 0
) ENGINE = InnoDB;
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
"#;

/// V2 UP（sqlite）：UNIQUE 是表 DDL 的一部分，键换绑 + 删列只能重建表
///（rename → create → 搬数 → drop）；旧行 api_key_hash=''、cost_usd_sum 整额入 input_cost。
const UP_V2_SQLITE: &str = r#"
ALTER TABLE token_usage RENAME TO token_usage_v1;
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
INSERT INTO token_usage (vendor, model, api_key_hash, date, hour, request_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, input_cost, output_cost, cache_read_cost, cache_creation_cost)
SELECT vendor, model, '', date, hour, request_count, prompt_tokens, completion_tokens, 0, 0, cost_usd_sum, 0.0, 0.0, 0.0 FROM token_usage_v1;
DROP TABLE token_usage_v1;
CREATE INDEX IF NOT EXISTS idx_token_usage_date ON token_usage(date);
CREATE INDEX IF NOT EXISTS idx_token_usage_key_date ON token_usage(api_key_hash, date);
"#;

/// V2 UP（postgres）：UNIQUE 约束按自动命名惯例 `{table}_{cols}_key` 换绑；
/// 新列带 DEFAULT 使旧行即时取得缺省值，再 UPDATE 回填映射（api_key_hash='' 为规格明文要求）。
const UP_V2_POSTGRES: &str = r#"
ALTER TABLE token_usage ADD COLUMN api_key_hash TEXT NOT NULL DEFAULT '', ADD COLUMN input_tokens BIGINT DEFAULT 0, ADD COLUMN output_tokens BIGINT DEFAULT 0, ADD COLUMN cache_read_tokens BIGINT DEFAULT 0, ADD COLUMN cache_creation_tokens BIGINT DEFAULT 0, ADD COLUMN input_cost DOUBLE PRECISION DEFAULT 0.0, ADD COLUMN output_cost DOUBLE PRECISION DEFAULT 0.0, ADD COLUMN cache_read_cost DOUBLE PRECISION DEFAULT 0.0, ADD COLUMN cache_creation_cost DOUBLE PRECISION DEFAULT 0.0;
UPDATE token_usage SET api_key_hash = '', input_tokens = prompt_tokens, output_tokens = completion_tokens, input_cost = cost_usd_sum;
ALTER TABLE token_usage DROP COLUMN total_tokens, DROP COLUMN prompt_tokens, DROP COLUMN completion_tokens, DROP COLUMN cost_usd_sum;
ALTER TABLE token_usage DROP CONSTRAINT token_usage_vendor_model_date_hour_key;
ALTER TABLE token_usage ADD CONSTRAINT token_usage_vendor_model_api_key_hash_date_hour_key UNIQUE (vendor, model, api_key_hash, date, hour);
CREATE INDEX IF NOT EXISTS idx_token_usage_date ON token_usage(date);
CREATE INDEX IF NOT EXISTS idx_token_usage_key_date ON token_usage(api_key_hash, date);
"#;

/// V2 UP（mysql）：内联 UNIQUE 自动以首列命名（V1 键名 = vendor），换绑 =
/// DROP INDEX + ADD UNIQUE KEY；普通索引走 `ensure_indexes_mysql`。
const UP_V2_MYSQL: &str = r#"
ALTER TABLE token_usage ADD COLUMN api_key_hash VARCHAR(255) NOT NULL DEFAULT '', ADD COLUMN input_tokens BIGINT DEFAULT 0, ADD COLUMN output_tokens BIGINT DEFAULT 0, ADD COLUMN cache_read_tokens BIGINT DEFAULT 0, ADD COLUMN cache_creation_tokens BIGINT DEFAULT 0, ADD COLUMN input_cost DOUBLE DEFAULT 0.0, ADD COLUMN output_cost DOUBLE DEFAULT 0.0, ADD COLUMN cache_read_cost DOUBLE DEFAULT 0.0, ADD COLUMN cache_creation_cost DOUBLE DEFAULT 0.0;
UPDATE token_usage SET api_key_hash = '', input_tokens = prompt_tokens, output_tokens = completion_tokens, input_cost = cost_usd_sum;
ALTER TABLE token_usage DROP COLUMN total_tokens, DROP COLUMN prompt_tokens, DROP COLUMN completion_tokens, DROP COLUMN cost_usd_sum, DROP INDEX vendor, ADD UNIQUE KEY uk_token_usage_v2 (vendor, model, api_key_hash, date, hour);
"#;

/// V2 DOWN（三后端）：V2 的逆映射（input→prompt、output→completion、
/// 四项成本合计回 cost_usd_sum、token 合计回 total_tokens）。sqlite 重建，
/// pg/mysql 换绑约束后增删列。
const DOWN_V2_SQLITE: &str = r#"
ALTER TABLE token_usage RENAME TO token_usage_v2;
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
INSERT INTO token_usage (vendor, model, date, hour, request_count, total_tokens, prompt_tokens, completion_tokens, cost_usd_sum)
SELECT vendor, model, date, hour, request_count, input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens, input_tokens, output_tokens, input_cost + output_cost + cache_read_cost + cache_creation_cost FROM token_usage_v2;
DROP TABLE token_usage_v2;
"#;

const DOWN_V2_POSTGRES: &str = r#"
ALTER TABLE token_usage DROP CONSTRAINT token_usage_vendor_model_api_key_hash_date_hour_key;
ALTER TABLE token_usage ADD CONSTRAINT token_usage_vendor_model_date_hour_key UNIQUE (vendor, model, date, hour);
ALTER TABLE token_usage ADD COLUMN total_tokens BIGINT DEFAULT 0, ADD COLUMN prompt_tokens BIGINT DEFAULT 0, ADD COLUMN completion_tokens BIGINT DEFAULT 0, ADD COLUMN cost_usd_sum DOUBLE PRECISION DEFAULT 0.0;
UPDATE token_usage SET prompt_tokens = input_tokens, completion_tokens = output_tokens, total_tokens = input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens, cost_usd_sum = input_cost + output_cost + cache_read_cost + cache_creation_cost;
ALTER TABLE token_usage DROP COLUMN api_key_hash, DROP COLUMN input_tokens, DROP COLUMN output_tokens, DROP COLUMN cache_read_tokens, DROP COLUMN cache_creation_tokens, DROP COLUMN input_cost, DROP COLUMN output_cost, DROP COLUMN cache_read_cost, DROP COLUMN cache_creation_cost;
DROP INDEX IF EXISTS idx_token_usage_date;
DROP INDEX IF EXISTS idx_token_usage_key_date;
"#;

const DOWN_V2_MYSQL: &str = r#"
ALTER TABLE token_usage DROP INDEX uk_token_usage_v2, ADD UNIQUE KEY vendor (vendor, model, date, hour);
ALTER TABLE token_usage ADD COLUMN total_tokens BIGINT DEFAULT 0, ADD COLUMN prompt_tokens BIGINT DEFAULT 0, ADD COLUMN completion_tokens BIGINT DEFAULT 0, ADD COLUMN cost_usd_sum DOUBLE DEFAULT 0.0;
UPDATE token_usage SET prompt_tokens = input_tokens, completion_tokens = output_tokens, total_tokens = input_tokens + output_tokens + cache_read_tokens + cache_creation_tokens, cost_usd_sum = input_cost + output_cost + cache_read_cost + cache_creation_cost;
ALTER TABLE token_usage DROP COLUMN api_key_hash, DROP COLUMN input_tokens, DROP COLUMN output_tokens, DROP COLUMN cache_read_tokens, DROP COLUMN cache_creation_tokens, DROP COLUMN input_cost, DROP COLUMN output_cost, DROP COLUMN cache_read_cost, DROP COLUMN cache_creation_cost;
"#;

/// V3 UP（sqlite）：密钥保密柜（key-vault）；payload 为 cocoon self-contained 密文。
const UP_V3_SQLITE: &str = r#"
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
"#;

/// V3 UP（postgres）：id 用 TEXT 主键（uuid 形态 hex），二进制密文走 BYTEA。
const UP_V3_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS vault_keys (
    id TEXT PRIMARY KEY,
    vendor_id TEXT NOT NULL,
    label TEXT,
    payload BYTEA NOT NULL,
    mask TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_vault_keys_vendor_fingerprint ON vault_keys (vendor_id, fingerprint);
CREATE INDEX IF NOT EXISTS idx_vault_keys_vendor ON vault_keys (vendor_id);
"#;

/// V3 UP（mysql）：TEXT 不能作主键，id/vendor_id 走 VARCHAR；密文 VARBINARY(8192)
/// 覆盖单条 blob 量级（数百字节，上限远大于实际）。
const UP_V3_MYSQL: &str = r#"
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
CREATE UNIQUE INDEX idx_vault_keys_vendor_fingerprint ON vault_keys (vendor_id, fingerprint);
CREATE INDEX idx_vault_keys_vendor ON vault_keys (vendor_id);
"#;

/// V3 DOWN（三后端通用：DROP TABLE 隐式带走索引；`IF EXISTS` 使回滚可重入）。
const DOWN_V3: &str = r#"
DROP TABLE IF EXISTS vault_keys;
"#;

/// V1 DOWN（三后端通用：DROP TABLE 隐式带走索引；`IF EXISTS` 使回滚可重入）。
const DOWN_COMMON: &str = r#"
DROP TABLE IF EXISTS model_stats;
DROP TABLE IF EXISTS vendor_status;
DROP TABLE IF EXISTS token_usage;
DROP TABLE IF EXISTS request_logs;
"#;

/// 按连接后端与版本构造迁移文件（内存嵌入；`-- UP`/`-- DOWN` 标记行供
/// `MigrationExecutor` 切分）。未知后端/未知版本返回 Err（不静默按 sqlite）。
pub(crate) fn core_migration_file(
    backend: DatabaseBackend,
    version: u32,
) -> Result<MigrationFile, DbError> {
    let label = match backend {
        DatabaseBackend::Postgres => "postgres",
        DatabaseBackend::MySql => "mysql",
        DatabaseBackend::Sqlite => "sqlite",
        _ => {
            return Err(DbError::Config(
                "core migrations cover sqlite/postgres/mysql; \
                 other backends use the legacy DDL path"
                    .to_string(),
            ));
        }
    };
    let up: &str = match (version, backend) {
        (1, DatabaseBackend::Sqlite) => UP_SQLITE,
        (1, DatabaseBackend::Postgres) => UP_POSTGRES,
        (1, DatabaseBackend::MySql) => UP_MYSQL,
        (2, DatabaseBackend::Sqlite) => UP_V2_SQLITE,
        (2, DatabaseBackend::Postgres) => UP_V2_POSTGRES,
        (2, DatabaseBackend::MySql) => UP_V2_MYSQL,
        (3, DatabaseBackend::Sqlite) => UP_V3_SQLITE,
        (3, DatabaseBackend::Postgres) => UP_V3_POSTGRES,
        (3, DatabaseBackend::MySql) => UP_V3_MYSQL,
        _ => {
            return Err(DbError::Config(format!(
                "no core migration v{version} for {label}"
            )));
        }
    };
    let down: &str = match version {
        1 => DOWN_COMMON,
        3 => DOWN_V3,
        2 => match backend {
            DatabaseBackend::Sqlite => DOWN_V2_SQLITE,
            DatabaseBackend::Postgres => DOWN_V2_POSTGRES,
            DatabaseBackend::MySql => DOWN_V2_MYSQL,
            _ => DOWN_COMMON,
        },
        _ => {
            return Err(DbError::Config(format!(
                "no core migration DOWN for v{version}"
            )));
        }
    };
    let content = format!(
        "-- UP nexusflow core v{version} ({label})\n{up}\n-- DOWN nexusflow core v{version} ({label})\n{down}\n"
    );
    Ok(MigrationFile::new(
        version,
        format!("nexusflow core tables v{version} ({label})"),
        PathBuf::from(format!("embedded:core_v{version}_{label}.sql")),
        content,
    ))
}

/// 应用核心迁移 V1..=V2（幂等：已在历史中的版本跳过不重放），返回本次应用的迁移数。
/// Err 上浮为启动 fail-fast；未知后端回落旧 DDL（见 `init_schema`）。
pub(crate) async fn apply_core_migrations(pool: &DbPool) -> Result<u32, DbError> {
    // 先释放 session 再跑 executor：测试池 max_connections=1，同时持有两个 session 会死等。
    let (backend, conn) = {
        let session = pool.get_session("admin").await?;
        let backend = session.connection()?.get_database_backend();
        let conn = session.connection()?.clone();
        (backend, conn)
    };
    let db_type = match backend {
        DatabaseBackend::Postgres => DatabaseType::Postgres,
        DatabaseBackend::MySql => DatabaseType::MySql,
        DatabaseBackend::Sqlite => DatabaseType::Sqlite,
        _ => {
            return Err(DbError::Config(
                "core migrations cover sqlite/postgres/mysql; \
                 other backends use the legacy DDL path"
                    .to_string(),
            ));
        }
    };
    let mut executor = MigrationExecutor::new(conn, db_type);
    executor.load_history().await?;
    let mut applied = 0;
    for version in 1..=CORE_SCHEMA_VERSION {
        if executor.get_all_versions().contains(&version) {
            continue;
        }
        executor
            .apply_migration_file_public(&core_migration_file(backend, version)?)
            .await?;
        applied += 1;
    }
    Ok(applied)
}

/// 回滚全部核心迁移（测试面：逆序执行 DOWN 并删除历史行），唯一调用点是
/// [`live_tests`] 的 DOWN 断言。
#[cfg(all(test, feature = "db-sqlite"))]
pub(crate) async fn rollback_core_all(pool: &DbPool) -> Result<(), DbError> {
    // 同 apply_core_migrations：先释放 session（size=1 池防死等）。
    let (backend, conn) = {
        let session = pool.get_session("admin").await?;
        let backend = session.connection()?.get_database_backend();
        let conn = session.connection()?.clone();
        (backend, conn)
    };
    let db_type = match backend {
        DatabaseBackend::Postgres => DatabaseType::Postgres,
        DatabaseBackend::MySql => DatabaseType::MySql,
        DatabaseBackend::Sqlite => DatabaseType::Sqlite,
        _ => {
            return Err(DbError::Config(
                "core migration rollback covers sqlite/postgres/mysql".to_string(),
            ));
        }
    };
    let mut executor = MigrationExecutor::new(conn, db_type);
    executor.load_history().await?;
    for version in (1..=CORE_SCHEMA_VERSION).rev() {
        if !executor.get_all_versions().contains(&version) {
            continue;
        }
        executor
            .rollback_version(version, &core_migration_file(backend, version)?)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod content_tests {
    use super::*;
    use sea_orm::DatabaseBackend;

    const TABLES: [&str; 4] = [
        "request_logs",
        "token_usage",
        "vendor_status",
        "model_stats",
    ];
    const INDEXES: [&str; 4] = ["idx_timestamp", "idx_vendor", "idx_model", "idx_request_id"];

    /// V1 UP 含四表；sqlite/postgres 另含四索引；mysql 索引走运行时探针。
    #[test]
    fn v1_up_defines_all_tables_and_expected_indexes() {
        for (backend, with_indexes) in [
            (DatabaseBackend::Sqlite, true),
            (DatabaseBackend::Postgres, true),
            (DatabaseBackend::MySql, false),
        ] {
            let file = core_migration_file(backend, 1).expect("三后端必有 V1");
            assert_eq!(file.version(), 1);
            // DOWN 段无 CREATE 语句，全文断言等价于 UP 断言
            let up = file.content();
            for table in TABLES {
                assert!(
                    up.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
                    "{backend:?} UP 缺表 {table}"
                );
            }
            for index in INDEXES {
                let present = up.contains(&format!("CREATE INDEX IF NOT EXISTS {index}"));
                assert_eq!(
                    present, with_indexes,
                    "{backend:?} UP 索引 {index} 形态不符"
                );
            }
        }
    }

    /// FROZEN 钉住：V1 token_usage 必须保持组合聚合形态——分账形态只允许住在 V2。
    #[test]
    fn v1_token_usage_is_frozen_combined_shape() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let file = core_migration_file(backend, 1).expect("三后端必有 V1");
            let up = file.content();
            assert!(
                up.contains("cost_usd_sum"),
                "{backend:?} V1 组合成本列必须保留"
            );
            assert!(
                !up.contains("api_key_hash"),
                "{backend:?} V1 冻结形态不得出现 api_key_hash"
            );
            assert!(
                !up.contains("idx_token_usage"),
                "{backend:?} V1 不得携带 V2 的查询索引"
            );
        }
    }

    /// V2 UP（三后端同语义）：分账列、UNIQUE 键换绑；成本查询索引仅
    /// sqlite/postgres 内联（mysql 走 ensure_indexes_mysql 运行时探针）。
    #[test]
    fn v2_up_upgrades_token_usage_to_ledger_shape() {
        for (backend, with_indexes) in [
            (DatabaseBackend::Sqlite, true),
            (DatabaseBackend::Postgres, true),
            (DatabaseBackend::MySql, false),
        ] {
            let file = core_migration_file(backend, 2).expect("三后端必有 V2");
            assert_eq!(file.version(), 2);
            let up = file.content();
            for col in [
                "api_key_hash",
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_creation_tokens",
                "input_cost",
                "output_cost",
                "cache_read_cost",
                "cache_creation_cost",
            ] {
                assert!(up.contains(col), "{backend:?} V2 UP 缺分账列 {col}");
            }
            assert!(
                up.contains("api_key_hash, date, hour)") && up.contains("UNIQUE"),
                "{backend:?} V2 UNIQUE 键必须含 api_key_hash"
            );
            // 旧行映射（旧数据不丢）：sqlite 走重建搬数，postgres/mysql 走 ALTER+UPDATE 回填。
            let mapped = if backend == DatabaseBackend::Sqlite {
                up.contains("SELECT vendor, model, '', date, hour, request_count, prompt_tokens, completion_tokens")
            } else {
                up.contains("input_tokens = prompt_tokens")
            };
            assert!(mapped, "{backend:?} V2 旧行映射缺失（旧数据不丢）");
            for index in ["idx_token_usage_date", "idx_token_usage_key_date"] {
                let present = up.contains(&format!("CREATE INDEX IF NOT EXISTS {index}"));
                assert_eq!(
                    present, with_indexes,
                    "{backend:?} V2 查询索引 {index} 形态不符"
                );
            }
        }
    }

    /// V1/V2 DOWN 可提取且含表 DROP（回滚路径不缺失）；V2 DOWN 携带逆映射。
    #[test]
    fn down_segments_are_extractable() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            for version in [1u32, 2] {
                let file = core_migration_file(backend, version).expect("三后端必有迁移");
                let down =
                    MigrationExecutor::extract_down_sql(file.content()).expect("必须有 DOWN 段");
                if version == 1 {
                    for table in TABLES {
                        assert!(
                            down.contains(&format!("DROP TABLE IF EXISTS {table}")),
                            "{backend:?} V1 DOWN 缺 {table}"
                        );
                    }
                } else {
                    assert!(
                        down.contains("cost_usd_sum"),
                        "{backend:?} V2 DOWN 必须逆映射回组合列"
                    );
                    let reverted = if backend == DatabaseBackend::Sqlite {
                        down.contains("INSERT INTO token_usage")
                    } else {
                        down.contains("DROP COLUMN api_key_hash")
                    };
                    assert!(reverted, "{backend:?} V2 DOWN 必须移除分账形态");
                }
            }
        }
    }

    /// FROZEN V1 与文档 schema.sql 表集合一致（双源并存，漂移即红——禁止静默改 V1）。
    #[test]
    fn frozen_v1_tables_match_documented_schema() {
        fn table_set(sql: &str) -> std::collections::HashSet<String> {
            sql.lines()
                .filter_map(|line| {
                    let t = line.trim();
                    t.strip_prefix("CREATE TABLE IF NOT EXISTS ")
                        .and_then(|rest| rest.split_whitespace().next())
                        .map(|name| {
                            name.trim_matches(|c| c == '"' || c == '`' || c == '(')
                                .to_string()
                        })
                })
                .collect()
        }

        let doc_sql = include_str!("schema.sql");
        // schema.sql 与迁移全集（V1..=CORE_SCHEMA_VERSION）的表集合一致；
        // 新版本追加表后此处随全集自动对齐，无需逐版维护
        let mut expected = std::collections::HashSet::new();
        for version in 1..=CORE_SCHEMA_VERSION {
            let file =
                core_migration_file(DatabaseBackend::Sqlite, version).expect("sqlite 全版本可取");
            // UP 段（DOWN 标记之前）参与比对
            let up = file.content().split("-- DOWN").next().unwrap_or_default();
            expected.extend(table_set(up));
        }
        assert_eq!(
            expected,
            table_set(doc_sql),
            "迁移全集与 schema.sql 表集合必须一致"
        );
    }
}

/// sqlite 生效面 live 测试（批量/全新库/存量基线/V1 升级 V2/DOWN/只读 fail-fast）。
#[cfg(all(test, feature = "db-sqlite"))]
mod live_tests {
    use super::*;
    use crate::database::{repository::init_schema, LogFilter, Repository, RequestLog};
    use dbnexus::{DbConfig, DbPool};
    use sea_orm::{ConnectionTrait, Statement};
    use std::collections::HashSet;

    async fn memory_pool() -> DbPool {
        DbPool::with_config(DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("memory pool")
    }

    async fn table_names(pool: &DbPool) -> HashSet<String> {
        let session = pool.get_session("admin").await.expect("session");
        let stmt = Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table'".to_string(),
        );
        session
            .connection()
            .expect("conn")
            .query_all_raw(stmt)
            .await
            .expect("sqlite_master")
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect()
    }

    async fn applied_versions(pool: &DbPool) -> Vec<u32> {
        // 同生产路径：先释放 session 再读历史（size=1 池防死等）。
        let conn = {
            let session = pool.get_session("admin").await.expect("session");
            session.connection().expect("conn").clone()
        };
        let mut executor = MigrationExecutor::new(conn, DatabaseType::Sqlite);
        executor.load_history().await.expect("history");
        executor.get_all_versions()
    }

    /// 存量库基线表形态（表在、历史空；request_logs 缺 cost 列以覆盖
    /// v0.1 列回填路径）。
    const LEGACY_TABLES_NO_COST: [&str; 4] = [
        "CREATE TABLE request_logs (id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT UNIQUE NOT NULL, timestamp INTEGER NOT NULL, endpoint TEXT NOT NULL, input_format TEXT NOT NULL, abstract_model TEXT, messages TEXT NOT NULL, actual_vendor TEXT NOT NULL, actual_model TEXT NOT NULL, routing_strategy TEXT NOT NULL, status_code INTEGER, response TEXT, error_message TEXT, prompt_tokens INTEGER, completion_tokens INTEGER, total_tokens INTEGER, duration_ms INTEGER)",
        "CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, vendor TEXT NOT NULL, model TEXT NOT NULL, date TEXT NOT NULL, hour INTEGER NOT NULL, request_count INTEGER DEFAULT 0, total_tokens INTEGER DEFAULT 0, prompt_tokens INTEGER DEFAULT 0, completion_tokens INTEGER DEFAULT 0, cost_usd_sum REAL DEFAULT 0.0, UNIQUE(vendor, model, date, hour))",
        "CREATE TABLE vendor_status (vendor TEXT PRIMARY KEY, status TEXT NOT NULL, last_updated INTEGER NOT NULL, window_used_tokens INTEGER DEFAULT 0, window_reset_at INTEGER, error_count INTEGER DEFAULT 0)",
        "CREATE TABLE model_stats (id INTEGER PRIMARY KEY AUTOINCREMENT, model TEXT NOT NULL, vendor TEXT NOT NULL, date TEXT NOT NULL, request_count INTEGER DEFAULT 0, success_count INTEGER DEFAULT 0, error_count INTEGER DEFAULT 0, avg_duration_ms REAL, UNIQUE(model, vendor, date))",
    ];

    /// 绕过迁移直建表（模拟旧版本建的存量库：表在、历史空）。
    async fn create_tables_raw(pool: &DbPool) {
        let session = pool.get_session("admin").await.expect("session");
        for ddl in LEGACY_TABLES_NO_COST {
            session.execute_raw_ddl(ddl).await.expect("legacy ddl");
        }
    }

    fn sample_log(i: i64, messages: &str) -> RequestLog {
        RequestLog {
            id: None,
            request_id: format!("req-{i:03}"),
            timestamp: 1_700_000_000 + i,
            endpoint: "/v1/chat/completions".to_string(),
            input_format: "openai".to_string(),
            abstract_model: Some("grok".to_string()),
            messages: messages.to_string(),
            actual_vendor: "mock-openai".to_string(),
            actual_model: "grok-4".to_string(),
            routing_strategy: "weighted".to_string(),
            status_code: Some(200),
            response: Some("{\"ok\":true}".to_string()),
            error_message: None,
            prompt_tokens: Some(5),
            completion_tokens: Some(2),
            total_tokens: Some(7),
            duration_ms: Some(12),
            cost_usd: Some(0.0001),
        }
    }

    fn all_core_versions() -> Vec<u32> {
        (1..=CORE_SCHEMA_VERSION).collect()
    }

    /// 全新库：迁移建四表+索引、记 v1+v2、幂等重跑 0；20 行批量（含注入形态
    /// 载荷）逐行可读——参数化绑定无手写转义。
    #[tokio::test]
    async fn fresh_db_migrates_and_batch_roundtrips() {
        let pool = memory_pool().await;
        init_schema(&pool).await.expect("init");
        init_schema(&pool).await.expect("re-init idempotent");

        let tables = table_names(&pool).await;
        for t in [
            "request_logs",
            "token_usage",
            "vendor_status",
            "model_stats",
        ] {
            assert!(tables.contains(t), "缺表 {t}");
        }
        assert!(tables.contains("dbnexus_migrations"), "缺迁移历史表");
        assert_eq!(applied_versions(&pool).await, all_core_versions());

        let repo = Repository::new(pool.clone());
        let hostile = [
            "'; DROP TABLE request_logs; --",
            "\"\\u0000\\'",
            "O'Brien",
            "正常中文🎉",
            "%_\\",
        ];
        let logs: Vec<RequestLog> = (0..20)
            .map(|i: i64| sample_log(i, hostile[i as usize % hostile.len()]))
            .collect();
        repo.log_requests_batch(&logs).await.expect("batch");

        let back = repo
            .query_logs(&LogFilter {
                limit: 100,
                ..Default::default()
            })
            .await
            .expect("query");
        assert_eq!(back.len(), 20);
        let by_id: std::collections::HashMap<_, _> =
            back.iter().map(|r| (r.request_id.clone(), r)).collect();
        for (i, log) in logs.iter().enumerate() {
            let row = by_id.get(&log.request_id).expect("逐行可读");
            assert_eq!(row.messages, hostile[i % hostile.len()], "字段完整");
            assert_eq!(row.timestamp, log.timestamp);
            assert_eq!(row.status_code, Some(200));
        }
        // request_logs 表仍在（注入载荷未生效）
        assert!(table_names(&pool).await.contains("request_logs"));
    }

    /// 存量库（手工建表、历史为空，缺 cost 列模拟 V0.1 旧库）：启动不报错、
    /// 不重复建表、缺列回填、补记 v1+v2 历史、可正常写入。
    #[tokio::test]
    async fn existing_db_without_history_is_baselined() {
        let pool = memory_pool().await;
        create_tables_raw(&pool).await;

        init_schema(&pool).await.expect("存量基线不报错");
        assert_eq!(applied_versions(&pool).await, all_core_versions());

        // 缺列已回填
        let session = pool.get_session("admin").await.expect("session");
        let stmt = Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA table_info(request_logs)".to_string(),
        );
        let cols: HashSet<String> = session
            .connection()
            .expect("conn")
            .query_all_raw(stmt)
            .await
            .expect("pragma")
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect();
        assert!(cols.contains("cost_usd"), "v0.1 列回填");
        drop(session);

        let repo = Repository::new(pool.clone());
        repo.log_requests_batch(&[sample_log(0, "hello")])
            .await
            .expect("基线后可写");
        let back = repo
            .query_logs(&LogFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("query");
        assert_eq!(back.len(), 1);
    }

    /// V1 旧库启动自动原地升级到 V2：旧数据不丢（映射可读），新写入走分账新形态。
    #[tokio::test]
    async fn v1_db_upgrades_to_v2_in_place() {
        let pool = memory_pool().await;
        create_tables_raw(&pool).await;
        // 存量 V1 行：组合聚合（prompt=10、completion=2、组合成本 0.5）。
        {
            let session = pool.get_session("admin").await.expect("session");
            session
                .execute_with_params(
                    "INSERT INTO token_usage (vendor, model, date, hour, request_count, total_tokens, prompt_tokens, completion_tokens, cost_usd_sum) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    &[
                        "mock-openai".into(),
                        "grok-4".into(),
                        "2026-10-01".into(),
                        5.into(),
                        3.into(),
                        12.into(),
                        10.into(),
                        2.into(),
                        0.5.into(),
                    ],
                )
                .await
                .expect("legacy row");
        }

        init_schema(&pool).await.expect("V1 库升级不报错");
        assert_eq!(
            applied_versions(&pool).await,
            all_core_versions(),
            "仅追加 V2"
        );

        // 旧行保留且映射可读：api_key_hash=''、prompt→input、completion→output、cost_usd_sum 整额入 input_cost。
        let repo = Repository::new(pool.clone());
        let stats = repo.get_token_stats(None).await.expect("stats");
        assert_eq!(stats.len(), 1, "升级不丢行");
        let row = &stats[0];
        assert_eq!(row.vendor, "mock-openai");
        assert_eq!(row.model, "grok-4");
        assert_eq!(row.api_key_hash, "", "历史流量不归属到 key");
        assert_eq!(row.prompt_tokens, 10, "prompt→input 映射");
        assert_eq!(row.completion_tokens, 2, "completion→output 映射");
        assert!((row.cost_usd_sum - 0.5).abs() < 1e-9, "成本总额保留");
        assert_eq!(row.request_count, 3);

        // 新写入走分账新形态：同键二次记账各自累加（新 UNIQUE 键生效，不与旧行冲突）。
        let pa = crate::database::TokenPreAggregator::new(
            std::sync::Arc::new(repo.clone()),
            std::time::Duration::from_secs(3600),
        );
        let keyed = crate::storage::KeyedUsage {
            vendor: "mock-openai".to_string(),
            model: "grok-4".to_string(),
            api_key_hash: "abc123".to_string(),
            prompt_tokens: 7,
            completion_tokens: 1,
            cache_read_tokens: 4,
            cache_creation_tokens: 2,
            input_cost: 0.01,
            output_cost: 0.02,
            cache_read_cost: 0.001,
            cache_creation_cost: 0.002,
        };
        pa.record_keyed(&keyed).await;
        pa.record_keyed(&keyed).await;
        pa.flush().await.expect("flush");

        let stats = repo.get_token_stats(None).await.expect("stats");
        assert_eq!(stats.len(), 2, "新写入落新形态行（与旧行按 hash 分账）");
        let ledger = stats
            .iter()
            .find(|r| r.api_key_hash == "abc123")
            .expect("新形态行");
        assert_eq!(ledger.prompt_tokens, 14, "两次记账累加");
        assert_eq!(ledger.cache_read_tokens, 8, "cache_read 入账");
        assert!((ledger.input_cost - 0.02).abs() < 1e-9, "分项成本累加");
    }

    /// 分账表形态：token_usage 含 api_key_hash、四类 token 与分项成本列；
    /// 组合列不残留；成本查询索引就位。
    #[tokio::test]
    async fn token_usage_ledger_shape() {
        let pool = memory_pool().await;
        init_schema(&pool).await.expect("init");

        let session = pool.get_session("admin").await.expect("session");
        let stmt = Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA table_info(token_usage)".to_string(),
        );
        let cols: HashSet<String> = session
            .connection()
            .expect("conn")
            .query_all_raw(stmt)
            .await
            .expect("pragma")
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect();
        for col in [
            "api_key_hash",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "input_cost",
            "output_cost",
            "cache_read_cost",
            "cache_creation_cost",
        ] {
            assert!(cols.contains(col), "分账列缺失 {col}");
        }
        assert!(
            !cols.contains("cost_usd_sum"),
            "组合成本列不得残留（分项替代）"
        );

        let ddl_stmt = Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE name='token_usage'".to_string(),
        );
        let ddl: String = session
            .connection()
            .expect("conn")
            .query_all_raw(ddl_stmt)
            .await
            .expect("ddl")
            .iter()
            .filter_map(|row| row.try_get::<String>("", "sql").ok())
            .next()
            .unwrap_or_default();
        assert!(
            ddl.contains("UNIQUE(vendor, model, api_key_hash, date, hour)"),
            "UNIQUE 键必须含 api_key_hash: {ddl}"
        );

        let idx_stmt = Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='token_usage'"
                .to_string(),
        );
        let indexes: HashSet<String> = session
            .connection()
            .expect("conn")
            .query_all_raw(idx_stmt)
            .await
            .expect("indexes")
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect();
        for index in ["idx_token_usage_date", "idx_token_usage_key_date"] {
            assert!(indexes.contains(index), "成本查询索引缺失 {index}");
        }
    }

    /// DOWN 可执行：v2→v1 逆序回滚后四表消失、历史行清空。
    #[tokio::test]
    async fn down_migration_rolls_back() {
        let pool = memory_pool().await;
        assert_eq!(
            apply_core_migrations(&pool).await.expect("apply"),
            CORE_SCHEMA_VERSION
        );
        assert_eq!(apply_core_migrations(&pool).await.expect("re-apply"), 0);
        rollback_core_all(&pool).await.expect("rollback");

        let tables = table_names(&pool).await;
        for t in [
            "request_logs",
            "token_usage",
            "vendor_status",
            "model_stats",
            "vault_keys",
        ] {
            assert!(!tables.contains(t), "回滚后 {t} 应消失");
        }
        assert!(applied_versions(&pool).await.is_empty(), "历史行应删除");
    }

    /// 只读库上迁移失败必须 fail-fast（Err），禁止静默降级为无库运行。
    #[tokio::test]
    async fn readonly_db_fails_fast() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("ro.db");
        let path_str = path.display().to_string();
        let rwc = DbPool::with_config(DbConfig {
            url: format!("sqlite://{path_str}?mode=rwc"),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("rwc pool");
        // 只建表、不记历史：模拟"表在、历史空"存量库，使只读重开后迁移真正
        // 尝试写（建表 no-op + 记历史行），而非被幂等短路掩盖。
        create_tables_raw(&rwc).await;
        drop(rwc);

        let ro = DbPool::with_config(DbConfig {
            url: format!("sqlite://{path_str}?mode=ro"),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        })
        .await
        .expect("ro connect");
        let err = init_schema(&ro).await.expect_err("只读库必须 fail-fast");
        assert!(!err.to_string().is_empty(), "错误须可输出");
    }
}
