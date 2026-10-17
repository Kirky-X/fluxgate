// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::{
    CostGroupBy, CostUsageRow, LogFilter, RequestLog, TokenUsageRow, VendorStatusRecord,
};
use crate::i18n::t_args;
use dbnexus::{DbError, DbPool, Session};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, Value,
};

/// LIKE 通配符转义（纯函数）：`\` → `\\`、`%` → `\%`、`_` → `\_`，
/// 配合 SQL 侧 `ESCAPE ?`（转义符经绑定参数传入）让用户输入按字面匹配。
pub(crate) fn escape_like(keyword: &str) -> String {
    keyword
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 按连接后端选择方言投影。schema.sql（SQLite）为可读文档源；
/// 启动建表已走版本化迁移，本函数仅服务非穷尽后端兜底 DDL 与一致性单测
pub(crate) fn schema_sql_for_backend(backend: DatabaseBackend) -> &'static str {
    match backend {
        DatabaseBackend::Postgres => include_str!("schema.postgres.sql"),
        DatabaseBackend::MySql => include_str!("schema.mysql.sql"),
        // DatabaseBackend 带 #[non_exhaustive]：剩余可能（含未来新后端）回落权威源
        _ => include_str!("schema.sql"),
    }
}

/// schema 初始化单一实现（Database::new 与 Repository::initialize 共用）：
/// sqlite/postgres/mysql 走 dbnexus 版本化迁移（V1..=V2），其余后端回落
/// 逐语句 DDL；MySQL 普通索引幂等创建（无 CREATE INDEX IF NOT EXISTS），
/// v0.1 列回填保持在迁移之后。迁移失败直接返回 Err（启动 fail-fast）；
/// session 逐段获取逐段释放（测试池 max_connections=1）
pub(crate) async fn init_schema(pool: &DbPool) -> Result<(), DbError> {
    let backend = {
        let session = pool.get_session("admin").await?;
        session.connection()?.get_database_backend()
    };
    match backend {
        DatabaseBackend::Postgres | DatabaseBackend::MySql | DatabaseBackend::Sqlite => {
            crate::database::migration::apply_core_migrations(pool).await?;
        }
        _ => {
            let session = pool.get_session("admin").await?;
            for statement in schema_sql_for_backend(backend).split(';') {
                let trimmed = statement.trim();
                if !trimmed.is_empty() {
                    session.execute_raw_ddl(trimmed).await?;
                }
            }
        }
    }
    if backend == DatabaseBackend::MySql {
        let session = pool.get_session("admin").await?;
        ensure_indexes_mysql(session.connection()?).await?;
    }
    {
        let session = pool.get_session("admin").await?;
        migrate_v0_1_columns(&session, backend).await?;
    }
    Ok(())
}

/// V0.1 旧库列迁移：并入 init_schema 使迁移随生产建链可达。各后端均无
/// ALTER TABLE ADD COLUMN IF NOT EXISTS 的可移植写法，探针查列、缺则 ALTER
async fn migrate_v0_1_columns(session: &Session, backend: DatabaseBackend) -> Result<(), DbError> {
    let cost_type = match backend {
        DatabaseBackend::MySql => "DOUBLE DEFAULT 0.0",
        DatabaseBackend::Sqlite => "REAL DEFAULT 0.0",
        _ => "DOUBLE PRECISION DEFAULT 0.0",
    };
    Repository::ensure_column_exists(session, backend, "request_logs", "cost_usd", cost_type)
        .await?;
    Ok(())
}

/// MySQL 索引幂等创建：`information_schema.statistics` 查存在性，缺则建。
/// 含 成本查询索引（idx_token_usage_date / idx_token_usage_key_date，
/// 与 sqlite/postgres 的 V2 UP 内联索引同语义；MySQL 无
/// `CREATE INDEX IF NOT EXISTS` 故统一走本探针）。
async fn ensure_indexes_mysql(conn: &DatabaseConnection) -> Result<(), DbError> {
    const INDEXES: [(&str, &str, &str); 6] = [
        ("idx_timestamp", "request_logs", "timestamp"),
        ("idx_vendor", "request_logs", "actual_vendor"),
        ("idx_model", "request_logs", "abstract_model"),
        ("idx_request_id", "request_logs", "request_id"),
        ("idx_token_usage_date", "token_usage", "date"),
        (
            "idx_token_usage_key_date",
            "token_usage",
            "api_key_hash, date",
        ),
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

    /// 测试面入口（生产链走 `Database::new` → `init_schema`，迁移已并入后者）
    pub async fn initialize(&self) -> Result<(), DbError> {
        init_schema(&self.pool).await
    }

    /// 检查指定表是否已有某列，不存在则 ALTER TABLE ADD COLUMN。
    /// 多后端：探针语句随连接后端分派。
    async fn ensure_column_exists(
        session: &Session,
        backend: DatabaseBackend,
        table: &str,
        column: &str,
        column_def: &str,
    ) -> Result<(), DbError> {
        let conn = session.connection()?;

        // 表名/列名仅来自本文件内的常量调用点，无用户输入拼接面
        let (probe_sql, name_key) = Repository::column_probe(backend, table, column);
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
                "{}",
                t_args(
                    "database-repo-column-migrated",
                    &[("table", table.to_string()), ("column", column.to_string())],
                )
            );
        }

        Ok(())
    }

    /// 列存在性探针语句与结果列名（纯函数）。
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
        let session = self.pool.get_session("admin").await?;
        let (sql, params) = Self::insert_log_parts(log);
        session.execute_with_params(&sql, &params).await?;
        Ok(())
    }

    /// 单事务参数化批量写入：N 条 INSERT 包在一个事务里 commit（N 次 fsync
    /// 合为 1），参数绑定无手写转义（不用裸 SQL batch——用户内容拼接引入
    /// 注入面）；单条失败回滚整批
    pub async fn log_requests_batch(&self, logs: &[RequestLog]) -> Result<(), DbError> {
        if logs.is_empty() {
            return Ok(());
        }
        let session = self.pool.get_session("admin").await?;
        session.begin_transaction().await?;
        let mut failed: Option<DbError> = None;
        for log in logs {
            let (sql, params) = Self::insert_log_parts(log);
            if let Err(e) = session.execute_with_params(&sql, &params).await {
                failed = Some(e);
                break;
            }
        }
        match failed {
            // commit 失败同样尝试回滚，不残留开事务连接
            None => {
                if let Err(e) = session.commit().await {
                    let _ = session.rollback().await;
                    return Err(e);
                }
                Ok(())
            }
            Some(e) => {
                let _ = session.rollback().await;
                Err(e)
            }
        }
    }

    /// request_logs 参数化 INSERT（`log_request`/`log_requests_batch` 共用）。
    /// 占位符统一 `?`：`Statement::from_sql_and_values` 按 backend 标注翻译
    ///（postgres 下 `?` → `$n`），调用方不再按后端分支。
    fn insert_log_parts(log: &RequestLog) -> (String, Vec<serde_json::Value>) {
        const SQL: &str = r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms,
                cost_usd
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#;
        let params = vec![
            serde_json::json!(log.request_id),
            serde_json::json!(log.timestamp),
            serde_json::json!(log.endpoint),
            serde_json::json!(log.input_format),
            serde_json::json!(log.abstract_model),
            serde_json::json!(log.messages),
            serde_json::json!(log.actual_vendor),
            serde_json::json!(log.actual_model),
            serde_json::json!(log.routing_strategy),
            serde_json::json!(log.status_code),
            serde_json::json!(log.response),
            serde_json::json!(log.error_message),
            serde_json::json!(log.prompt_tokens),
            serde_json::json!(log.completion_tokens),
            serde_json::json!(log.total_tokens),
            serde_json::json!(log.duration_ms),
            serde_json::json!(log.cost_usd),
        ];
        (SQL.to_string(), params)
    }

    pub async fn query_logs(&self, filter: &LogFilter) -> Result<Vec<RequestLog>, DbError> {
        let mut query = String::from("SELECT * FROM request_logs WHERE 1=1");
        let mut values: Vec<Value> = Vec::new();

        if let Some(start) = filter.start_time {
            query.push_str(" AND timestamp >= ?");
            values.push(start.into());
        }

        if let Some(end) = filter.end_time {
            query.push_str(" AND timestamp <= ?");
            values.push(end.into());
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

        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let stmt = Statement::from_sql_and_values(conn.get_database_backend(), &query, values);
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    pub async fn search_logs(&self, keyword: &str, limit: u32) -> Result<Vec<RequestLog>, DbError> {
        // 通配符转义：keyword 中的 %/_ 按字面匹配（防止用户输入 % 全表通配）；
        // 转义符 '\' 作为绑定参数传递——sqlite 无默认转义符必须显式 ESCAPE，
        // pg/mysql 默认即反斜杠，显式子句语义一致，且规避三端字符串字面量
        // 对反斜杠转义的方言差异
        let pattern = format!("%{}%", escape_like(keyword));

        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        // LIKE 的大小写语义随 collation 漂移：sqlite/mysql（默认 collation）
        // ASCII 不区分大小写，postgres 区分——ILIKE 对齐 sqlite 既有行为。
        // 性能边界（审查 MEDIUM）：%kw% 前置通配在 postgres 下无法走 btree
        // （LIKE 同样不可用，非回归）；request_logs 无界增长，数据达百万行
        // 前建议加 pg_trgm GIN 索引（CREATE EXTENSION pg_trgm; ... USING gin
        // (messages gin_trgm_ops)）——低频 admin 端点，暂不强制
        let like_op = Self::like_op(backend);
        let sql = format!(
            r#"
            SELECT * FROM request_logs
            WHERE messages {like_op} ? ESCAPE ? OR error_message {like_op} ? ESCAPE ?
            ORDER BY timestamp DESC
            LIMIT ?
        "#
        );

        let values: Vec<Value> = vec![
            pattern.as_str().into(),
            "\\".into(),
            pattern.as_str().into(),
            "\\".into(),
            (limit as i64).into(),
        ];

        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_request_log).collect())
    }

    /// 模糊匹配算子（纯函数）：postgres 用 ILIKE，其余后端 LIKE
    fn like_op(backend: DatabaseBackend) -> &'static str {
        match backend {
            DatabaseBackend::Postgres => "ILIKE",
            _ => "LIKE",
        }
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

        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let stmt = Statement::from_sql_and_values(conn.get_database_backend(), &sql, values);
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows.iter().map(map_token_usage).collect())
    }

    /// 成本分账聚合查询：按 vendor 或 model 分组，对四类 token 与
    /// 分项成本求和。`from`/`to` 为 `YYYY-MM-DD` 日期闭区间（None = 不限）；
    /// `api_key_hash` 为请求方 key 摘要过滤（None = 不限）。空窗口返回空集。
    /// 分组列白名单枚举（调用方已校验，SQL 插值安全）。
    pub async fn query_cost_usage(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        group_by: CostGroupBy,
        api_key_hash: Option<&str>,
    ) -> Result<Vec<CostUsageRow>, DbError> {
        let dimension = match group_by {
            CostGroupBy::Vendor => "vendor",
            CostGroupBy::Model => "model",
        };
        let mut sql = format!(
            "SELECT {dimension} AS dimension, \
             SUM(request_count) AS request_count, SUM(input_tokens) AS input_tokens, \
             SUM(output_tokens) AS output_tokens, SUM(cache_read_tokens) AS cache_read_tokens, \
             SUM(cache_creation_tokens) AS cache_creation_tokens, SUM(input_cost) AS input_cost, \
             SUM(output_cost) AS output_cost, SUM(cache_read_cost) AS cache_read_cost, \
             SUM(cache_creation_cost) AS cache_creation_cost \
             FROM token_usage"
        );
        let mut values: Vec<Value> = Vec::new();
        let mut clauses: Vec<String> = Vec::new();
        if let Some(from) = from {
            clauses.push("date >= ?".to_string());
            values.push(from.into());
        }
        if let Some(to) = to {
            clauses.push("date <= ?".to_string());
            values.push(to.into());
        }
        if let Some(hash) = api_key_hash {
            clauses.push("api_key_hash = ?".to_string());
            values.push(hash.into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(&format!(" GROUP BY {dimension} ORDER BY {dimension}"));

        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let stmt = Statement::from_sql_and_values(conn.get_database_backend(), &sql, values);
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows
            .iter()
            .map(|row| {
                let input_cost: f64 = row.try_get("", "input_cost").unwrap_or_default();
                let output_cost: f64 = row.try_get("", "output_cost").unwrap_or_default();
                let cache_read_cost: f64 = row.try_get("", "cache_read_cost").unwrap_or_default();
                let cache_creation_cost: f64 =
                    row.try_get("", "cache_creation_cost").unwrap_or_default();
                let dimension: String = row.try_get("", "dimension").unwrap_or_default();
                let (vendor, model) = match group_by {
                    CostGroupBy::Vendor => (Some(dimension), None),
                    CostGroupBy::Model => (None, Some(dimension)),
                };
                CostUsageRow {
                    vendor,
                    model,
                    request_count: row.try_get("", "request_count").unwrap_or_default(),
                    input_tokens: row.try_get("", "input_tokens").unwrap_or_default(),
                    output_tokens: row.try_get("", "output_tokens").unwrap_or_default(),
                    cache_read_tokens: row.try_get("", "cache_read_tokens").unwrap_or_default(),
                    cache_creation_tokens: row
                        .try_get("", "cache_creation_tokens")
                        .unwrap_or_default(),
                    input_cost,
                    output_cost,
                    cache_read_cost,
                    cache_creation_cost,
                    total_cost: input_cost + output_cost + cache_read_cost + cache_creation_cost,
                }
            })
            .collect())
    }

    pub async fn get_all_vendor_status(&self) -> Result<Vec<VendorStatusRecord>, DbError> {
        let sql = "SELECT * FROM vendor_status ORDER BY vendor";
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let stmt = Statement::from_sql_and_values(conn.get_database_backend(), sql, Vec::new());
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
    let input: i64 = row.try_get("", "input_tokens").unwrap_or_default();
    let output: i64 = row.try_get("", "output_tokens").unwrap_or_default();
    let cache_read: i64 = row.try_get("", "cache_read_tokens").unwrap_or_default();
    let cache_creation: i64 = row.try_get("", "cache_creation_tokens").unwrap_or_default();
    let input_cost: f64 = row.try_get("", "input_cost").unwrap_or_default();
    let output_cost: f64 = row.try_get("", "output_cost").unwrap_or_default();
    let cache_read_cost: f64 = row.try_get("", "cache_read_cost").unwrap_or_default();
    let cache_creation_cost: f64 = row.try_get("", "cache_creation_cost").unwrap_or_default();
    TokenUsageRow {
        id: row.try_get::<Option<i64>>("", "id").unwrap_or(None),
        vendor: row.try_get("", "vendor").unwrap_or_default(),
        model: row.try_get("", "model").unwrap_or_default(),
        api_key_hash: row.try_get("", "api_key_hash").unwrap_or_default(),
        date: row.try_get("", "date").unwrap_or_default(),
        hour: row.try_get("", "hour").unwrap_or_default(),
        request_count: row.try_get("", "request_count").unwrap_or_default(),
        total_tokens: input + output + cache_read + cache_creation,
        prompt_tokens: input,
        completion_tokens: output,
        cost_usd_sum: input_cost + output_cost + cache_read_cost + cache_creation_cost,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
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
        // MySQL 要求 BLOB/TEXT 索引必须带前缀长度（ERROR 1170）：键规格列
        // （PK/UNIQUE/索引覆盖）必须是 VARCHAR（回归钉住）
        assert!(!mysql.contains("TEXT UNIQUE"));
        assert!(!mysql.contains("TEXT PRIMARY KEY"));
        assert!(mysql.contains("request_id VARCHAR(255)"));
        assert!(mysql.contains("vendor VARCHAR(255)"));
        assert!(mysql.contains("actual_vendor VARCHAR(255)"));
    }

    /// LIKE 通配符转义：三类元字符 + 反斜杠优先（R-schema-003 关联的
    /// search 输入面收敛）
    #[test]
    fn escape_like_escapes_wildcards_and_backslash() {
        assert_eq!(super::escape_like("abc"), "abc");
        assert_eq!(super::escape_like("100%"), "100\\%");
        assert_eq!(super::escape_like("a_b"), "a\\_b");
        assert_eq!(super::escape_like("a\\%b"), "a\\\\\\%b");
    }

    /// 三投影逐列集合守卫（R-schema-001）：每表全部列名
    /// 必须在三份投影的对应建表段中出现——防「加列只改权威源」漂移
    #[test]
    fn column_sets_match_across_projections() {
        let tables: [(&str, &[&str]); 4] = [
            (
                "request_logs",
                &[
                    "id",
                    "request_id",
                    "timestamp",
                    "endpoint",
                    "input_format",
                    "abstract_model",
                    "messages",
                    "actual_vendor",
                    "actual_model",
                    "routing_strategy",
                    "status_code",
                    "response",
                    "error_message",
                    "prompt_tokens",
                    "completion_tokens",
                    "total_tokens",
                    "duration_ms",
                    "cost_usd",
                ],
            ),
            (
                "token_usage",
                &[
                    "id",
                    "vendor",
                    "model",
                    "api_key_hash",
                    "date",
                    "hour",
                    "request_count",
                    "input_tokens",
                    "output_tokens",
                    "cache_read_tokens",
                    "cache_creation_tokens",
                    "input_cost",
                    "output_cost",
                    "cache_read_cost",
                    "cache_creation_cost",
                ],
            ),
            (
                "vendor_status",
                &[
                    "vendor",
                    "status",
                    "last_updated",
                    "window_used_tokens",
                    "window_reset_at",
                    "error_count",
                ],
            ),
            (
                "model_stats",
                &[
                    "id",
                    "model",
                    "vendor",
                    "date",
                    "request_count",
                    "success_count",
                    "error_count",
                    "avg_duration_ms",
                ],
            ),
        ];
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let sql = ddl(schema_sql_for_backend(backend));
            for (table, cols) in tables {
                let start = sql
                    .find(&format!("CREATE TABLE IF NOT EXISTS {table}"))
                    .unwrap_or_else(|| panic!("{backend:?}: 缺表 {table}"));
                let seg = &sql[start..];
                let seg = &seg[..seg.find(");").unwrap_or(seg.len())];
                for &col in cols {
                    assert!(
                        seg.contains(&format!("    {col} ")),
                        "{backend:?}/{table}: 建表段缺列 {col}"
                    );
                }
            }
        }
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
