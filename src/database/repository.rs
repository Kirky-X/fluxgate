// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::{LogFilter, RequestLog, TokenUsageRow, VendorStatusRecord};
use dbnexus::{DbError, DbPool};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
    Value,
};

/// 按连接后端选择方言投影（db-backend-selection T004）。
/// `schema.sql`（SQLite）为权威源；schema.postgres.sql / schema.mysql.sql 为
/// 方言投影，头注释声明同步义务——权威源加表/加列时三处同改。
pub(crate) fn schema_sql_for_backend(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Postgres => include_str!("schema.postgres.sql"),
        DatabaseBackend::MySql => include_str!("schema.mysql.sql"),
        // DatabaseBackend 带 #[non_exhaustive]：剩余可能（含未来新后端）回落权威源
        _ => include_str!("schema.sql"),
    }
}

/// schema 初始化单一实现（`Database::new` 与 `Repository::initialize` 共用，
/// 消除两处逐语句循环副本）：按连接后端加载方言投影，逐语句 `execute_raw_ddl`；
/// MySQL 另行幂等建索引（MySQL 8 无 `CREATE INDEX IF NOT EXISTS`，故方言
/// 投影只含建表）。
pub(crate) async fn init_schema(pool: &DbPool) -> Result<(), DbError> {
    let session = pool.get_session("admin").await?;
    let backend = session.connection()?.get_database_backend();
    for statement in schema_sql_for_backend(backend).split(';') {
        let trimmed = statement.trim();
        if !trimmed.is_empty() {
            session.execute_raw_ddl(trimmed).await?;
        }
    }
    if backend == DatabaseBackend::MySql {
        ensure_indexes_mysql(session.connection()?).await?;
    }
    Ok(())
}

/// MySQL 索引幂等创建：`information_schema.statistics` 查存在性，缺则建。
async fn ensure_indexes_mysql(conn: &DatabaseConnection) -> Result<(), DbError> {
    const INDEXES: [(&str, &str, &str); 4] = [
        ("idx_timestamp", "request_logs", "timestamp"),
        ("idx_vendor", "request_logs", "actual_vendor"),
        ("idx_model", "request_logs", "abstract_model"),
        ("idx_request_id", "request_logs", "request_id"),
    ];
    for (index, table, column) in INDEXES {
        let exists_sql = format!(
            "SELECT INDEX_NAME FROM information_schema.statistics \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = '{table}' AND INDEX_NAME = '{index}'"
        );
        let stmt = Statement::from_sql_and_values(DatabaseBackend::MySql, &exists_sql, Vec::new());
        if !conn.query_all_raw(stmt).await?.is_empty() {
            continue;
        }
        let create_sql = format!("CREATE INDEX {index} ON {table} ({column})");
        let stmt = Statement::from_sql_and_values(DatabaseBackend::MySql, &create_sql, Vec::new());
        conn.execute_raw(stmt).await?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct Repository {
    pub pool: DbPool,
}

impl Repository {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub async fn initialize(&self) -> Result<(), DbError> {
        init_schema(&self.pool).await?;

        // 旧库迁移：v0.1 数据库可能缺少 cost_usd 列。
        // 各后端均无 ALTER TABLE ADD COLUMN IF NOT EXISTS 的可移植写法，
        // 探针查列存在性（sqlite PRAGMA / pg·mysql information_schema），
        // 缺则 ALTER TABLE ADD COLUMN。
        self.ensure_column_exists("request_logs", "cost_usd", "REAL DEFAULT 0.0")
            .await?;
        self.ensure_column_exists("token_usage", "cost_usd_sum", "REAL DEFAULT 0.0")
            .await?;

        Ok(())
    }

    /// 检查指定表是否已有某列，不存在则 ALTER TABLE ADD COLUMN。
    /// 多后端（db-backend-selection T005）：探针语句随连接后端分派。
    async fn ensure_column_exists(
        &self,
        table: &str,
        column: &str,
        column_def: &str,
    ) -> Result<(), DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();

        // 表名/列名仅来自本文件内的常量调用点，无用户输入拼接面
        let (probe_sql, name_key) = Self::column_probe(backend, table, column);
        let stmt = Statement::from_sql_and_values(backend, &probe_sql, Vec::new());
        let rows = conn.query_all_raw(stmt).await?;

        let column_exists = rows.iter().any(|row| {
            row.try_get::<String>("", name_key)
                .map(|name| name == column)
                .unwrap_or(false)
        });

        if !column_exists {
            let alter_sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, column_def);
            let stmt = Statement::from_sql_and_values(backend, &alter_sql, Vec::new());
            conn.execute_raw(stmt).await?;
            tracing::info!(
                table = table,
                column = column,
                "migrated: added missing column"
            );
        }

        Ok(())
    }

    /// 列存在性探针语句与结果列名（纯函数，db-backend-selection T005）。
    /// sqlite 的 PRAGMA table_info 返回键为 `name`，information_schema 为
    /// `COLUMN_NAME`——语句与键随元组成对返回，调用方不再分支。
    fn column_probe(backend: DatabaseBackend, table: &str, column: &str) -> (String, &'static str) {
        match backend {
            DatabaseBackend::Sqlite => (format!("PRAGMA table_info({table})"), "name"),
            DatabaseBackend::MySql => (
                format!(
                    "SELECT COLUMN_NAME FROM information_schema.columns \
                     WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = '{table}' AND COLUMN_NAME = '{column}'"
                ),
                "COLUMN_NAME",
            ),
            // DatabaseBackend 带 #[non_exhaustive]：其余服务端后端（含
            // postgres）共用 ANSI information_schema，按当前 schema 限定
            _ => (
                format!(
                    "SELECT COLUMN_NAME FROM information_schema.columns \
                     WHERE TABLE_SCHEMA = current_schema() AND TABLE_NAME = '{table}' AND COLUMN_NAME = '{column}'"
                ),
                "COLUMN_NAME",
            ),
        }
    }

    pub async fn log_request(&self, log: &RequestLog) -> Result<(), DbError> {
        let stmt = Self::insert_log_statement(log);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        conn.execute_raw(stmt).await?;
        Ok(())
    }

    /// recId=9①：单事务批量写入 — N 条 INSERT 包在一个事务里 commit，
    /// 替换 AsyncLogger 逐条 await 的逐事务模式（SQLite 每 INSERT 一次
    /// fsync，批量事务把 N 次 fsync 合为 1 次）。
    ///
    /// 事务承载（与 plan 的偏差，以源码为准）：plan 写 `session.begin_transaction()`
    /// 包 N 条 INSERT —— 实核实 dbnexus `Session::execute_raw(&self, sql: &str)`
    /// 只接收裸 SQL（无参数绑定），事务内执行走 `tx.execute_unprepared`，
    /// 需要手写转义（messages/response/error_message 是任意用户内容，
    /// 引入注入面）。故改为 `session.connection()?.begin()` 取 sea-orm
    /// `DatabaseTransaction`，参数化 Statement 循环执行后 `tx.commit()` ——
    /// 语义等价（单事务批量 + 提交），绑定安全，连接仍来自 dbnexus 池。
    pub async fn log_requests_batch(&self, logs: &[RequestLog]) -> Result<(), DbError> {
        if logs.is_empty() {
            return Ok(());
        }
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let tx = conn.begin().await?;
        for log in logs {
            let stmt = Self::insert_log_statement(log);
            // sea-orm 2.0：预构建 Statement 走 execute_raw（execute 接收
            // StatementBuilder，即 sea_query 语句类型）
            if let Err(e) = tx.execute_raw(stmt).await {
                // 单条失败回滚整批（事务语义：不丢也不留半批）
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// request_logs 的参数化 INSERT Statement（log_request / log_requests_batch 共用）
    fn insert_log_statement(log: &RequestLog) -> Statement {
        const SQL: &str = r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms,
                cost_usd
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#;

        let values: Vec<Value> = vec![
            log.request_id.as_str().into(),
            log.timestamp.into(),
            log.endpoint.as_str().into(),
            log.input_format.as_str().into(),
            log.abstract_model.as_deref().into(),
            log.messages.as_str().into(),
            log.actual_vendor.as_str().into(),
            log.actual_model.as_str().into(),
            log.routing_strategy.as_str().into(),
            log.status_code.into(),
            log.response.as_deref().into(),
            log.error_message.as_deref().into(),
            log.prompt_tokens.into(),
            log.completion_tokens.into(),
            log.total_tokens.into(),
            log.duration_ms.into(),
            log.cost_usd.into(),
        ];

        Statement::from_sql_and_values(DatabaseBackend::Sqlite, SQL, values)
    }

    pub async fn query_logs(&self, filter: &LogFilter) -> Result<Vec<RequestLog>, DbError> {
        let mut query = String::from("SELECT * FROM request_logs WHERE 1=1");
        let mut values: Vec<Value> = Vec::new();

        if let Some(start) = filter.start_time {
            query.push_str(" AND timestamp >= ?");
            values.push(start.into());
        }

        if let Some(ref vendor) = filter.vendor {
            query.push_str(" AND actual_vendor = ?");
            values.push(vendor.as_str().into());
        }

        if let Some(ref model) = filter.model {
            query.push_str(" AND actual_model = ?");
            values.push(model.as_str().into());
        }

        query.push_str(" ORDER BY timestamp DESC");

        if filter.limit > 0 {
            query.push_str(" LIMIT ?");
            values.push((filter.limit as i64).into());
        }

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, &query, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    pub async fn search_logs(&self, keyword: &str, limit: u32) -> Result<Vec<RequestLog>, DbError> {
        let pattern = format!("%{}%", keyword);
        let sql = r#"
            SELECT * FROM request_logs
            WHERE messages LIKE ? OR error_message LIKE ?
            ORDER BY timestamp DESC
            LIMIT ?
        "#;

        let values: Vec<Value> = vec![
            pattern.as_str().into(),
            pattern.as_str().into(),
            (limit as i64).into(),
        ];

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    pub async fn get_token_stats(
        &self,
        vendor: Option<&str>,
    ) -> Result<Vec<TokenUsageRow>, DbError> {
        let (sql, values): (String, Vec<Value>) = if let Some(v) = vendor {
            (
                "SELECT * FROM token_usage WHERE vendor = ? ORDER BY date DESC, hour DESC"
                    .to_string(),
                vec![v.into()],
            )
        } else {
            (
                "SELECT * FROM token_usage ORDER BY date DESC, hour DESC".to_string(),
                Vec::new(),
            )
        };

        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, &sql, values);
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_token_usage).collect())
    }

    pub async fn get_all_vendor_status(&self) -> Result<Vec<VendorStatusRecord>, DbError> {
        let sql = "SELECT * FROM vendor_status ORDER BY vendor";
        let stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, Vec::new());
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_vendor_status).collect())
    }
}

fn map_request_log(row: &QueryResult) -> RequestLog {
    RequestLog {
        id: row.try_get::<Option<i64>>("", "id").unwrap_or(None),
        request_id: row.try_get("", "request_id").unwrap_or_default(),
        timestamp: row.try_get("", "timestamp").unwrap_or_default(),
        endpoint: row.try_get("", "endpoint").unwrap_or_default(),
        input_format: row.try_get("", "input_format").unwrap_or_default(),
        abstract_model: row
            .try_get::<Option<String>>("", "abstract_model")
            .unwrap_or(None),
        messages: row.try_get("", "messages").unwrap_or_default(),
        actual_vendor: row.try_get("", "actual_vendor").unwrap_or_default(),
        actual_model: row.try_get("", "actual_model").unwrap_or_default(),
        routing_strategy: row.try_get("", "routing_strategy").unwrap_or_default(),
        status_code: row
            .try_get::<Option<i32>>("", "status_code")
            .unwrap_or(None),
        response: row
            .try_get::<Option<String>>("", "response")
            .unwrap_or(None),
        error_message: row
            .try_get::<Option<String>>("", "error_message")
            .unwrap_or(None),
        prompt_tokens: row
            .try_get::<Option<i32>>("", "prompt_tokens")
            .unwrap_or(None),
        completion_tokens: row
            .try_get::<Option<i32>>("", "completion_tokens")
            .unwrap_or(None),
        total_tokens: row
            .try_get::<Option<i32>>("", "total_tokens")
            .unwrap_or(None),
        duration_ms: row
            .try_get::<Option<i64>>("", "duration_ms")
            .unwrap_or(None),
        cost_usd: row.try_get::<Option<f64>>("", "cost_usd").unwrap_or(None),
    }
}

fn map_token_usage(row: &QueryResult) -> TokenUsageRow {
    TokenUsageRow {
        id: row.try_get::<Option<i64>>("", "id").unwrap_or(None),
        vendor: row.try_get("", "vendor").unwrap_or_default(),
        model: row.try_get("", "model").unwrap_or_default(),
        date: row.try_get("", "date").unwrap_or_default(),
        hour: row.try_get("", "hour").unwrap_or_default(),
        request_count: row.try_get("", "request_count").unwrap_or_default(),
        total_tokens: row.try_get("", "total_tokens").unwrap_or_default(),
        prompt_tokens: row.try_get("", "prompt_tokens").unwrap_or_default(),
        completion_tokens: row.try_get("", "completion_tokens").unwrap_or_default(),
        cost_usd_sum: row.try_get("", "cost_usd_sum").unwrap_or_default(),
    }
}

fn map_vendor_status(row: &QueryResult) -> VendorStatusRecord {
    VendorStatusRecord {
        vendor: row.try_get("", "vendor").unwrap_or_default(),
        status: row.try_get("", "status").unwrap_or_default(),
        last_updated: row.try_get("", "last_updated").unwrap_or_default(),
        window_used_tokens: row.try_get("", "window_used_tokens").unwrap_or_default(),
        window_reset_at: row
            .try_get::<Option<i64>>("", "window_reset_at")
            .unwrap_or(None),
        error_count: row.try_get("", "error_count").unwrap_or_default(),
    }
}

#[cfg(test)]
mod schema_tests {
    use super::schema_sql_for_backend;
    use sea_orm::DatabaseBackend;

    /// 断言只看 DDL：方言投影头注释含类型映射说明（"AUTOINCREMENT -> ..."、
    /// "CREATE INDEX IF NOT EXISTS" 等字样），须剥离注释行再匹配
    fn ddl(sql: &str) -> String {
        sql.lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 三后端投影均非空、均含四张表（db-backend-selection R-schema-001）
    #[test]
    fn all_backends_define_the_same_tables() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let sql = ddl(schema_sql_for_backend(backend));
            assert!(!sql.is_empty(), "{backend:?}: 投影为空");
            for table in [
                "request_logs",
                "token_usage",
                "vendor_status",
                "model_stats",
            ] {
                assert!(
                    sql.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
                    "{backend:?}: 缺表 {table}"
                );
            }
        }
    }

    /// mysql 投影只含建表（索引由 ensure_indexes_mysql 幂等创建）；
    /// sqlite/pg 投影自带 CREATE INDEX IF NOT EXISTS（R-schema-001/002）
    #[test]
    fn mysql_projection_defers_indexes_to_runtime() {
        assert!(!ddl(schema_sql_for_backend(DatabaseBackend::MySql)).contains("CREATE INDEX"));
        assert!(ddl(schema_sql_for_backend(DatabaseBackend::Sqlite))
            .contains("CREATE INDEX IF NOT EXISTS"));
        assert!(ddl(schema_sql_for_backend(DatabaseBackend::Postgres))
            .contains("CREATE INDEX IF NOT EXISTS"));
    }

    /// 类型映射抽查：自增主键与浮点（R-schema-001）
    #[test]
    fn dialect_type_mappings() {
        let pg = ddl(schema_sql_for_backend(DatabaseBackend::Postgres));
        assert!(pg.contains("GENERATED ALWAYS AS IDENTITY"));
        assert!(pg.contains("DOUBLE PRECISION"));
        assert!(!pg.contains("AUTOINCREMENT"));
        let mysql = ddl(schema_sql_for_backend(DatabaseBackend::MySql));
        assert!(mysql.contains("AUTO_INCREMENT"));
        assert!(mysql.contains("DOUBLE"));
        assert!(!mysql.contains("AUTOINCREMENT"));
    }

    /// 列探针语句分派：语句与结果键成对返回（R-schema-002）
    #[test]
    fn column_probe_dispatch_by_backend() {
        use super::Repository;

        let (sql, key) =
            Repository::column_probe(DatabaseBackend::Sqlite, "request_logs", "cost_usd");
        assert_eq!(sql, "PRAGMA table_info(request_logs)");
        assert_eq!(key, "name");

        let (sql, key) =
            Repository::column_probe(DatabaseBackend::MySql, "request_logs", "cost_usd");
        assert!(sql.contains("TABLE_SCHEMA = DATABASE()"));
        assert!(sql.contains("TABLE_NAME = 'request_logs'"));
        assert!(sql.contains("COLUMN_NAME = 'cost_usd'"));
        assert_eq!(key, "COLUMN_NAME");

        let (sql, key) =
            Repository::column_probe(DatabaseBackend::Postgres, "request_logs", "cost_usd");
        assert!(sql.contains("TABLE_SCHEMA = current_schema()"));
        assert_eq!(key, "COLUMN_NAME");
    }
}
