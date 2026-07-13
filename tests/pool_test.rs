// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! T043 — ConnectionPool 单元测试。
//!
//! 测试属性（规则9）：
//! 1. max_concurrent=2 时，第三个 acquire() 阻塞等待，drop 第一个后第三个成功
//! 2. available_permits() 反映池容量变化：初始 == max_concurrent，
//!    acquire 后减少，drop 后恢复

#![cfg(feature = "vendor-management")]

use nexusflow::vendor::pool::ConnectionPool;
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
        let _g3 = pool_clone.acquire().await.expect("third acquire after drop");
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
