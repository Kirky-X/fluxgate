// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! 核心表版本化迁移。
//!
//! 运行源唯一性：sqlite/postgres/mysql 的启动建表只走本模块的 FROZEN V1
//!（经 dbnexus `MigrationExecutor`），不再逐语句执行 `schema.*.sql`。
//! `src/database/schema.sql`（及 mysql/postgres 方言投影）现为可读文档——
//! 其定稿内容已冻结为下方的 V1 UP 常量；后续表/列变更走新迁移版本，
//! **禁止**改动 FROZEN V1（已部署库的历史行指向它）。
//!
//! 存量库基线策略 = 幂等重放：V1 UP 全为 `IF NOT EXISTS`，表已存在、历史为
//! 空的旧库重放为 no-op，随后记 v1 历史行（不重复建表）；缺列由
//! `repository::init_schema` 的 v0.1 列回填补齐。MySQL 索引不在 V1 UP 内
//! （MySQL 8 无 `CREATE INDEX IF NOT EXISTS`，重放会失败）——仍由
//! `ensure_indexes_mysql` 按 information_schema 探针幂等创建。
//!
//! 失败语义：迁移错误直接上浮（`Database::new` 转 fail-fast），无静默降级。

use dbnexus::{DatabaseType, DbError, DbPool, MigrationExecutor, MigrationFile};
use sea_orm::DatabaseBackend;
use std::path::PathBuf;

/// 核心表迁移版本（V1：request_logs/token_usage/vendor_status/model_stats）。
pub(crate) const CORE_SCHEMA_VERSION: u32 = 1;

/// V1 UP（sqlite，FROZEN——源自 schema.sql 定稿，含 cost 列与 4 索引）。
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

/// V1 UP（postgres，FROZEN——源自 schema.postgres.sql 定稿）。
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

/// V1 UP（mysql，FROZEN——源自 schema.mysql.sql 定稿；只含建表，索引走
/// `ensure_indexes_mysql`，见模块文档）。
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

/// V1 DOWN（三后端通用：DROP TABLE 隐式带走索引；`IF EXISTS` 使回滚可重入）。
const DOWN_COMMON: &str = r#"
DROP TABLE IF EXISTS model_stats;
DROP TABLE IF EXISTS vendor_status;
DROP TABLE IF EXISTS token_usage;
DROP TABLE IF EXISTS request_logs;
"#;

/// 按连接后端构造 V1 迁移文件（内存嵌入，无目录依赖——`-- UP`/`-- DOWN`
/// 标记行供 `MigrationExecutor` 切分；UP 体内无以 UP/DOWN 开头的行）。
/// 按连接后端构造 V1 迁移文件（内存嵌入，无目录依赖——`-- UP`/`-- DOWN`
/// 标记行供 `MigrationExecutor` 切分；UP 体内无以 UP/DOWN 开头的行）。
/// 未知后端返回 Err（调用方回落旧 DDL；静默按 sqlite 会在 exotic 后端上
/// 建错表）。
pub(crate) fn core_migration_file(backend: DatabaseBackend) -> Result<MigrationFile, DbError> {
    let (label, up) = match backend {
        DatabaseBackend::Postgres => ("postgres", UP_POSTGRES),
        DatabaseBackend::MySql => ("mysql", UP_MYSQL),
        DatabaseBackend::Sqlite => ("sqlite", UP_SQLITE),
        _ => {
            return Err(DbError::Config(
                "core v1 migration covers sqlite/postgres/mysql; \
                 other backends use the legacy DDL path"
                    .to_string(),
            ));
        }
    };
    let content = format!(
        "-- UP nexusflow core v1 ({label})\n{up}\n-- DOWN nexusflow core v1 ({label})\n{DOWN_COMMON}\n"
    );
    Ok(MigrationFile::new(
        CORE_SCHEMA_VERSION,
        format!("nexusflow core tables v1 ({label})"),
        PathBuf::from(format!("embedded:core_v1_{label}.sql")),
        content,
    ))
}

/// 应用核心 V1 迁移（幂等：v1 已在历史中即返回 0，不重放）。
///
/// 返回本次应用的迁移数（0 或 1）。调用方（`init_schema`）把 Err 直接上浮
/// 为启动 fail-fast。`DatabaseBackend` 非穷尽扩展出的未来后端返回
/// `DbError::Config`（调用方回落旧 DDL，见 `init_schema`）。
pub(crate) async fn apply_core_v1(pool: &DbPool) -> Result<u32, DbError> {
    // 先释放 session 再跑 executor：测试池 max_connections=1，持有 session
    // 的同时让 executor 另取连接会死等（clone 的只是池句柄）。
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
                "core v1 migration covers sqlite/postgres/mysql; \
                 other backends use the legacy DDL path"
                    .to_string(),
            ));
        }
    };
    let mut executor = MigrationExecutor::new(conn, db_type);
    executor.load_history().await?;
    if executor.get_all_versions().contains(&CORE_SCHEMA_VERSION) {
        return Ok(0);
    }
    executor
        .apply_migration_file_public(&core_migration_file(backend)?)
        .await?;
    Ok(1)
}

/// 回滚核心 V1（测试面：执行 DOWN 并删除 v1 历史行）。
/// 唯一调用点是 [`live_tests`] 的 DOWN 断言，cfg 门控与该模块逐位一致：
/// 非测试构建、以及非 db-sqlite 后端的测试构建均不参与编译。
#[cfg(all(test, feature = "db-sqlite"))]
pub(crate) async fn rollback_core_v1(pool: &DbPool) -> Result<(), DbError> {
    // 同 apply_core_v1：先释放 session（size=1 池防死等）。
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
                "core v1 rollback covers sqlite/postgres/mysql".to_string(),
            ));
        }
    };
    let executor = MigrationExecutor::new(conn, db_type);
    executor
        .rollback_version(CORE_SCHEMA_VERSION, &core_migration_file(backend)?)
        .await
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
            let file = core_migration_file(backend).expect("三后端必有 V1");
            assert_eq!(file.version(), CORE_SCHEMA_VERSION);
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

    /// DOWN 可提取且含四表 DROP（回滚路径不缺失）。
    #[test]
    fn v1_down_drops_all_tables() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let file = core_migration_file(backend).expect("三后端必有 V1");
            let down =
                MigrationExecutor::extract_down_sql(file.content()).expect("V1 必须有 DOWN 段");
            for table in TABLES {
                assert!(
                    down.contains(&format!("DROP TABLE IF EXISTS {table}")),
                    "{backend:?} DOWN 缺 {table}"
                );
            }
        }
    }

    /// FROZEN V1 与文档 schema.sql 表集合一致
    /// 双源并存，表集合漂移即红——文档改了必须同步出新迁移版本，
    /// 禁止静默改 V1。
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
        let v1 = core_migration_file(DatabaseBackend::Sqlite).expect("sqlite V1");
        // UP 段（DOWN 标记之前）参与比对
        let up = v1.content().split("-- DOWN").next().unwrap_or_default();
        assert_eq!(
            table_set(up),
            table_set(doc_sql),
            "FROZEN V1 与 schema.sql 表集合必须一致"
        );
    }
}

/// sqlite 生效面 live 测试（G4 验收：批量/全新库/存量基线/DOWN/只读 fail-fast）。
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

    /// V0.1 旧库表形态（无 cost 列）：手工建表模拟存量库，迁移历史为空。
    const LEGACY_TABLES_NO_COST: [&str; 4] = [
        "CREATE TABLE request_logs (id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT UNIQUE NOT NULL, timestamp INTEGER NOT NULL, endpoint TEXT NOT NULL, input_format TEXT NOT NULL, abstract_model TEXT, messages TEXT NOT NULL, actual_vendor TEXT NOT NULL, actual_model TEXT NOT NULL, routing_strategy TEXT NOT NULL, status_code INTEGER, response TEXT, error_message TEXT, prompt_tokens INTEGER, completion_tokens INTEGER, total_tokens INTEGER, duration_ms INTEGER)",
        "CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, vendor TEXT NOT NULL, model TEXT NOT NULL, date TEXT NOT NULL, hour INTEGER NOT NULL, request_count INTEGER DEFAULT 0, total_tokens INTEGER DEFAULT 0, prompt_tokens INTEGER DEFAULT 0, completion_tokens INTEGER DEFAULT 0, UNIQUE(vendor, model, date, hour))",
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

    /// 全新库：迁移建四表+索引、记 v1、幂等重跑 0；20 行批量（含注入形态
    /// 载荷）逐行可读、字段完整——参数化绑定无手写转义。
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
        assert_eq!(applied_versions(&pool).await, vec![CORE_SCHEMA_VERSION]);

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
    /// 不重复建表、缺列回填、记 v1、可正常写入。
    #[tokio::test]
    async fn existing_db_without_history_is_baselined() {
        let pool = memory_pool().await;
        create_tables_raw(&pool).await;

        init_schema(&pool).await.expect("存量基线不报错");
        assert_eq!(applied_versions(&pool).await, vec![CORE_SCHEMA_VERSION]);

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

    /// DOWN 可执行：回滚后四表消失、v1 历史行删除。
    #[tokio::test]
    async fn down_migration_rolls_back() {
        let pool = memory_pool().await;
        assert_eq!(apply_core_v1(&pool).await.expect("apply"), 1);
        assert_eq!(apply_core_v1(&pool).await.expect("re-apply"), 0);
        rollback_core_v1(&pool).await.expect("rollback");

        let tables = table_names(&pool).await;
        for t in [
            "request_logs",
            "token_usage",
            "vendor_status",
            "model_stats",
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
        // 只建表、不记历史：模拟"表在、历史空"的存量库，使只读重开后迁移
        // 真正尝试写（建表 no-op + 记历史行），而非被幂等短路掩盖。
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
