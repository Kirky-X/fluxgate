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

        // 初始化 schema
        let schema = include_str!("schema.sql");
        let session = pool
            .get_session("admin")
            .await
            .map_err(|e| NexusFlowError::Database {
                message: t_args("database-session", &[("error", e.to_string())]),
            })?;

        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                session
                    .execute_raw_ddl(trimmed)
                    .await
                    .map_err(|e| NexusFlowError::Database {
                        message: t_args("database-init-schema", &[("error", e.to_string())]),
                    })?;
            }
        }

        drop(session);

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
/// （nexusflow 层 feature；llm-gate 零存储假设，不感知后端轴），未启用时
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
impl llm_gate::storage::RequestLogSink for Database {
    async fn log(
        &self,
        log: llm_gate::storage::RequestLog,
    ) -> Result<(), llm_gate::error::GateError> {
        self.repository()
            .log_request(&log)
            .await
            .map_err(|e| llm_gate::error::GateError::Config {
                message: format!("request log write failed: {e}"),
            })
    }

    async fn query_logs(
        &self,
        filter: &llm_gate::storage::LogFilter,
    ) -> Result<Vec<llm_gate::storage::RequestLog>, llm_gate::error::GateError> {
        self.repository()
            .query_logs(filter)
            .await
            .map_err(|e| llm_gate::error::GateError::Config {
                message: format!("request log query failed: {e}"),
            })
    }

    async fn search_logs(
        &self,
        keyword: &str,
        limit: u32,
    ) -> Result<Vec<llm_gate::storage::RequestLog>, llm_gate::error::GateError> {
        self.repository()
            .search_logs(keyword, limit)
            .await
            .map_err(|e| llm_gate::error::GateError::Config {
                message: format!("request log search failed: {e}"),
            })
    }
}

// 全部用例依赖 sqlite 回退分支（path 归一）——仅 db-sqlite 构建下编译执行；
// 非 db-sqlite 构建的 Err 分支由该 cfg 组合的编译检查覆盖（T008 门禁）
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
