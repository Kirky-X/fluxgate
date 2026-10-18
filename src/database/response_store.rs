// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! Responses 有状态表仓储实现（`response_store`，迁移 V4）。
//!
//! 所有查询强制携带 `tenant_hash` 与 `now`（过期行视为不存在）；
//! 占位符统一书 `?`，postgres 方言化由 [`raw_statement`] 收口。

use crate::database::{raw_statement, Repository};
use crate::error::GateError;
use crate::storage::{ResponseStoreSink, StoredResponse};
use sea_orm::{ConnectionTrait, QueryResult, Value};

fn db_error(error: impl std::fmt::Display) -> GateError {
    GateError::Config {
        message: format!("response_store: {error}"),
    }
}

fn map_stored(row: &QueryResult) -> StoredResponse {
    StoredResponse {
        id: row.try_get("", "id").unwrap_or_default(),
        tenant_hash: row.try_get("", "tenant_hash").unwrap_or_default(),
        conversation_id: row.try_get("", "conversation_id").unwrap_or(None),
        parent_response_id: row.try_get("", "parent_response_id").unwrap_or(None),
        model: row.try_get("", "model").unwrap_or_default(),
        status: row.try_get("", "status").unwrap_or_default(),
        store: row
            .try_get::<i64>("", "store_flag")
            .map(|value| value != 0)
            .unwrap_or(true),
        background: row
            .try_get::<i64>("", "background")
            .map(|value| value != 0)
            .unwrap_or(false),
        created_at: row.try_get("", "created_at").unwrap_or_default(),
        expires_at: row.try_get("", "expires_at").unwrap_or_default(),
        input_items: row.try_get("", "input_items").unwrap_or_default(),
        output_items: row.try_get("", "output_items").unwrap_or_default(),
        response_json: row.try_get("", "response_json").unwrap_or_default(),
    }
}

#[async_trait::async_trait]
impl ResponseStoreSink for Repository {
    async fn insert_response(&self, row: &StoredResponse) -> Result<(), GateError> {
        const SQL: &str = "INSERT INTO response_store (\
            id, tenant_hash, conversation_id, parent_response_id, model, status, \
            store_flag, background, created_at, expires_at, input_items, output_items, response_json\
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(
            conn.get_database_backend(),
            SQL,
            vec![
                Value::from(row.id.clone()),
                Value::from(row.tenant_hash.clone()),
                Value::from(row.conversation_id.clone()),
                Value::from(row.parent_response_id.clone()),
                Value::from(row.model.clone()),
                Value::from(row.status.clone()),
                Value::from(if row.store { 1_i64 } else { 0_i64 }),
                Value::from(if row.background { 1_i64 } else { 0_i64 }),
                Value::from(row.created_at),
                Value::from(row.expires_at),
                Value::from(row.input_items.clone()),
                Value::from(row.output_items.clone()),
                Value::from(row.response_json.clone()),
            ],
        );
        conn.execute_raw(statement).await.map_err(db_error)?;
        Ok(())
    }

    async fn get_response(
        &self,
        id: &str,
        tenant_hash: &str,
        now: i64,
    ) -> Result<Option<StoredResponse>, GateError> {
        const SQL: &str =
            "SELECT * FROM response_store WHERE id = ? AND tenant_hash = ? AND expires_at > ?";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(
            conn.get_database_backend(),
            SQL,
            vec![
                Value::from(id.to_string()),
                Value::from(tenant_hash.to_string()),
                Value::from(now),
            ],
        );
        let rows = conn.query_all_raw(statement).await.map_err(db_error)?;
        Ok(rows.first().map(map_stored))
    }

    async fn delete_response(
        &self,
        id: &str,
        tenant_hash: &str,
        now: i64,
    ) -> Result<bool, GateError> {
        const SQL: &str =
            "DELETE FROM response_store WHERE id = ? AND tenant_hash = ? AND expires_at > ?";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(
            conn.get_database_backend(),
            SQL,
            vec![
                Value::from(id.to_string()),
                Value::from(tenant_hash.to_string()),
                Value::from(now),
            ],
        );
        let result = conn.execute_raw(statement).await.map_err(db_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_conversation(
        &self,
        conversation_id: &str,
        tenant_hash: &str,
        now: i64,
    ) -> Result<Vec<StoredResponse>, GateError> {
        const SQL: &str = "SELECT * FROM response_store \
             WHERE conversation_id = ? AND tenant_hash = ? AND expires_at > ? \
             ORDER BY created_at, id";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(
            conn.get_database_backend(),
            SQL,
            vec![
                Value::from(conversation_id.to_string()),
                Value::from(tenant_hash.to_string()),
                Value::from(now),
            ],
        );
        let rows = conn.query_all_raw(statement).await.map_err(db_error)?;
        Ok(rows.iter().map(map_stored).collect())
    }

    async fn set_response_status(
        &self,
        id: &str,
        tenant_hash: &str,
        status: &str,
        response_json: &str,
    ) -> Result<(), GateError> {
        const SQL: &str =
            "UPDATE response_store SET status = ?, response_json = ? WHERE id = ? AND tenant_hash = ?";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(
            conn.get_database_backend(),
            SQL,
            vec![
                Value::from(status.to_string()),
                Value::from(response_json.to_string()),
                Value::from(id.to_string()),
                Value::from(tenant_hash.to_string()),
            ],
        );
        conn.execute_raw(statement).await.map_err(db_error)?;
        Ok(())
    }

    async fn purge_expired_responses(&self, now: i64, limit: u32) -> Result<u64, GateError> {
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let backend = conn.get_database_backend();
        const SELECT: &str = "SELECT id FROM response_store WHERE expires_at < ? LIMIT ?";
        let statement = raw_statement(
            backend,
            SELECT,
            vec![Value::from(now), Value::from(i64::from(limit))],
        );
        let rows = conn.query_all_raw(statement).await.map_err(db_error)?;
        let ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<String>("", "id").ok())
            .collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!("DELETE FROM response_store WHERE id IN ({placeholders})");
        let values: Vec<Value> = ids.into_iter().map(Value::from).collect();
        let statement = raw_statement(backend, sql, values);
        let result = conn.execute_raw(statement).await.map_err(db_error)?;
        Ok(result.rows_affected())
    }

    async fn mark_orphaned_responses_failed(&self) -> Result<u64, GateError> {
        const SQL: &str = "UPDATE response_store SET status = 'failed' \
             WHERE status IN ('queued', 'in_progress')";
        let session = self.pool.get_session("admin").await.map_err(db_error)?;
        let conn = session.connection().map_err(db_error)?;
        let statement = raw_statement(conn.get_database_backend(), SQL, Vec::<Value>::new());
        let result = conn.execute_raw(statement).await.map_err(db_error)?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Repository;
    use dbnexus::{DbConfig, DbPool};

    async fn repository() -> Repository {
        let config = DbConfig {
            url: "sqlite::memory:".to_string(),
            pool_config: dbnexus::PoolConfig {
                max_connections: 1,
                min_connections: 1,
                ..Default::default()
            },
            admin_role: "admin".to_string(),
            ..Default::default()
        };
        let pool = DbPool::with_config(config).await.expect("内存池");
        let repository = Repository::new(pool);
        repository.initialize().await.expect("迁移");
        repository
    }

    fn row(id: &str, tenant: &str, created_at: i64) -> StoredResponse {
        StoredResponse {
            id: id.to_string(),
            tenant_hash: tenant.to_string(),
            conversation_id: Some("conv_1".to_string()),
            parent_response_id: None,
            model: "gpt-4.1".to_string(),
            status: "completed".to_string(),
            store: true,
            background: false,
            created_at,
            expires_at: created_at + 3600,
            input_items: r#"[{"role":"user","content":"hi"}]"#.to_string(),
            output_items: r#"[{"type":"message"}]"#.to_string(),
            response_json: r#"{"id":"resp_1"}"#.to_string(),
        }
    }

    #[tokio::test]
    async fn insert_get_and_tenant_isolation() {
        let repository = repository().await;
        repository
            .insert_response(&row("resp_1", "tenant_a", 100))
            .await
            .expect("写入");
        let hit = repository
            .get_response("resp_1", "tenant_a", 200)
            .await
            .expect("查询")
            .expect("命中");
        assert_eq!(hit.model, "gpt-4.1");
        assert!(hit.store);
        assert!(!hit.background);
        assert_eq!(hit.conversation_id.as_deref(), Some("conv_1"));

        assert!(
            repository
                .get_response("resp_1", "tenant_b", 200)
                .await
                .expect("查询")
                .is_none(),
            "跨租户不可见"
        );
        assert!(
            repository
                .get_response("resp_1", "tenant_a", 10_000)
                .await
                .expect("查询")
                .is_none(),
            "过期不可见"
        );
    }

    #[tokio::test]
    async fn delete_respects_tenant_and_expiry() {
        let repository = repository().await;
        repository
            .insert_response(&row("resp_1", "tenant_a", 100))
            .await
            .expect("写入");
        assert!(
            !repository
                .delete_response("resp_1", "tenant_b", 200)
                .await
                .expect("删除"),
            "跨租户删除必须落空"
        );
        assert!(
            repository
                .delete_response("resp_1", "tenant_a", 200)
                .await
                .expect("删除"),
            "命中删除"
        );
        assert!(repository
            .get_response("resp_1", "tenant_a", 200)
            .await
            .expect("查询")
            .is_none());
    }

    #[tokio::test]
    async fn conversation_listing_is_ordered() {
        let repository = repository().await;
        repository
            .insert_response(&row("resp_b", "tenant_a", 200))
            .await
            .expect("写入");
        repository
            .insert_response(&row("resp_a", "tenant_a", 100))
            .await
            .expect("写入");
        let rows = repository
            .list_conversation("conv_1", "tenant_a", 300)
            .await
            .expect("列表");
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["resp_a", "resp_b"],
            "按 created_at 升序"
        );
    }

    #[tokio::test]
    async fn status_update_and_purge() {
        let repository = repository().await;
        let mut first = row("resp_1", "tenant_a", 100);
        first.expires_at = 150;
        repository.insert_response(&first).await.expect("写入");
        repository
            .insert_response(&row("resp_2", "tenant_a", 100))
            .await
            .expect("写入");

        repository
            .set_response_status("resp_1", "tenant_a", "failed", r#"{"error":"x"}"#)
            .await
            .expect("状态回写");
        let updated = repository
            .get_response("resp_1", "tenant_a", 140)
            .await
            .expect("查询")
            .expect("命中");
        assert_eq!(updated.status, "failed");
        assert_eq!(updated.response_json, r#"{"error":"x"}"#);

        let purged = repository
            .purge_expired_responses(200, 10)
            .await
            .expect("清理");
        assert_eq!(purged, 1, "仅过期行被清理");
        assert!(repository
            .get_response("resp_2", "tenant_a", 200)
            .await
            .expect("查询")
            .is_some());
    }

    #[tokio::test]
    async fn orphaned_jobs_marked_failed() {
        let repository = repository().await;
        let mut queued = row("resp_1", "tenant_a", 100);
        queued.status = "queued".to_string();
        queued.background = true;
        repository.insert_response(&queued).await.expect("写入");
        let mut running = row("resp_2", "tenant_a", 100);
        running.status = "in_progress".to_string();
        running.background = true;
        repository.insert_response(&running).await.expect("写入");
        repository
            .insert_response(&row("resp_3", "tenant_a", 100))
            .await
            .expect("写入");

        let affected = repository
            .mark_orphaned_responses_failed()
            .await
            .expect("孤儿清理");
        assert_eq!(affected, 2);
        for id in ["resp_1", "resp_2"] {
            let stored = repository
                .get_response(id, "tenant_a", 200)
                .await
                .expect("查询")
                .expect("命中");
            assert_eq!(stored.status, "failed");
        }
        let completed = repository
            .get_response("resp_3", "tenant_a", 200)
            .await
            .expect("查询")
            .expect("命中");
        assert_eq!(completed.status, "completed", "已完成行不受影响");
    }
}
