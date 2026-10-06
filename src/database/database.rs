// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::config::DatabaseConfig;
use crate::database::Repository;
use crate::error::NexusFlowError;
use crate::i18n::t_args;
use dbnexus::{DbConfig, DbPool};

#[derive(Clone)]
pub struct Database {
    pub pool: DbPool,
    pub repository: std::sync::Arc<Repository>,
}

impl Database {
    /// 接收**解析后的完整数据库 URL**（postgres://、mysql://、sqlite://…）。
    /// 调用方经 [`resolve_database_url`] 从配置解析（bootstrap 单一出口）；
    /// sqlite 文件库的父目录自举仅对 sqlite 形态生效，其余后端无文件系统
    /// 语义直接跳过。
    pub async fn new(url: &str, pool_size: u32) -> Result<Self, NexusFlowError> {
        ensure_sqlite_parent_dir(url)?;

        let config = DbConfig {
            url: url.to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: pool_size,
                // rc.5 校验 min <= max；PoolConfig::default() 的 min=5 会
                // 在 pool_size < 5 时失败，显式给 1 与旧行为等价
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        };

        let pool = DbPool::with_config(config)
            .await
            .map_err(|e| NexusFlowError::Database {
                message: t_args("database-connect", &[("error", e.to_string())]),
            })?;

        // 初始化 schema（与 Repository::initialize 共享单一实现，按连接后端
        // 选择方言投影；MySQL 幂等建索引见 repository::init_schema）
        crate::database::repository::init_schema(&pool)
            .await
            .map_err(|e| NexusFlowError::Database {
                message: t_args("database-init-schema", &[("error", e.to_string())]),
            })?;

        let repository = std::sync::Arc::new(Repository::new(pool.clone()));

        Ok(Self { pool, repository })
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    pub async fn close(&self) {
        // DbPool 在 Drop 时自动清理，无需显式关闭
    }
}

/// sqlite 文件库父目录自举（`Database::new` 既有语义：库文件可落在尚不
/// 存在的目录）。仅 sqlite 文件形态有文件系统语义；从 URL 提取路径时剥掉
/// scheme 前缀与 query（mode=rwc 等）。
#[cfg(feature = "db-sqlite")]
fn ensure_sqlite_parent_dir(url: &str) -> Result<(), NexusFlowError> {
    if url.contains(":memory:") {
        return Ok(());
    }
    let Some(path) = url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    let path = path.split('?').next().unwrap_or(path);
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| NexusFlowError::Database {
                message: t_args("database-create-dir", &[("error", e.to_string())]),
            })?;
        }
    }
    Ok(())
}

/// 非 sqlite 构建无文件系统语义，恒成功。
#[cfg(not(feature = "db-sqlite"))]
fn ensure_sqlite_parent_dir(_url: &str) -> Result<(), NexusFlowError> {
    Ok(())
}

/// 解析数据库连接 URL（db-backend-selection D2：URL 单一事实源）。
///
/// `config.url` 非空时原样优先（postgres://、mysql://、sqlite://…）；
/// 否则回退 `path` 按 sqlite 文件库归一——该回退要求编译期启用 `db-sqlite`
/// （本 crate 的 feature；路由半边零存储假设，不感知后端轴），未启用时
/// 返回带重建指引的错误。
pub fn resolve_database_url(config: &DatabaseConfig) -> Result<String, NexusFlowError> {
    if !config.url.is_empty() {
        return Ok(config.url.clone());
    }
    #[cfg(not(feature = "db-sqlite"))]
    {
        Err(NexusFlowError::Database {
            message: t_args("database-backend-missing", &[("path", config.path.clone())]),
        })
    }
    #[cfg(feature = "db-sqlite")]
    {
        Ok(normalize_sqlite_url(&config.path))
    }
}

/// 数据库 URL 日志脱敏 — `config.url` 契约支持 postgres://、mysql:// 等
/// 惯例内嵌口令的形态（userinfo），原样进日志会把凭证泄进日志通道（脱敏
/// 先例同 sdk client 的 Debug 输出与 env 缺失告警）。剥除 userinfo
/// 并以 `[REDACTED]` 显式标注「此处有凭证」，host/port/path 保留供排障定位；
/// 无 userinfo 或非 scheme://host 形态（sqlite::memory:、裸路径）逐字节
/// 原样返回。解析用 url crate（IPv6 主机/默认端口等边界），输出手工拼装——
/// Url::to_string() 会把 userinfo 槽的 `[` `]` 百分号编码，日志不可读。
pub fn redact_db_url(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.username().is_empty() && parsed.password().is_none() {
        return url.to_string();
    }
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
    format!(
        "{}://[REDACTED]@{}{}{}{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or(""),
        port,
        parsed.path(),
        query,
    )
}

/// 将原始路径规范化为 SeaORM/sqlx 接受的 SQLite URL。
/// 文件路径追加 `mode=rwc`：sqlx 默认不创建不存在的库文件，网关首次启动
/// 必须能自举建库。调用方已显式给出 mode 的 URL 原样保留。
#[cfg(feature = "db-sqlite")]
fn normalize_sqlite_url(path: &str) -> String {
    if path.contains(":memory:") {
        // 内存库无文件可建，规范化为标准形式
        return "sqlite::memory:".to_string();
    }
    let url = if path.starts_with("sqlite:") {
        path.to_string()
    } else {
        format!("sqlite://{}", path)
    };
    if url.contains("mode=") {
        return url;
    }
    if url.contains('?') {
        format!("{url}&mode=rwc")
    } else {
        format!("{url}?mode=rwc")
    }
}

#[async_trait::async_trait]
impl crate::storage::RequestLogSink for Database {
    async fn log(&self, log: crate::storage::RequestLog) -> Result<(), crate::error::GateError> {
        self.repository()
            .log_request(&log)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("request log write failed: {e}"),
            })
    }

    async fn query_logs(
        &self,
        filter: &crate::storage::LogFilter,
    ) -> Result<Vec<crate::storage::RequestLog>, crate::error::GateError> {
        self.repository()
            .query_logs(filter)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("request log query failed: {e}"),
            })
    }

    async fn search_logs(
        &self,
        keyword: &str,
        limit: u32,
    ) -> Result<Vec<crate::storage::RequestLog>, crate::error::GateError> {
        self.repository()
            .search_logs(keyword, limit)
            .await
            .map_err(|e| crate::error::GateError::Config {
                message: format!("request log search failed: {e}"),
            })
    }
}

// 全部用例依赖 sqlite 回退分支（path 归一）——仅 db-sqlite 构建下编译执行；
// 非 db-sqlite 构建的 Err 分支由该 cfg 组合的编译检查覆盖（门禁）
#[cfg(all(test, feature = "db-sqlite"))]
mod tests {
    use super::normalize_sqlite_url;
    use super::resolve_database_url;
    use crate::config::DatabaseConfig;

    /// url 非空：原样优先，path 不参与（db-backend-selection D2）
    #[test]
    fn explicit_url_takes_precedence_over_path() {
        let config = DatabaseConfig {
            path: "./data/llm_proxy.db".to_string(),
            pool_size: 10,
            url: "postgres://db.internal:5432/nexusflow".to_string(),
        };
        assert_eq!(
            resolve_database_url(&config).expect("resolve ok"),
            "postgres://db.internal:5432/nexusflow"
        );
    }

    /// url 空：path 走 sqlite 归一（mode=rwc 自举建库语义不变）
    #[test]
    fn empty_url_falls_back_to_sqlite_path() {
        let config = DatabaseConfig {
            path: "./data/llm_proxy.db".to_string(),
            pool_size: 10,
            url: String::new(),
        };
        assert_eq!(
            resolve_database_url(&config).expect("resolve ok"),
            "sqlite://./data/llm_proxy.db?mode=rwc"
        );
    }

    /// url 空 + :memory:：规范化为标准内存库形式
    #[test]
    fn empty_url_memory_path_canonicalized() {
        let config = DatabaseConfig {
            path: ":memory:".to_string(),
            pool_size: 1,
            url: String::new(),
        };
        assert_eq!(
            resolve_database_url(&config).expect("resolve ok"),
            "sqlite::memory:"
        );
    }

    #[test]
    fn relative_path_gets_mode_rwc() {
        assert_eq!(
            normalize_sqlite_url("./data/llm_proxy.db"),
            "sqlite://./data/llm_proxy.db?mode=rwc"
        );
    }

    #[test]
    fn absolute_path_gets_mode_rwc() {
        assert_eq!(
            normalize_sqlite_url("/tmp/nx/llm_proxy.db"),
            "sqlite:///tmp/nx/llm_proxy.db?mode=rwc"
        );
    }

    #[test]
    fn memory_database_is_canonicalized_without_mode() {
        assert_eq!(normalize_sqlite_url(":memory:"), "sqlite::memory:");
        assert_eq!(normalize_sqlite_url("sqlite::memory:"), "sqlite::memory:");
    }

    #[test]
    fn explicit_sqlite_url_appends_mode() {
        assert_eq!(
            normalize_sqlite_url("sqlite:///tmp/a.db"),
            "sqlite:///tmp/a.db?mode=rwc"
        );
        assert_eq!(
            normalize_sqlite_url("sqlite:///tmp/a.db?cache=shared"),
            "sqlite:///tmp/a.db?cache=shared&mode=rwc"
        );
    }

    #[test]
    fn explicit_mode_is_respected() {
        assert_eq!(
            normalize_sqlite_url("sqlite:///tmp/a.db?mode=rw"),
            "sqlite:///tmp/a.db?mode=rw"
        );
    }
}

// 后端无关的纯字符串逻辑，不做 db-sqlite 门控——任意 db-* 组合下都必须
// 编译执行（弱凭证入日志通道与后端选择正交）
#[cfg(test)]
mod redact_db_url_tests {
    use super::redact_db_url;

    /// server-side URL 内嵌口令（DatabaseConfig.url 契约形态）→ userinfo
    /// 剥除并显式标注脱敏，host/port/path 原样保留（排障仍可定位实例）
    #[test]
    fn userinfo_credentials_are_stripped_with_marker() {
        assert_eq!(
            // 假夹具口令，非真实凭证：验证的正是脱敏行为本身
            redact_db_url("postgres://alice:s3cret@db.internal:5432/nexusflow"), // pragma: allowlist secret
            "postgres://[REDACTED]@db.internal:5432/nexusflow"
        );
    }

    /// 仅用户名无口令（口令常走连接参数/密管）同样剥除
    #[test]
    fn user_only_userinfo_is_stripped_too() {
        assert_eq!(
            redact_db_url("mysql://alice@db.internal/nexusflow"),
            "mysql://[REDACTED]@db.internal/nexusflow"
        );
    }

    /// 无 userinfo 的 URL 原样返回（逐字节不动，含 sqlite 归一产物）
    #[test]
    fn url_without_userinfo_is_untouched() {
        assert_eq!(
            redact_db_url("postgres://db.internal:5432/nexusflow"),
            "postgres://db.internal:5432/nexusflow"
        );
        assert_eq!(
            redact_db_url("sqlite://./data/llm_proxy.db?mode=rwc"),
            "sqlite://./data/llm_proxy.db?mode=rwc"
        );
    }

    /// 非 scheme://host 形态（内存库、裸路径）解析不了 userinfo，原样返回
    #[test]
    fn non_hierarchical_url_is_untouched() {
        assert_eq!(redact_db_url("sqlite::memory:"), "sqlite::memory:");
        assert_eq!(redact_db_url("./data/llm_proxy.db"), "./data/llm_proxy.db");
    }
}
