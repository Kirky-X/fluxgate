// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

use crate::database::{
    raw_statement, ContextStatRow, CostGroupBy, CostUsageRow, LogFilter, RequestLog, SessionRow,
    TokenUsageRow, VendorModelCostRow, VendorStatusRecord,
};
use crate::i18n::t_args;
use dbnexus::{DbError, DbPool, Session};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TransactionTrait,
    Value,
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
/// session 逐段获取逐段释放（测试池 max_connections=1）。
///
/// postgres/mysql 初始化全程持有跨进程迁移锁（见
/// [`crate::database::migration::acquire_migration_lock`]）：两个实例对同
/// 一库并发启动会同时读到空迁移历史并重放 V1（pg pg_type 23505 / mysql
/// 历史行 1062 必现冲突），MySQL 索引探针与 v0.1 列回填同为
/// check-then-create TOCTOU——建表迁移、索引补齐、列回填必须整体串行。
pub(crate) async fn init_schema(pool: &DbPool) -> Result<(), DbError> {
    let backend = {
        let session = pool.get_session("admin").await?;
        session.connection()?.get_database_backend()
    };
    let lock = crate::database::migration::acquire_migration_lock(pool, backend).await?;
    let result = async {
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
        Ok::<(), DbError>(())
    }
    .await;
    lock.release().await;
    result
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
    // idx_model 列携带前缀长度：V1 mysql 的 abstract_model 是 TEXT，MySQL 拒绝
    // 无长度键规格的 TEXT 索引（1170 BLOB/TEXT column used in key specification
    // without a key length）；255 字符 = 1020 字节，在 3072 字节上限内，与
    // sqlite/postgres 的全列索引同语义（非唯一索引，等值/前缀查找等价）。
    const INDEXES: [(&str, &str, &str); 11] = [
        ("idx_timestamp", "request_logs", "timestamp"),
        ("idx_vendor", "request_logs", "actual_vendor"),
        ("idx_model", "request_logs", "abstract_model(255)"),
        ("idx_request_id", "request_logs", "request_id"),
        // 复合索引只服务标题查询的 (session_key, timestamp) 等值定位。
        // 它**不**加速 sessions/context-stats 的聚合：9 个输出列里 7 个不在
        // 索引中，非覆盖索引 + 每行回表实测比全表扫更慢（优化器还会被带离
        // idx_timestamp）。聚合靠时间窗走 idx_timestamp。
        (
            "idx_request_logs_session",
            "request_logs",
            "session_key, timestamp",
        ),
        ("idx_token_usage_date", "token_usage", "date"),
        (
            "idx_token_usage_key_date",
            "token_usage",
            "api_key_hash, date",
        ),
        (
            "idx_response_store_tenant",
            "response_store",
            "tenant_hash, created_at",
        ),
        (
            "idx_response_store_conversation",
            "response_store",
            "conversation_id, created_at",
        ),
        ("idx_response_store_expires", "response_store", "expires_at"),
        (
            "idx_affinity_sticks_expires",
            "affinity_sticks",
            "expires_at",
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
    /// sqlite 的 PRAGMA table_info 返回键为 `name`；information_schema 两后端
    /// 统一 `AS column_name` 显式别名——postgres 对未加引号的标识符做小写折叠
    ///（裸 `COLUMN_NAME` 结果列名实为 `column_name`，按原名取值恒落空）。
    /// 语句与键随元组成对返回，调用方不再分支。
    fn column_probe(backend: DatabaseBackend, table: &str, column: &str) -> (String, &'static str) {
        match backend {
            DatabaseBackend::Sqlite => (format!("PRAGMA table_info({table})"), "name"),
            DatabaseBackend::MySql => (
                format!(
                    "SELECT COLUMN_NAME AS column_name FROM information_schema.columns \
                     WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = '{table}' AND COLUMN_NAME = '{column}'"
                ),
                "column_name",
            ),
            // DatabaseBackend 带 #[non_exhaustive]：其余服务端后端（含
            // postgres）共用 ANSI information_schema，按当前 schema 限定
            _ => (
                format!(
                    "SELECT COLUMN_NAME AS column_name FROM information_schema.columns \
                     WHERE TABLE_SCHEMA = current_schema() AND TABLE_NAME = '{table}' AND COLUMN_NAME = '{column}'"
                ),
                "column_name",
            ),
        }
    }

    pub async fn log_request(&self, log: &RequestLog) -> Result<(), DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let (sql, params) = Self::insert_log_parts(log);
        let stmt = raw_statement(conn.get_database_backend(), &sql, params);
        conn.execute_raw(stmt).await?;
        Ok(())
    }

    /// 流式占位行补写：按 request_id 回填 usage 响应体与 token 三列。
    ///
    /// 流式派发先落 `{"stream":true}` 占位行（usage 帧只在流中到达）；
    /// 调用方（AsyncLogger 通道内保序，先 flush 缓冲中的占位 INSERT）在
    /// 流终止点以本方法收尾。无匹配行（占位被 gate 拒发等）时静默成功——
    /// 日志面失败不失败请求。
    #[allow(clippy::too_many_arguments)]
    pub async fn update_stream_usage(
        &self,
        request_id: &str,
        response: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        cache_read_tokens: Option<i64>,
        cache_write_tokens: Option<i64>,
    ) -> Result<(), DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let sql = "UPDATE request_logs SET response = ?, prompt_tokens = ?, completion_tokens = ?, total_tokens = ?, cache_read_tokens = ?, cache_write_tokens = ? WHERE request_id = ?";
        let params: [sea_orm::Value; 7] = [
            response.into(),
            prompt_tokens.into(),
            completion_tokens.into(),
            total_tokens.into(),
            cache_read_tokens.into(),
            cache_write_tokens.into(),
            request_id.into(),
        ];
        let stmt = raw_statement(conn.get_database_backend(), sql, params);
        conn.execute_raw(stmt).await?;
        Ok(())
    }

    /// 单事务参数化批量写入：N 条 INSERT 包在一个事务里 commit（N 次 fsync
    /// 合为 1），参数绑定无手写转义（不用裸 SQL batch——用户内容拼接引入
    /// 注入面）；单条失败回滚整批。
    /// 事务取连接自身（`TransactionTrait`）：值走 sea-orm `Value` 带类型绑定
    /// ——dbnexus `execute_with_params` 的 JSON 映射把 null 绑成 TEXT，PG 下
    /// 数值列（cost_usd/token 计数）收到 TEXT 参数即拒（`expression is of
    /// type text`）。
    pub async fn log_requests_batch(&self, logs: &[RequestLog]) -> Result<(), DbError> {
        if logs.is_empty() {
            return Ok(());
        }
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let tx = conn.begin().await.map_err(DbError::from)?;
        let mut failed: Option<DbError> = None;
        for log in logs {
            let (sql, params) = Self::insert_log_parts(log);
            if let Err(e) = tx.execute_raw(raw_statement(backend, &sql, params)).await {
                failed = Some(DbError::from(e));
                break;
            }
        }
        match failed {
            // commit 失败同样尝试回滚，不残留开事务连接
            None => match tx.commit().await {
                Ok(()) => Ok(()),
                Err(e) => Err(DbError::from(e)),
            },
            Some(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// request_logs 参数化 INSERT（`log_request`/`log_requests_batch` 共用）。
    /// 占位符统一 `?`，执行前经 [`crate::database::dialect_placeholders`] 按
    /// backend 方言化（postgres 需 `$n`），调用方不再按后端分支。
    /// 参数为不透明 `Value`（带类型绑定）：sqlite/postgres/mysql 三端
    /// NULL 都按列类型落在正确位置。
    fn insert_log_parts(log: &RequestLog) -> (String, Vec<Value>) {
        const SQL: &str = r#"
            INSERT INTO request_logs (
                request_id, timestamp, endpoint, input_format,
                abstract_model, messages, actual_vendor, actual_model,
                routing_strategy, status_code, response, error_message,
                prompt_tokens, completion_tokens, total_tokens, duration_ms,
                cost_usd, session_key, context_window_tokens,
                cache_read_tokens, cache_write_tokens
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#;
        let params: Vec<Value> = vec![
            log.request_id.clone().into(),
            log.timestamp.into(),
            log.endpoint.clone().into(),
            log.input_format.clone().into(),
            log.abstract_model.clone().into(),
            log.messages.clone().into(),
            log.actual_vendor.clone().into(),
            log.actual_model.clone().into(),
            log.routing_strategy.clone().into(),
            log.status_code.into(),
            log.response.clone().into(),
            log.error_message.clone().into(),
            log.prompt_tokens.into(),
            log.completion_tokens.into(),
            log.total_tokens.into(),
            log.duration_ms.into(),
            log.cost_usd.into(),
            log.session_key.clone().into(),
            log.context_window_tokens.into(),
            log.cache_read_tokens.into(),
            log.cache_write_tokens.into(),
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
        let stmt = raw_statement(conn.get_database_backend(), &query, values);
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

        let stmt = raw_statement(backend, &sql, values);
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
        let stmt = raw_statement(conn.get_database_backend(), &sql, values);
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
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let mut sql = format!(
            "SELECT {dimension} AS dimension, \
             {request_count}, {input_tokens}, \
             {output_tokens}, {cache_read_tokens}, \
             {cache_creation_tokens}, SUM(input_cost) AS input_cost, \
             SUM(output_cost) AS output_cost, SUM(cache_read_cost) AS cache_read_cost, \
             SUM(cache_creation_cost) AS cache_creation_cost \
             FROM token_usage",
            request_count = int_sum_expr(backend, "request_count"),
            input_tokens = int_sum_expr(backend, "input_tokens"),
            output_tokens = int_sum_expr(backend, "output_tokens"),
            cache_read_tokens = int_sum_expr(backend, "cache_read_tokens"),
            cache_creation_tokens = int_sum_expr(backend, "cache_creation_tokens"),
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

        let stmt = raw_statement(backend, &sql, values);
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

    /// 会话聚合（`GET /admin/sessions`）：按 `session_key` 分组，
    /// NULL 会话排除（无归属语义）。标题取该会话首条 `messages` 前 80 字符。
    /// 聚合只统计 `since_ts` 之后的行：`request_logs` 无保留策略，全史聚合是
    /// 线性全表扫（百万行级秒级）；时间窗让查询走 `idx_timestamp`。
    pub async fn query_sessions(
        &self,
        since_ts: i64,
        limit: u32,
    ) -> Result<Vec<SessionRow>, DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = format!(
            "SELECT session_key, \
             {cnt} AS request_count, \
             {p} AS prompt_tokens, {c} AS completion_tokens, \
             {cr} AS cache_read_tokens, {cw} AS cache_write_tokens, \
             SUM(COALESCE(cost_usd, 0)) AS total_cost, \
             MIN(timestamp) AS first_at, MAX(timestamp) AS last_at \
             FROM request_logs WHERE session_key IS NOT NULL AND status_code = 200 \
             AND timestamp >= ? \
             GROUP BY session_key ORDER BY last_at DESC LIMIT ?",
            cnt = count_expr(backend),
            p = sum_expr(backend, "prompt_tokens"),
            c = sum_expr(backend, "completion_tokens"),
            cr = sum_expr(backend, "cache_read_tokens"),
            cw = sum_expr(backend, "cache_write_tokens"),
        );
        let stmt = raw_statement(
            backend,
            &sql,
            vec![since_ts.into(), i64::from(limit).into()],
        );
        let rows = conn.query_all_raw(stmt).await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            let session_key: String = row.try_get("", "session_key").unwrap_or_default();
            let first_at: i64 = row.try_get("", "first_at").unwrap_or_default();
            // 标题：该会话首条消息摘要（会话起点那一行）
            // 标题行 = 聚合已算出的 first_at 那一行，按 (session_key, timestamp)
            // 精确命中——走复合索引，无需临时排序；错误上浮而非静默置空。
            let title_stmt = raw_statement(
                backend,
                // 不截断 messages：JSON 被切开后无法解析，`session_title` 会
                // 退化成原始 JSON 前缀（含 system 提示词），比传输开销更糟。
                // ORDER BY id 消歧：timestamp 是秒级，同秒多请求时否则取到
                // 哪条由存储层决定。
                "SELECT messages FROM request_logs \
                 WHERE session_key = ? AND timestamp = ? ORDER BY id ASC LIMIT 1",
                vec![session_key.clone().into(), first_at.into()],
            );
            let title = conn
                .query_all_raw(title_stmt)
                .await?
                .first()
                .and_then(|r| r.try_get::<Option<String>>("", "messages").ok())
                .flatten()
                .map(|m| session_title(&m))
                .unwrap_or_default();
            out.push(SessionRow {
                session_key,
                request_count: row.try_get("", "request_count").unwrap_or_default(),
                prompt_tokens: row.try_get("", "prompt_tokens").unwrap_or_default(),
                completion_tokens: row.try_get("", "completion_tokens").unwrap_or_default(),
                cache_read_tokens: row.try_get("", "cache_read_tokens").unwrap_or_default(),
                cache_write_tokens: row.try_get("", "cache_write_tokens").unwrap_or_default(),
                total_cost: row.try_get("", "total_cost").unwrap_or_default(),
                first_at,
                last_at: row.try_get("", "last_at").unwrap_or_default(),
                title,
            });
        }
        Ok(out)
    }

    /// 上下文利用率分桶（`GET /admin/context-stats`）：按模型聚合。
    /// 占用率分母为 `context_window_tokens`，分子为输入侧 token
    ///（prompt + cache_read + cache_write，均为占窗的输入部分）。
    /// 窗口未登记的模型行不参与（分母缺失）。
    pub async fn query_context_stats(&self, since_ts: i64) -> Result<Vec<ContextStatRow>, DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        // 子查询先投影出 input_tokens / window_tokens，外层桶表达式只做比较
        // （原实现把 COALESCE 组合按桶内联 9 次，实测 1M 行多付 42%）。
        let sql = format!(
            "SELECT model, \
             {cnt} AS request_count, \
             {inp} AS input_tokens, \
             {cr} AS cache_read_tokens, \
             MAX(window_tokens) AS window_tokens, \
             {b0} AS b0, {b1} AS b1, {b2} AS b2, {b3} AS b3, {b4} AS b4 \
             FROM ( \
                 SELECT actual_model AS model, \
                        {INPUT_TOKENS_EXPR} AS input_tokens, \
                        COALESCE(cache_read_tokens, 0) AS cache_read_tokens, \
                        context_window_tokens AS window_tokens \
                 FROM request_logs \
                 WHERE context_window_tokens IS NOT NULL AND status_code = 200 \
                   AND timestamp >= ? \
             ) AS base \
             GROUP BY model ORDER BY model",
            cnt = count_expr(backend),
            inp = sum_expr(backend, "input_tokens"),
            cr = sum_expr(backend, "cache_read_tokens"),
            b0 = bucket_sum_expr(backend, 0),
            b1 = bucket_sum_expr(backend, 1),
            b2 = bucket_sum_expr(backend, 2),
            b3 = bucket_sum_expr(backend, 3),
            b4 = bucket_sum_expr(backend, 4),
        );
        let stmt = raw_statement(backend, &sql, vec![since_ts.into()]);
        let rows = conn.query_all_raw(stmt).await?;
        Ok(rows
            .iter()
            .map(|row| {
                let request_count: i64 = row.try_get("", "request_count").unwrap_or_default();
                let input_tokens: i64 = row.try_get("", "input_tokens").unwrap_or_default();
                let cache_read: i64 = row.try_get("", "cache_read_tokens").unwrap_or_default();
                let window: i64 = row.try_get("", "window_tokens").unwrap_or(1);
                let denom = request_count.max(1) as f64;
                ContextStatRow {
                    model: row.try_get("", "model").unwrap_or_default(),
                    request_count,
                    avg_occupancy: if window > 0 {
                        (input_tokens as f64 / denom) / window as f64
                    } else {
                        0.0
                    },
                    cache_read_share: if input_tokens > 0 {
                        cache_read as f64 / input_tokens as f64
                    } else {
                        0.0
                    },
                    bucket_counts: [
                        row.try_get("", "b0").unwrap_or_default(),
                        row.try_get("", "b1").unwrap_or_default(),
                        row.try_get("", "b2").unwrap_or_default(),
                        row.try_get("", "b3").unwrap_or_default(),
                        row.try_get("", "b4").unwrap_or_default(),
                    ],
                }
            })
            .collect())
    }

    /// 会话粘性批量 UPSERT（`affinity_sticks`）：复合主键
    /// (tenant, session_key) 冲突时覆盖 vendor_id / worth_tokens / expires_at。
    /// 空切片短路（无 SQL 往返）；五列全部参数化绑定。
    pub async fn upsert_affinity_sticks(
        &self,
        sticks: &[crate::storage::AffinityStick],
    ) -> Result<(), DbError> {
        if sticks.is_empty() {
            return Ok(());
        }
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::affinity_persist::upsert_affinity_sticks_sql(backend);
        for stick in sticks {
            let values = vec![
                Value::from(stick.tenant.clone()),
                Value::from(stick.session_key.clone()),
                Value::from(stick.vendor_id.clone()),
                Value::from(stick.worth_tokens),
                Value::from(stick.expires_at),
            ];
            let stmt = raw_statement(backend, sql, values);
            conn.execute_raw(stmt).await?;
        }
        Ok(())
    }

    /// 未过期粘性读取（启动恢复）：按 `expires_at` 降序取前 `limit` 条
    ///（最新写入优先）；`LIMIT` 下推到 SQL，不把全表拉进内存。
    pub async fn read_affinity_sticks(
        &self,
        now: i64,
        limit: u32,
    ) -> Result<Vec<crate::storage::AffinityStick>, DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::affinity_persist::read_unexpired_sql(backend);
        let stmt = raw_statement(backend, sql, vec![now.into(), i64::from(limit).into()]);
        let rows = conn.query_all_raw(stmt).await?;
        Ok(rows
            .iter()
            .map(|row| crate::storage::AffinityStick {
                tenant: row.try_get("", "tenant").unwrap_or_default(),
                session_key: row.try_get("", "session_key").unwrap_or_default(),
                vendor_id: row.try_get("", "vendor_id").unwrap_or_default(),
                worth_tokens: row.try_get("", "worth_tokens").unwrap_or_default(),
                expires_at: row.try_get("", "expires_at").unwrap_or_default(),
            })
            .collect())
    }

    /// 过期粘性清理（运维/测试）：删除 `expires_at <= now` 的行，返回删除数。
    pub async fn purge_expired_affinity_sticks(&self, now: i64) -> Result<u64, DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::affinity_persist::purge_expired_sql(backend);
        let stmt = raw_statement(backend, sql, vec![now.into()]);
        let result = conn.execute_raw(stmt).await?;
        Ok(result.rows_affected())
    }

    /// quota_ledger 批量 UPSERT（quota-aware-routing V7）：复合主键
    /// (api_key_hash, window_id, window_start) 冲突时覆盖用量与 updated_at。
    /// 空切片短路（无 SQL 往返）；六列全部参数化绑定；整批包单事务 commit
    ///（N 次自动提交 fsync 合为 1，与 [`Self::log_requests_batch`] 同模式，
    /// 单条失败回滚整批不留半写批次）。
    pub async fn upsert_quota_ledger(
        &self,
        rows: &[crate::database::quota_ledger::QuotaLedgerRow],
    ) -> Result<(), DbError> {
        use crate::database::quota_ledger::QuotaLedgerRow;
        if rows.is_empty() {
            return Ok(());
        }
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::quota_ledger::upsert_quota_ledger_sql(backend);
        let tx = conn.begin().await?;
        for QuotaLedgerRow {
            api_key_hash,
            window_id,
            window_start,
            tokens_used,
            cost_used_millicents,
            updated_at,
        } in rows
        {
            let values = vec![
                Value::from(api_key_hash.clone()),
                Value::from(window_id.clone()),
                Value::from(*window_start),
                Value::from(*tokens_used),
                Value::from(*cost_used_millicents),
                Value::from(*updated_at),
            ];
            let stmt = raw_statement(backend, sql, values);
            if let Err(e) = tx.execute_raw(stmt).await {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// quota_ledger 保留清理：删除 `window_start` 早于 `not_before` 下界的
    /// 过期周期行，返回删除数。下界由
    /// [`crate::middleware::quota::ledger_retention_secs`] 按配置最长窗推导
    ///（宁多留一行不误删活动行），与启动恢复读侧同参单源。
    pub async fn purge_expired_quota_ledger(&self, not_before: i64) -> Result<u64, DbError> {
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::quota_ledger::purge_expired_quota_ledger_sql(backend);
        let stmt = raw_statement(backend, sql, vec![not_before.into()]);
        let result = conn.execute_raw(stmt).await?;
        Ok(result.rows_affected())
    }

    /// quota_ledger 读取（启动恢复读侧）：`not_before` 为 `window_start`
    /// 下界（不可能仍活动的历史周期行 SQL 层过滤，不拉进内存）；列映射
    /// 错误显性上浮——列类型不符静默归零会让恢复账面失真，属必须暴露的
    /// 读取故障。活动期精判在内存侧按配置周期解释（账本不存周期事实），
    /// 见 [`crate::database::quota_ledger::read_quota_ledger_sql`]。
    pub async fn read_quota_ledger(
        &self,
        not_before: i64,
    ) -> Result<Vec<crate::database::quota_ledger::QuotaLedgerRow>, DbError> {
        use crate::database::quota_ledger::QuotaLedgerRow;
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let backend = conn.get_database_backend();
        let sql = crate::database::quota_ledger::read_quota_ledger_sql(backend);
        let stmt = raw_statement(backend, sql, vec![not_before.into()]);
        let rows = conn.query_all_raw(stmt).await?;
        rows.iter()
            .map(|row| {
                Ok(QuotaLedgerRow {
                    api_key_hash: row.try_get("", "api_key_hash")?,
                    window_id: row.try_get("", "window_id")?,
                    window_start: row.try_get("", "window_start")?,
                    tokens_used: row.try_get("", "tokens_used")?,
                    cost_used_millicents: row.try_get("", "cost_used_millicents")?,
                    updated_at: row.try_get("", "updated_at")?,
                })
            })
            .collect()
    }

    /// CostTracker 启动恢复聚合：按 `(vendor, model)` 分组对四档成本求和，
    /// 窗口 = 最近 `window_hours` 小时（date/hour 联合下界；cutoff 由本地
    /// 时钟计算后参数化，规避跨后端时间函数方言差）。空窗口返回空集。
    pub async fn query_cost_window(
        &self,
        window_hours: u64,
    ) -> Result<Vec<VendorModelCostRow>, DbError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(window_hours as i64);
        let cutoff_date = cutoff.format("%Y-%m-%d").to_string();
        let cutoff_hour: i32 = cutoff.format("%H").to_string().parse().unwrap_or(0);
        let sql = "SELECT vendor, model, \
             SUM(input_cost) + SUM(output_cost) + SUM(cache_read_cost) + SUM(cache_creation_cost) \
             AS total_cost \
             FROM token_usage \
             WHERE date > ? OR (date = ? AND hour >= ?) \
             GROUP BY vendor, model ORDER BY vendor, model";
        let session = self.pool.get_session("admin").await?;
        let conn = session.connection()?;
        let stmt = raw_statement(
            conn.get_database_backend(),
            sql,
            vec![
                cutoff_date.clone().into(),
                cutoff_date.into(),
                cutoff_hour.into(),
            ],
        );
        let rows = conn.query_all_raw(stmt).await?;

        Ok(rows
            .iter()
            .map(|row| VendorModelCostRow {
                vendor: row.try_get("", "vendor").unwrap_or_default(),
                model: row.try_get("", "model").unwrap_or_default(),
                total_cost: row.try_get("", "total_cost").unwrap_or_default(),
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

/// 整数聚合列投影（纯函数）：`SUM(bigint)` 在 postgres 是 `numeric`、在
/// mysql 是 `decimal`，与 i64 解码不兼容（`unwrap_or_default` 静默归 0），
/// 故按后端显式 CAST 回整数类型；sqlite 的 SUM 保持整数类型，原样即可。
/// 会话标题：从请求日志的 `messages` 摘要提取首条用户消息前 80 字符。
/// `messages` 可能是 JSON 数组（openai/anthropic 形状）或截断后的非
/// JSON 文本；两种形态都要产出可读标题，不能因解析失败留空。
fn session_title(messages: &str) -> String {
    const LIMIT: usize = 80;
    let text = serde_json::from_str::<serde_json::Value>(messages)
        .ok()
        .and_then(|value| {
            let array = value.as_array()?;
            // 只取 user 消息：system 消息常含运营方指令/路由提示，
            // 作为会话标题外露会泄漏内部配置。
            array
                .iter()
                .filter(|item| item.get("role").and_then(|r| r.as_str()) == Some("user"))
                .find_map(|item| {
                    let content = item.get("content")?;
                    if let Some(text) = content.as_str() {
                        return Some(text.to_string());
                    }
                    content.as_array().and_then(|blocks| {
                        blocks.iter().find_map(|block| {
                            block
                                .get("text")
                                .and_then(|t| t.as_str())
                                .map(str::to_string)
                        })
                    })
                })
        })
        .unwrap_or_else(|| messages.to_string());
    let trimmed: String = text.chars().take(LIMIT).collect();
    trimmed.replace('\n', " ").trim().to_string()
}

/// 输入侧 token 表达式（占窗部分）。
///
/// 口径约束：`request_logs` 只存 `actual_vendor`（id），不存协议类型，查询侧
/// 无法区分 Inclusive（OpenAI/Gemini：`prompt_tokens` 已含 `cache_read`）与
/// Exclusive（Anthropic：`prompt_tokens` 不含）。为让两种语义都**不重复
/// 计数**，分子取 `prompt + cache_write`：
/// - Inclusive：`prompt` 已含 `cache_read`，再加 `cache_write` 即真实入窗量；
/// - Exclusive：少计 `cache_read`，故该协议的占用率是**下界**（已文档披露）。
///
/// 选择不重复计数而非上界：重复计数会让占用率虚高、分桶误报；下界只是
/// 保守。`cache_read_tokens` 列仍保留上游原值，`cache_read_share` 不受影响。
const INPUT_TOKENS_EXPR: &str = "(COALESCE(prompt_tokens, 0) + COALESCE(cache_write_tokens, 0))";

/// 桶表达式用到的预投影列名（子查询内已算好，外层只比较）。
const BUCKET_INPUT_COL: &str = "input_tokens";
const BUCKET_WINDOW_COL: &str = "window_tokens";

/// 五分位桶求和（`idx` 0..4 对应 <20% / 20-40% / 40-60% / 60-80% / >=80%）。
/// PG/MySQL 的 `SUM(CASE ...)` 返回 numeric/decimal，必须 CAST 回整数，
/// 否则按 i64 解码失败会被静默吞成 0（见 `int_sum_expr` 的注释）。
fn bucket_sum_expr(backend: DatabaseBackend, idx: u8) -> String {
    // 桶 i 覆盖 [i*20%, (i+1)*20%)：以 `expr*5` 与 `window*(i+1)` 比较。
    let lower = if idx == 0 {
        String::new()
    } else {
        format!("{BUCKET_INPUT_COL} * 5 >= {BUCKET_WINDOW_COL} * {idx}")
    };
    let upper = if idx == 4 {
        String::new()
    } else {
        format!("{BUCKET_INPUT_COL} * 5 < {BUCKET_WINDOW_COL} * {}", idx + 1)
    };
    let cond = match (lower.is_empty(), upper.is_empty()) {
        (true, false) => upper,
        (false, true) => lower,
        (false, false) => format!("{lower} AND {upper}"),
        (true, true) => unreachable!("bucket 0 与 4 各有一侧条件"),
    };
    format!(
        "CAST(SUM(CASE WHEN {cond} THEN 1 ELSE 0 END) AS {})",
        match backend {
            DatabaseBackend::Postgres => "BIGINT",
            DatabaseBackend::MySql => "SIGNED",
            _ => "INTEGER",
        }
    )
}

/// 计数表达式：MySQL 的 `COUNT(*)` 返回 `BIGINT UNSIGNED`，与 `i64` 解码
/// 不兼容（sqlx 的 int 兼容检查要求非 UNSIGNED），须 CAST 成 SIGNED。
fn count_expr(backend: DatabaseBackend) -> String {
    match backend {
        DatabaseBackend::MySql => "CAST(COUNT(*) AS SIGNED)".to_string(),
        _ => "COUNT(*)".to_string(),
    }
}

/// 求和表达式（不含别名，供模板自行拼 `AS`）：PG/MySQL 的 SUM 返回
/// numeric/decimal，需 CAST 回整数；sqlite 原生整数。
fn sum_expr(backend: DatabaseBackend, column: &str) -> String {
    match backend {
        DatabaseBackend::Postgres => format!("CAST(SUM({column}) AS BIGINT)"),
        DatabaseBackend::MySql => format!("CAST(SUM({column}) AS SIGNED)"),
        _ => format!("SUM({column})"),
    }
}

#[cfg(test)]
mod title_tests {
    use super::session_title;

    #[test]
    fn session_title_prefers_first_user_message() {
        let messages = r#"[{"role":"system","content":"SECRET SYSTEM PROMPT"},{"role":"user","content":"hello there"}]"#;
        assert_eq!(session_title(messages), "hello there", "必须跳过 system");
    }

    /// 超长消息截 80 字符（按字符而非字节）。
    #[test]
    fn session_title_truncates_to_80_chars() {
        let long = "字".repeat(200);
        let messages = format!(r#"[{{"role":"user","content":"{long}"}}]"#);
        assert_eq!(session_title(&messages).chars().count(), 80);
    }

    /// 非 JSON（被截断的旧行）走原文回退，不 panic。
    #[test]
    fn session_title_falls_back_to_raw_text() {
        assert_eq!(session_title("plain text prefix"), "plain text prefix");
    }

    /// 多模态内容取首个 text 块。
    #[test]
    fn session_title_reads_first_text_block() {
        let messages = r#"[{"role":"user","content":[{"type":"image"},{"type":"text","text":"describe this"}]}]"#;
        assert_eq!(session_title(messages), "describe this");
    }
}

fn int_sum_expr(backend: DatabaseBackend, column: &str) -> String {
    match backend {
        DatabaseBackend::Postgres => format!("CAST(SUM({column}) AS BIGINT) AS {column}"),
        DatabaseBackend::MySql => format!("CAST(SUM({column}) AS SIGNED) AS {column}"),
        _ => format!("SUM({column}) AS {column}"),
    }
}

/// 整数列读取为 i32（纯函数）：postgres 侧这些列自 V1 起即 BIGINT，
/// 按 i32 直读会解码失败而静默落成缺省值（NULL/0）；统一经 i64 读再收窄
///（sqlite/mysql 的 i64 读法兼容）。越界值按缺省处理（计数/状态码量级内）。
fn read_i32(row: &QueryResult, column: &str) -> Option<i32> {
    row.try_get::<Option<i64>>("", column)
        .ok()
        .flatten()
        .and_then(|v| i32::try_from(v).ok())
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
        status_code: read_i32(row, "status_code"),
        response: row
            .try_get::<Option<String>>("", "response")
            .unwrap_or(None),
        error_message: row
            .try_get::<Option<String>>("", "error_message")
            .unwrap_or(None),
        prompt_tokens: read_i32(row, "prompt_tokens"),
        completion_tokens: read_i32(row, "completion_tokens"),
        total_tokens: read_i32(row, "total_tokens"),
        duration_ms: row
            .try_get::<Option<i64>>("", "duration_ms")
            .unwrap_or(None),
        cost_usd: row.try_get::<Option<f64>>("", "cost_usd").unwrap_or(None),
        session_key: row
            .try_get::<Option<String>>("", "session_key")
            .unwrap_or(None),
        context_window_tokens: row
            .try_get::<Option<i64>>("", "context_window_tokens")
            .unwrap_or(None),
        cache_read_tokens: row
            .try_get::<Option<i64>>("", "cache_read_tokens")
            .unwrap_or(None),
        cache_write_tokens: row
            .try_get::<Option<i64>>("", "cache_write_tokens")
            .unwrap_or(None),
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
        error_count: read_i32(row, "error_count").unwrap_or_default(),
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
                    "session_key",
                    "context_window_tokens",
                    "cache_read_tokens",
                    "cache_write_tokens",
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

    /// 列探针语句分派：语句与结果键成对返回（R-schema-002）。
    /// information_schema 两后端统一 `AS column_name` 显式别名——postgres 对
    /// 未加引号标识符做小写折叠，裸 `COLUMN_NAME` 的结果列名实为
    /// `column_name`，按原名取值恒落空（探针恒判缺失 → 重复 ALTER 报
    /// "column already exists"）；别名后结果键三后端均为小写口径。
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
        assert!(sql.contains("COLUMN_NAME AS column_name"), "{sql}");
        assert_eq!(key, "column_name");

        let (sql, key) =
            Repository::column_probe(DatabaseBackend::Postgres, "request_logs", "cost_usd");
        assert!(sql.contains("TABLE_SCHEMA = current_schema()"));
        assert!(sql.contains("COLUMN_NAME AS column_name"), "{sql}");
        assert_eq!(key, "column_name");
    }

    /// 整数聚合列投影分派：postgres 的 `SUM(bigint)` 是 numeric、mysql 是
    /// decimal，按 i64 解码静默归 0，故两后端显式 CAST；sqlite 的 SUM 保持
    /// 整数类型（R-schema-002 的读回口径）。
    #[test]
    fn int_sum_expr_casts_on_server_side_backends() {
        use super::int_sum_expr;

        assert_eq!(
            int_sum_expr(DatabaseBackend::Postgres, "request_count"),
            "CAST(SUM(request_count) AS BIGINT) AS request_count"
        );
        assert_eq!(
            int_sum_expr(DatabaseBackend::MySql, "input_tokens"),
            "CAST(SUM(input_tokens) AS SIGNED) AS input_tokens"
        );
        assert_eq!(
            int_sum_expr(DatabaseBackend::Sqlite, "output_tokens"),
            "SUM(output_tokens) AS output_tokens"
        );
    }
}
