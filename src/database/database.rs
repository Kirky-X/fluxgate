// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

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
    pub async fn new(path: &str, pool_size: u32) -> Result<Self, NexusFlowError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| NexusFlowError::Database {
                    message: t_args("database-create-dir", &[("error", e.to_string())]),
                })?;
            }
        }

        let url = normalize_sqlite_url(path);

        let config = DbConfig {
            url,
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

/// 将原始路径规范化为 SeaORM/sqlx 接受的 SQLite URL。
/// 文件路径追加 `mode=rwc`：sqlx 默认不创建不存在的库文件，网关首次启动
/// 必须能自举建库。调用方已显式给出 mode 的 URL 原样保留。
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

#[cfg(test)]
mod tests {
    use super::normalize_sqlite_url;

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
