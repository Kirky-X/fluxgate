// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 临时方言探针（跑完即删）：对 FLUXWEIR_TEST_DB_URL 指向的真实后端
//! 跑全迁移链 + 四列写读 + COUNT/SUM CAST 读回 + UTF-8 标题。
#![cfg(feature = "database")]

use dbnexus::{DbConfig, DbPool};
use fluxweir::database::Repository;

#[ignore = "server: requires FLUXWEIR_TEST_DB_URL (CI server-backend leg)"]
#[tokio::test]
async fn dialect_probe_full_chain_on_server_backend() {
    let url = std::env::var("FLUXWEIR_TEST_DB_URL").expect("need FLUXWEIR_TEST_DB_URL");
    let pool = DbPool::with_config(DbConfig {
        url,
        pool_config: dbnexus::PoolConfig {
            max_connections: 1,
            min_connections: 1,
            ..Default::default()
        },
        admin_role: "admin".into(),
        ..Default::default()
    })
    .await
    .expect("connect");
    let repo = Repository::new(pool.clone());
    repo.initialize().await.expect("migration V1..=V9");
    repo.initialize().await.expect("idempotent rerun");

    let log = fluxweir::database::RequestLog {
        id: None,
        request_id: "dialect-probe-1".into(),
        timestamp: 1_761_436_800,
        endpoint: "/v1/chat/completions".into(),
        input_format: "openai".into(),
        abstract_model: Some("probe-m".into()),
        messages: r#"[{"role":"user","content":"你好 dialect probe"}]"#.into(),
        actual_vendor: "probe-vendor".into(),
        actual_model: "probe-m".into(),
        routing_strategy: "weighted".into(),
        status_code: Some(200),
        response: None,
        error_message: None,
        prompt_tokens: Some(600),
        completion_tokens: Some(30),
        total_tokens: Some(630),
        duration_ms: Some(10),
        cost_usd: Some(0.5),
        session_key: Some("probe-session".into()),
        context_window_tokens: Some(1000),
        cache_read_tokens: Some(60),
        cache_write_tokens: Some(6),
        upstream_key_fingerprint: None,
    };
    // 清场：PG/MySQL 库是共享实例，request_id 有唯一键，先删自己上次的行
    {
        let session = pool.get_session("admin").await.expect("session");
        session
            .execute_raw("DELETE FROM request_logs WHERE request_id LIKE 'dialect-probe%'")
            .await
            .expect("cleanup");
        session
            .execute_raw("DELETE FROM token_usage WHERE vendor = 'probe-vendor'")
            .await
            .expect("cleanup token_usage");
    }

    repo.log_request(&log).await.expect("write request log");

    let sessions = repo.query_sessions(0, 50).await.expect("query_sessions");
    let hit = sessions
        .iter()
        .find(|s| s.session_key == "probe-session")
        .expect("session row present");
    assert_eq!(hit.request_count, 1, "COUNT(*) i64 readback (MySQL CAST)");
    assert_eq!(hit.prompt_tokens, 600);
    assert_eq!(hit.cache_read_tokens, 60);
    assert_eq!(hit.cache_write_tokens, 6);
    assert!((hit.total_cost - 0.5).abs() < 1e-9);
    assert_eq!(hit.first_at, 1_761_436_800);
    assert_eq!(hit.last_at, 1_761_436_800);
    assert_eq!(hit.title, "你好 dialect probe", "UTF-8 title readback");

    let stats = repo
        .query_context_stats(0)
        .await
        .expect("query_context_stats");
    let row = stats
        .iter()
        .find(|r| r.model == "probe-m")
        .expect("context row");
    assert_eq!(row.request_count, 1);
    assert_eq!(
        row.bucket_counts,
        [0, 0, 0, 1, 0],
        "630/1000=63% → b3（60-80% 桶）"
    );
    assert!(
        (row.avg_occupancy - 0.606).abs() < 1e-9,
        "占用率=(600+6)/1000=0.606，实际 {}",
        row.avg_occupancy
    );
    assert!((row.cache_read_share - 60.0 / 606.0).abs() < 1e-9);
}
