// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::database::Repository;
use crate::error::NexusFlowError;

#[derive(Clone)]
pub struct Database {
    #[allow(dead_code)]
    pub pool: sqlx::sqlite::SqlitePool,
    pub repository: std::sync::Arc<Repository>,
}

impl Database {
    pub async fn new(path: &str, pool_size: u32) -> Result<Self, NexusFlowError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| NexusFlowError::Database {
                message: format!("创建数据库目录失败: {}", e),
            })?;
        }

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(pool_size)
            .connect(path)
            .await
            .map_err(|e| NexusFlowError::Database {
                message: format!("连接数据库失败: {}", e),
            })?;

        // 初始化 schema
        let schema = include_str!("schema.sql");
        for statement in schema.split(';') {
            let trimmed = statement.trim();
            if !trimmed.is_empty() {
                sqlx::query(trimmed).execute(&pool).await.map_err(|e| {
                    NexusFlowError::Database {
                        message: format!("初始化 schema 失败: {}", e),
                    }
                })?;
            }
        }

        let repository = std::sync::Arc::new(Repository::new(pool.clone()));

        Ok(Self { pool, repository })
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    #[allow(dead_code)]
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
