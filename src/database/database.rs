// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::Repository;
use crate::error::NexusFlowError;
use dbnexus::{DbConfig, DbPool};

#[derive(Clone)]
pub struct Database {
    #[allow(dead_code)]
    pub pool: DbPool,
    pub repository: std::sync::Arc<Repository>,
}

impl Database {
    pub async fn new(path: &str, pool_size: u32) -> Result<Self, NexusFlowError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| NexusFlowError::Database {
                    message: format!("创建数据库目录失败: {}", e),
                })?;
            }
        }

        let url = normalize_sqlite_url(path);

        let config = DbConfig {
            url,
            max_connections: pool_size,
            admin_role: "admin".to_string(),
            ..Default::default()
        };

        let pool = DbPool::with_config(config)
            .await
            .map_err(|e| NexusFlowError::Database {
                message: format!("连接数据库失败: {}", e),
            })?;

        // 初始化 schema
        let schema = include_str!("schema.sql");
        let session = pool.get_session("admin").await.map_err(|e| {
            NexusFlowError::Database {
                message: format!("获取数据库会话失败: {}", e),
            }
        })?;

        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                session.execute_raw_ddl(trimmed).await.map_err(|e| {
                    NexusFlowError::Database {
                        message: format!("初始化 schema 失败: {}", e),
                    }
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

    #[allow(dead_code)]
    pub async fn close(&self) {
        // DbPool 在 Drop 时自动清理，无需显式关闭
    }
}

/// 将原始路径规范化为 SeaORM 接受的 SQLite URL
fn normalize_sqlite_url(path: &str) -> String {
    if path.starts_with("sqlite:") {
        path.to_string()
    } else if path == ":memory:" {
        "sqlite::memory:".to_string()
    } else {
        format!("sqlite://{}", path)
    }
}
