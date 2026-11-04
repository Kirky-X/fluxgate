// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
#![cfg(all(feature = "web-server", feature = "logging"))]

//! server::run 生产起服链路覆盖测试。
//!
//! 新建目的：src/server/http.rs 的 run 主体（bootstrap logger/config 装配、
//! bind/serve、停机广播消费）与 sdforge_config 的 [cors] 过滤/构造分支此前
//! 仅由 build_app 冒烟间接触及，run 主体 0 覆盖。
//!
//! 手段：真实启动 run(Some(config))，用 std TcpStream 轮询 /api/v1/health
//! 直到 200（拿到响应即证明 axum::serve 已 accept、停机信号 handler 已在
//! 其之前注册），随后向本进程发 SIGTERM 触发优雅停机，断言 run 返回 Ok。
//!
//! run 内部经 init_global_state 注册全局态（OnceCell 一次性写入），本文件
//! 进程内仅此一个消费全局态的测试，全部断言收敛在单一 #[tokio::test] 内。

#![recursion_limit = "256"]

use std::io::{Read, Write};
use std::time::Duration;

use fluxweir::database::{Database, QuotaLedgerRow};
use fluxweir::storage::{ResponseStoreSink, StoredResponse};

/// 探测一个当前空闲的回环端口并释放（bind :0 → 读取端口号 → drop）。
/// 消除固定端口的并行 CI / TIME_WAIT 敏感性；极小概率的释放-复用竞态
/// 由 wait_http_ready 的重试窗口吸收。
fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 to find a free port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// 点亮并求值 tracing 事件（registry + 全字段格式化层）：无订阅者时
/// info/warn/error 宏整体跳过；有订阅者但不记录时 message 字段的惰性
/// 格式化闭包（i18n 文案 t_args）仍不求值——on_event 内 format 全部字段
/// 才能迫使这些行执行。同时预装 log 门面桥：run 内 LogTracer::init 二次
/// 安装走「桥已装」告警分支。本文件进程内仅一个测试，全局安装无串扰。
static SEEN_TARGETS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static RECORD_DEBUG_MSG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn install_callsite_enabler() {
    struct FormatAllLayer;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FormatAllLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if let Ok(mut seen) = SEEN_TARGETS.lock() {
                seen.push(event.metadata().target().to_string());
            }
            struct FormatAll;
            impl tracing::field::Visit for FormatAll {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        RECORD_DEBUG_MSG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if let Ok(mut seen) = SEEN_TARGETS.lock() {
                            seen.push(format!("MSG: {value:?}"));
                        }
                    }
                    let _ = format!("{value:?}");
                }
            }
            let mut visitor = FormatAll;
            event.record(&mut visitor);
        }
    }

    use tracing_subscriber::layer::SubscriberExt as _;
    let _ = tracing_log::LogTracer::init();
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(FormatAllLayer),
    );
}

/// 预置文件库重启态：配额账本一行（未过期窗，window_id 对齐 [quota] 缺省
/// 编译出的 "default" 单 rolling 窗）+ 孤儿响应一行（in_progress）——
/// run() 装配期读回后分别触发「账本恢复 N>0」与「孤儿清扫 N>0」日志分支。
async fn seed_restart_state(db_path: &std::path::Path) {
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let database = Database::new(&url, 1).await.expect("seed database init");
    let now = chrono::Utc::now().timestamp();

    database
        .repository
        .upsert_quota_ledger(&[QuotaLedgerRow {
            api_key_hash: "seed-tenant-hash".to_string(),
            window_id: "default".to_string(),
            window_start: now - 60,
            tokens_used: 4321,
            cost_used_millicents: 99,
            updated_at: now,
        }])
        .await
        .expect("seed quota ledger row");

    ResponseStoreSink::insert_response(
        database.repository(),
        &StoredResponse {
            id: "resp-seed-orphan".to_string(),
            tenant_hash: "seed-tenant-hash".to_string(),
            conversation_id: None,
            parent_response_id: None,
            model: "gpt-4".to_string(),
            status: "in_progress".to_string(),
            store: true,
            background: false,
            created_at: now,
            expires_at: now + 3600,
            input_items: "[]".to_string(),
            output_items: "[]".to_string(),
            response_json: "{}".to_string(),
        },
    )
    .await
    .expect("seed orphan response row");

    drop(database);
}

/// run 的配置面：[cors] 混入通配符 "*"（覆盖 sdforge_config 的 origin 过滤
/// 与 CorsConfig 构造分支）、[cache] 开启（覆盖 bootstrap 的 enabled 日志
/// 分支）、[grpc] 段启用（default 构建下仅解析不生效；grpc feature 下覆盖
/// run 的 gRPC 装配/启动/排空路径）。
///
/// 覆盖面扩展（文件库 + 装配分支）：
/// - [database] 指向预置过重启态（配额账本/孤儿响应）的文件库——装配期
///   「账本恢复 N>0」「孤儿置 failed N>0」与 CostTracker 文件库恢复路径入覆盖；
/// - [cache.semantic] 开启而 semantic-cache feature 缺席——装配期
///   「语义缓存特性缺失」error 分支入覆盖；
/// - [routing.difficulty] 开启（w_clf=0：default 构建无分类器特性，V4 校验
///   要求其为 0）——难度定档接线分支入覆盖；model_id 与 abstract_id 拆开，
///   档位名（抽象名）不得撞真实 model_id（validate_difficulty 撞名拒绝）；
/// - [quota] 开启——账本读侧下界（now − 最长窗）放行预置的未过期窗行。
fn run_config_toml(http_port: u16, grpc_port: u16, db_path: &str) -> String {
    format!(
        r#"
[server]
host = "127.0.0.1"
port = {http_port}

[database]
path = "{db_path}"
pool_size = 1

[[vendors]]
id = "openai_main"
name = "OpenAI Main"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"
concurrency = {{ requests_per_second = 1000 }}
rate_limit = {{ type = "token", max_tokens = 1000000 }}

[[vendors.models]]
model_id = "gpt-4-fast"
abstract_id = "gpt-4"

[global_model]
enabled = true
strategy = "weighted"

[cors]
enabled = true
allowed_origins = ["https://console.example.com", "*"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Authorization"]

[cache]
enabled = true

[cache.semantic]
enabled = true

[routing.difficulty]
enabled = true
w_clf = 0.0
w_rule = 0.4

[routing.difficulty.tiers]
low = "gpt-4"
high = "gpt-4"

[quota]
enabled = true

[grpc]
enabled = true
host = "127.0.0.1"
port = {grpc_port}
"#
    )
}

/// 轮询直到 /api/v1/health 返回 200。std 阻塞 IO + 短超时，不引入 reqwest
/// 依赖；read_timeout 兜底防响应未回时读挂。
async fn wait_http_ready(addr: std::net::SocketAddr) {
    for _ in 0..200 {
        if let Ok(mut stream) = std::net::TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let request =
                format!("GET /api/v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                if String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200") {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("server did not serve /api/v1/health 200 within 10s at {addr}");
}

/// SIGTERM 触发优雅停机（shutdown_signal 的 terminate 分支）：
/// run 起服 → 真实 HTTP 探活 → kill 自身 → run 排空并返回 Ok。
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_serves_http_until_sigterm_then_flushes() {
    install_callsite_enabler();
    let tmpdir = tempfile::tempdir().expect("tempdir creatable");
    let dir = tmpdir.path().to_path_buf();
    let http_port = free_loopback_port();
    let grpc_port = free_loopback_port();
    let db_path = dir.join("gate.db");
    seed_restart_state(&db_path).await;
    // 保密柜 env 吸收：api_key_env 命中已设环境变量 → absorbed>0 日志分支入覆盖。
    // 测试结束必须移除：进程内后续「缺 key」用例会读到伪造值（假阴/假阳）。
    std::env::set_var("OPENAI_API_KEY", "sk-cov-absorb-0123456789abcdef");
    let config_path = dir.join("config.toml");
    let db_path_str = db_path.to_string_lossy().to_string();
    std::fs::write(
        &config_path,
        run_config_toml(http_port, grpc_port, &db_path_str),
    )
    .expect("write run config");

    let config = config_path.to_string_lossy().to_string();
    let run_task = tokio::spawn(async move { fluxweir::server::run(Some(config)).await });

    wait_http_ready(format!("127.0.0.1:{http_port}").parse().unwrap()).await;

    // 此刻停机广播 handler 已注册（HTTP 200 证明 serve 已 accept），
    // SIGTERM 只会触发优雅停机而非进程默认终止
    let status = std::process::Command::new("kill")
        .args(["-s", "TERM", &std::process::id().to_string()])
        .status()
        .expect("kill(1) should exist on unix");
    assert!(status.success(), "kill -TERM delivery should succeed");

    // 若本测试"无输出地消失"而非断言失败，说明 run 的信号注册路径被破坏，
    // SIGTERM 走了进程默认终止——先查 shutdown_signal 的 handler 注册
    let result = tokio::time::timeout(Duration::from_secs(30), run_task)
        .await
        .expect("run should stop well before 30s after SIGTERM")
        .expect("run task must not panic");
    assert!(result.is_ok(), "优雅停机后 run 必须返回 Ok: {result:?}");

    let _ = std::fs::remove_dir_all(dir);
    // 清理 env 吸收夹具，防跨用例污染
    std::env::remove_var("OPENAI_API_KEY");
}
