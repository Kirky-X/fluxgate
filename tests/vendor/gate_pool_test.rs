// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(feature = "vendor-management")]

//! ConnectionPool 单元测试。
//!
//! 测试属性：
//! 1. max_concurrent=2 时，第三个 acquire() 阻塞等待，drop 第一个后第三个成功
//! 2. available_permits() 反映池容量变化：初始 == max_concurrent，
//!    acquire 后减少，drop 后恢复

#![cfg(feature = "vendor-management")]

use fluxgate::vendor::pool::ConnectionPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn test_third_acquire_waits_then_succeeds_after_drop() {
    let pool = ConnectionPool::new(2);
    let g1 = pool.acquire().await.expect("first acquire should succeed");
    let _g2 = pool.acquire().await.expect("second acquire should succeed");
    assert_eq!(
        pool.available_permits(),
        0,
        "permits should be exhausted after 2 acquires"
    );

    // 启动后台任务尝试第三次 acquire — 应该阻塞
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let pool_clone = pool.clone();
    let task = tokio::spawn(async move {
        let _g3 = pool_clone
            .acquire()
            .await
            .expect("third acquire after drop");
        counter_clone.fetch_add(1, Ordering::SeqCst);
    });

    // 等待 100ms，第三个 acquire 应仍处于等待状态
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "third acquire should still be waiting when permits exhausted"
    );

    // 释放第一个 permit — 第三个 acquire 应当完成
    drop(g1);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("third acquire should complete after first dropped")
        .expect("task should not panic");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "third acquire should have completed after drop"
    );
}

#[tokio::test]
async fn test_pool_capacity_tracks_max_concurrent() {
    let pool = ConnectionPool::new(3);
    assert_eq!(pool.available_permits(), 3, "initial capacity");

    let g1 = pool.acquire().await.expect("first acquire");
    assert_eq!(pool.available_permits(), 2, "after one acquire");

    let g2 = pool.acquire().await.expect("second acquire");
    assert_eq!(pool.available_permits(), 1, "after two acquires");

    let _g3 = pool.acquire().await.expect("third acquire");
    assert_eq!(pool.available_permits(), 0, "after three acquires");

    // Drop 中间一个 — 容量应立即恢复 1
    drop(g2);
    assert_eq!(pool.available_permits(), 1, "after drop g2");

    // 再 acquire — 应立即成功
    let _g4 = pool.acquire().await.expect("acquire after drop");
    assert_eq!(pool.available_permits(), 0, "after re-acquire");

    // 释放所有
    drop(g1);
    drop(_g3);
    drop(_g4);
    assert_eq!(pool.available_permits(), 3, "all permits restored");
}

/// 获取的 OwnedReqwestClient 两条引用口径（client() 访问器与 Deref 自动
/// 解引用）都拿到同一个可发真实请求的 reqwest::Client：各自打一次 mockito
/// 并断言响应状态与正文，permit 随 guard drop 释放后可再获取。
#[tokio::test]
async fn acquired_client_supports_requests_via_accessor_and_deref() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/echo")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true}"#)
        .expect(2)
        .create_async()
        .await;

    let pool = ConnectionPool::new(1);
    let g1 = pool.acquire().await.expect("first acquire");
    // 显式访问器口径
    let resp = g1
        .client()
        .post(format!("{}/echo", server.url()))
        .header("content-type", "application/json")
        .body(r#"{"ping":1}"#)
        .send()
        .await
        .expect("request via accessor");
    assert_eq!(resp.status(), 200);

    // 释放唯一 permit 后重新获取（capacity=1 的排他语义）
    drop(g1);
    let g2 = pool.acquire().await.expect("re-acquire after drop");
    // Deref 口径：guard 直接以 reqwest::Client 的方法调用
    let resp = g2
        .post(format!("{}/echo", server.url()))
        .body("raw")
        .send()
        .await
        .expect("request via deref");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"ok":true}"#);
    mock.assert_async().await;
}
