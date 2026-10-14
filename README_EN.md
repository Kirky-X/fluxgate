<div align="center">

<img src="docs/assets/NexusFlow.png" alt="NexusFlow Logo" width="180">

[![CI Status](https://github.com/Kirky-X/NexusFlow/actions/workflows/ci.yml/badge.svg)](https://github.com/Kirky-X/NexusFlow/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/Kirky-X/NexusFlow)](https://github.com/Kirky-X/NexusFlow/releases) [![License](https://img.shields.io/github/license/Kirky-X/NexusFlow)](LICENSE) [![Rust](https://img.shields.io/badge/rust-1.97.1%2B-orange.svg)](https://www.rust-lang.org/)

[中文](README.md) | **English**

**Intelligent LLM routing gateway — unified multi-provider access · protocol conversion · resilient scheduling**

[✨ Features](#-features) • [🚀 Quick Start](#-quick-start) • [📚 Documentation](#-documentation) • [🐳 Docker Deployment](#-docker-deployment) • [🤝 Contributing](#-contributing)

</div>

---

<div align="center">

### 🎯 One endpoint, many providers

Clients call the gateway in either OpenAI or Anthropic format; the gateway handles auth, rate limiting, routing, protocol conversion and failover, abstracting multiple upstream LLM providers behind a single endpoint:

<table style="width:100%; border-collapse: collapse">
<tr>
<td align="center" width="25%">🔄<br><b>Protocol conversion</b><br><span style="color:#64748B">OpenAI ↔ Anthropic · zero-copy passthrough</span></td>
<td align="center" width="25%">🧭<br><b>Smart routing</b><br><span style="color:#64748B">5-factor weighting · session affinity · LinUCB</span></td>
<td align="center" width="25%">🛡️<br><b>Resilience</b><br><span style="color:#64748B">circuit breaker · retry · fallback chain</span></td>
<td align="center" width="25%">📊<br><b>Observability & cost</b><br><span style="color:#64748B">request logs · token pre-aggregation · cost tracking</span></td>
</tr>
</table>

</div>

---

## 📋 Table of Contents

- [✨ Features](#-features)
- [🚀 Quick Start](#-quick-start)
- [⚙️ Configuration Overview](#️-configuration-overview)
- [📚 Documentation](#-documentation)
- [🏗️ Architecture](#️-architecture)
- [📚 As a Library Dependency](#-as-a-library-dependency)
- [🧪 Testing](#-testing)
- [⚡ Performance Design](#-performance-design)
- [🔒 Security](#-security)
- [🗺️ Roadmap](#️-roadmap)
- [🐳 Docker Deployment](#-docker-deployment)
- [🤝 Contributing](#-contributing)
- [📋 Changelog](#-changelog)
- [📄 License](#-license)
- [🙏 Acknowledgments](#-acknowledgments)
- [📞 Contact & Support](#-contact--support)
- [⭐ Star History](#-star-history)

---

## ✨ Features

<table style="width:100%; border-collapse: collapse">
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">🔄 <b>Protocol conversion</b><br><span style="color:#64748B">Bidirectional OpenAI Chat / Embeddings ↔ Anthropic Messages conversion with SSE streaming on both sides; zero-copy passthrough when the inbound protocol matches the target vendor</span></td>
<td width="50%" style="vertical-align:top; padding: 12px">🧭 <b>Smart routing + difficulty tiers</b><br><span style="color:#64748B">Weighted-random selection over 5 factors (model weight × queue × time window, plus optional latency / error rate) with temperature smoothing; optional Power-of-Two-Choices, LinUCB contextual bandit, session affinity, SLA virtual queue and LPS queueing lookahead</span></td>
</tr>
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">🛡️ <b>Resilience</b><br><span style="color:#64748B">limiteron circuit breaker (opens after 5 failures for 30s, slow-call threshold 60s avoids false trips), status-classified retries (429 honors Retry-After with spread, 5xx fixed-interval delay + spread), priority-driven fallback chain (total attempt budget defaults to 8)</span></td>
<td width="50%" style="vertical-align:top; padding: 12px">🚦 <b>Inbound rate limiting & admission</b><br><span style="color:#64748B">limiteron tower-layer inbound rate limiting (endpoint table + global layer), plus <code>[[rate_limit.rules]]</code> path rules (with no global rules configured, a shared token bucket is the fallback); congestion-aware priority admission (per-key tiers dedicated/guaranteed/elastic/spot)</span></td>
</tr>
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">🔐 <b>Secure auth &amp; key vault</b><br><span style="color:#64748B">API keys stored only as SHA256 digests with constant-time comparison; upstream credentials can persist in the encrypted vault (<code>POST/GET/DELETE /admin/vault/keys</code>, cocoon AEAD at-rest encryption, zero plaintext in responses/logs), seeded once from env and never re-entered — the master key comes from <code>NEXUSFLOW_MASTER_KEY</code> or an auto-generated file (<code>~/.nexusflow/vault_master.key</code>, 0600); <b>losing it is unrecoverable, back it up</b></span></td>
<td width="50%" style="vertical-align:top; padding: 12px">⚡ <b>Response cache</b><br><span style="color:#64748B">Exact-match response cache (oxcache-backed) keyed by tenant + vendor + body fingerprint, tiered TTL multipliers with load linking, concurrent de-duplication (single-flight) carried by oxcache <code>get_or</code>, <code>X-No-Cache: 1</code> to force origin fetch</span></td>
</tr>
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">📊 <b>Request logs & pre-aggregation</b><br><span style="color:#64748B">Asynchronous batched writes to the configured backend (SQLite by default; db-postgres/db-mysql builds write to that backend — drop-counting under backpressure, never blocks responses), token usage UPSERT pre-aggregated by (vendor, model, API-key digest, date, hour)</span></td>
<td width="50%" style="vertical-align:top; padding: 12px">💰 <b>Cost tracking</b><br><span style="color:#64748B">the four-tier unit prices in <code>[[model_catalog]]</code> are the costing source of truth (micro-USD atomic accumulation per vendor/model, hot-reloadable); <code>[[pricing]]</code> feeds only the routing SLA factor and the quota fallback tier for uncataloged models</span></td>
</tr>
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">🔥 <b>Hot config reload</b><br><span style="color:#64748B">Lock-free hot reads via confers DynamicField; transactional write path (validate → atomic persist → in-memory swap) with automatic snapshots in <code>config-backups/</code> (10 kept, secrets masked); <code>server</code>/<code>cors</code> changes are reported explicitly in the <code>restart_required</code> response field (other restart-required sections such as <code>rate_limit</code> carry no such signal — restart manually)</span></td>
<td width="50%" style="vertical-align:top; padding: 12px">🌐 <b>Admin endpoints & observability</b><br><span style="color:#64748B">16 admin/health method endpoints: log query/search, token stats, cost breakdown query, global metrics (affinity hit rate, drop counts), vendor status & key-pool reset, route explain, config & model management, health check</span></td>
</tr>
<tr>
<td width="50%" style="vertical-align:top; padding: 12px">🗣️ <b>Internationalization</b><br><span style="color:#64748B">Fluent dual bundles (en/zh) covering all business messages and error copy, with a <code>NEXUSFLOW_LANG</code> → system locale → en fallback chain</span></td>
<td width="50%" style="vertical-align:top; padding: 12px">🧰 <b>Production ready</b><br><span style="color:#64748B">Multi-stage Docker build with non-root runtime, image-level health check (<code>HEALTHCHECK</code>) plus docker-compose resource limits, GitHub Actions CI (fmt/clippy/test/coverage/audit) and automated release pipeline</span></td>
</tr>
</table>

---

## 🚀 Quick Start

### 📦 Requirements

- Rust 1.97.1+ (the MSRV declared by `rust-version` in `Cargo.toml`; edition 2021)
- SQLite: **statically embedded in every build** — `libsqlite3-sys` compiles the bundled C source and links it statically (`cargo tree` shows bundled; `ldd` on the artifact shows no dynamic sqlite dependency; identical for glibc and musl), so neither a system sqlite library nor `libsqlite3-dev` is required. TLS is pure rustls — no OpenSSL
- The 7 self-developed libraries (`confers`, `dbnexus`, `inklog`, `limiteron`, `oxcache`, `sdforge`, `trait-kit`) come from crates.io registry — cargo fetches them automatically, no sibling directory needed

### 🔨 Build & run

```bash
# Runtime config is not committed (it carries API key digests and vendor endpoints);
# start from the full reference shipped in the repo
cp config.example.toml config.toml

# Reads config.toml from the working directory by default
cargo run --release

# Or specify a config file
cargo run --release -- /path/to/config.toml
```

### 💡 First requests

```bash
# Health check (no auth; the registered path carries the sdforge version prefix)
curl http://127.0.0.1:8080/api/v1/health

# OpenAI-compatible chat completion
curl -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "Hello"}]}'

# Anthropic-compatible messages
curl -X POST http://127.0.0.1:8080/v1/messages \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "claude-3-opus", "max_tokens": 1024, "messages": [{"role": "user", "content": "Hello"}]}'

# Model list (OpenAI list format, catalog ∪ vendors deduplicated live, never cached)
curl http://127.0.0.1:8080/v1/models \
  -H "Authorization: Bearer <your-api-key>"

# Token counting (approximate estimation, deterministic)
curl -X POST http://127.0.0.1:8080/v1/messages/count_tokens \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "gpt-4", "messages": [{"role": "user", "content": "Hello"}]}'
```

> Note: the bare `/health` path is not a registered route (it 404s) but is included in the auth and rate-limit whitelists. See the [📘 API Reference](docs/API_REFERENCE.md) for the full endpoint list.

---

## ⚙️ Configuration Overview

Runtime configuration is a single `config.toml` (**not committed** — `.gitignore` lists it since it carries deployment-private material such as API key digests and upstream endpoints; start from `cp config.example.toml config.toml`, which carries every section and field with its default value), with 18 top-level sections (per-field details of `model_catalog` are covered in the [📖 User Guide](docs/USER_GUIDE.md)):

| Section | Purpose | Hot reload |
|---------|---------|------------|
| `server` / `database` | Listen address, SQLite path / pool size | ❌ restart required |
| `vendors` / `global_model` | Vendors, models, weights, rate-limit forms | ✅ hot-read (`global_model` takes effect only through the admin write path; direct file edits are rejected by the structural gate) |
| `auth` / `cors` | API key digests, CORS | auth ✅ / cors ❌ |
| `routing` / `retry` / `resilience` | Routing coefficients & selection algorithm, retry policy, resilience coverage | ✅ |
| `cache` / `rate_limit` / `admission` | Response cache, inbound rate limiting, congestion admission | cache/admission ✅ / rate_limit ❌ |
| `model_catalog` / `pricing` | Model capability catalog (its four-tier prices are the costing source of truth) / SLA factor and quota fallback tier | ✅ |
| `health` / `quota` | Active health probes (default off), cost/token quota (default off) | ✅ both hot-read |
| `grpc` / `docs` | gRPC facade, OpenAPI/Swagger UI surface (both opt-in) | ❌ restart required |

```toml
[[vendors]]
id = "openai"
type_ = "openai"
api_key_env = "OPENAI_API_KEY"     # upstream key read from the environment
api_base = "https://api.openai.com/v1"
enabled = true

[[vendors.models]]
model_id = "gpt-4"
weight = 1.0                        # participates in routing weights

[[pricing]]
vendor_id = "openai"
model_id = "gpt-4"
input_per_1k_tokens = 0.03
output_per_1k_tokens = 0.06
```

To enable auth, put SHA256 digests of your API keys into the `[auth]` section (`echo -n "your-key" | sha256sum`). Full field reference, rate-limit rules and admission settings: [📖 User Guide](docs/USER_GUIDE.md).

---

## 📚 Documentation

| Document | Description |
|----------|-------------|
| [📖 User Guide](docs/USER_GUIDE.md) | End-to-end tutorial: configuration, deployment, operations |
| [📘 API Reference](docs/API_REFERENCE.md) | All 21 method endpoints (5 data-plane + 15 admin + 1 health): parameters, responses, error codes |
| [🏗️ Architecture](docs/ARCHITECTURE.md) | Design principles, routing decision system, resilience & hot reload |
| [🔒 Security](docs/SECURITY.md) | Security design, supply-chain gates, vulnerability handling |
| [❓ FAQ](docs/FAQ.md) | Common questions and known limitations |
| [📋 Changelog](docs/CHANGELOG.md) | Version history (Keep a Changelog format) |
| [🤝 Contributing](docs/CONTRIBUTING.md) | Environment setup, development commands, commit conventions |

---

## 🏗️ Architecture

NexusFlow uses a layered pipeline: inbound requests pass through the middleware chain (auth → rate limit → admission), then the router picks a target from the eligible vendor set (three-level decision: abstract model → `vendor.model` direct → `auto` global pool), and the request is executed by the VendorManager with circuit breaking and retries, after protocol conversion (or zero-copy passthrough). On the response side, SSE framing and protocol back-conversion complete while logs, token aggregation, cost accumulation, affinity binding and LinUCB rewards are recorded asynchronously.

```mermaid
flowchart TB
    Client["Client (OpenAI / Anthropic format)"]

    subgraph GW["NexusFlow Gateway (Axum 0.8 + sdforge)"]
        direction LR
        MW["Middleware chain<br/>auth (SHA256) · sliding-window limiter (token-bucket fallback) · admission"]
        Handler["API handlers<br/>routing hints · response cache"]
        Router["Routing decision<br/>5-factor weighting · affinity · LinUCB"]
        Conv["Protocol conversion<br/>OpenAI ↔ Anthropic · zero-copy passthrough"]
        Vendor["VendorManager<br/>circuit · retry · pool · fallback"]
        MW --> Handler --> Router --> Conv --> Vendor
    end

    subgraph Store["Persistence (dbnexus · SQLite)"]
        Logs["request_logs (async batched writes)"]
        Agg["token_usage (hourly pre-aggregation)"]
    end

    UP1["OpenAI-compatible upstream"]
    UP2["Anthropic-compatible upstream"]

    Client --> MW
    Vendor --> UP1
    Vendor --> UP2
    Vendor -. async .-> Logs
    Vendor -. async .-> Agg
```

The full routing decision system, resilience state machines, hot-reload transaction semantics, DI assembly and i18n design are documented in the [🏗️ Architecture guide](docs/ARCHITECTURE.md).

NexusFlow is also a **library-first** component set: the server assembly chain lives in `nexusflow::server` (an independently deployable HTTP server) and the Rust client lives in `nexusflow::sdk` (gated behind the `sdk` feature) — the binary `main.rs` is only a thin bootstrap. The `grpc` feature (independently gated, not part of `full`) additionally provides a gRPC server: the sdforge `sdforge.v1.SdForgeService` facade (GetInfo / Call / CallStream, JSON over gRPC) coexists with HTTP in the same process via `take_grpc_server`, authentication reuses the `[auth]` section SHA256 digests, and graceful shutdown shares the same signal as HTTP.

---

## 📚 As a Library Dependency

`nexusflow` is both an executable gateway and a Rust library (`src/lib.rs` exports the public API). The default `full` feature includes the server components; the client SDK is provided by the `sdk` feature (included in `full`, also independently enableable).

**Embedded startup** (hosting the gateway inside your own app):

```rust
#[tokio::main]
async fn main() -> Result<(), nexusflow::error::NexusFlowError> {
    // Assembly (config/database/vendors/routing) + serving + graceful-shutdown
    // flush are all handled by the library
    nexusflow::server::run(Some("config.toml".into())).await
}
```

**HTTP client SDK** (talking to a deployed gateway, requires the `sdk` feature):

```rust
use nexusflow::sdk::{ChatMessage, ChatRequest, ChatTool, ChatToolChoice, ChatToolDefinition, NexusFlowClientBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = NexusFlowClientBuilder::new()
        .base_url("http://127.0.0.1:8080")
        .api_key(std::env::var("NEXUSFLOW_API_KEY")?)
        .build()?;

    // Convenience constructor + request-level config
    // (temperature / max_tokens / hints etc. go through RequestConfig)
    let request = ChatRequest::new("gpt-4", "Hello")
        .with_tools(
            vec![ChatTool {
                function: ChatToolDefinition {
                    name: "get_weather".into(),
                    description: Some("Look up the weather".into()),
                    parameters: serde_json::json!({"type": "object"}),
                },
            }],
            Some(ChatToolChoice::auto()),
        )
        .with_vendor_extension("thinking_mode", serde_json::json!("off"))?; // reserved keys (model/messages etc.) are rejected

    let resp = client.chat(&request).await?;
    println!("{}", resp.content);
    for call in &resp.tool_calls {
        println!("tool_call: {} {}", call.id, call.function.name);
    }
    println!(
        "finish_reason: {} | tokens: {} + {} (cache_read: {})",
        resp.finish_reason.as_wire(),
        resp.usage.prompt_tokens,
        resp.usage.completion_tokens,
        resp.usage.cache_read_tokens
    );
    Ok(())
}
```

> **SDK alignment**: `ChatMessage::content` supports multimodal content parts (`ChatContent::Parts`, image URLs / base64 data URLs); tool-role messages are filled back with `ChatMessage::tool(tool_call_id, content)`; the admin surface uses real routes (catalog `GET/PUT/DELETE /admin/models`, health `GET /api/v1/health`, vendor runtime status).

**Database backend selection (compile-time)**: `full` itself carries no database driver — it must be paired with one `db-*` feature group. `default = ["full", "db-sqlite"]` keeps the standalone default on the sqlite file store.

| Build target | Feature set |
| --- | --- |
| Standalone (default, sqlite) | `cargo run` |
| Standalone (PostgreSQL) | `cargo build --no-default-features --features "full,db-postgres"` |
| Standalone (MySQL) | `cargo build --no-default-features --features "full,db-mysql"` |
| Embedded library (own backend) | `nexusflow = { default-features = false, features = ["full", "db-postgres"] }` |

- The three groups are mutually exclusive, enforced by dbnexus's compile-time guard (embedded and server-side backends cannot mix). `--all-features` therefore fails to compile — this is expected; CI and pre-commit use explicit feature matrices instead.
- On the config side, `[database] url` (a full connection URL) takes precedence when set; when empty, `path` falls back to the sqlite file store (without the db-sqlite feature the startup fails with `database-backend-missing` and a rebuild hint).
- Schema and column migrations dispatch on the connected backend (`schema.postgres.sql` / `schema.mysql.sql` are dialect projections of `schema.sql`; keep all three in sync when adding tables/columns).

> The SDK is an "HTTP client" (calling the gateway REST API, including SSE streaming), not an in-process route-core binding; for embedding use `nexusflow::server` and `nexusflow::api` directly.

---

## 🧪 Testing

### 🎯 Strategy

Unit tests live inline in `src/` (`#[cfg(test)]`); integration tests are organized by functional domain under `tests/` subdirectories (api / routing / resilience / config / database / protocol / vendor / cost / server / i18n / matrix / utils / guard, 49 `.rs` files in total — 48 test binaries plus the `tests/common/mod.rs` helper), each binary explicitly registered via `[[test]]` in the root `Cargo.toml` (one binary per file, process-exclusive semantics preserved; the grpc_smoke / grpc_coverage pair carries `required-features` and compiles only under the `grpc` caliber — default builds no empty binaries). Mocks use mockall / mockito; time-sensitive tests inject the limiteron MockClock fake clock.

### ▶️ Commands (CI-consistent)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Non-default backend matrix (must scope to this package with `-p nexusflow`,
# otherwise workspace members pull in db-sqlite and the backends collide)
cargo check -p nexusflow --no-default-features --features "full,db-postgres"
cargo check -p nexusflow --no-default-features --features "full,db-mysql"
```

Caliber note: default already carries `full + db-sqlite`; `--all-features` opens all three mutually exclusive backends and cannot compile (see the section above), so clippy / test / coverage run on the default caliber while non-default backends are checked per package — literally matching `ci.yml`.

### 📊 Scale

As of the current version (measured with `cargo test --workspace`): **1379 tests pass, 0 failures** — 490 unit tests (inline in `src/`) plus 889 integration tests (48 test binaries under `tests/`, 46 compiled on the default caliber — grpc_smoke / grpc_coverage build with the `grpc` feature); one `rust,ignore` example doctest is listed but not executed. Overall line coverage is measured with `cargo llvm-cov --workspace`. Main distribution:

| Test domain | Cases | Location | Coverage |
|-------------|-------|----------|----------|
| protocol_test | 157 | tests/protocol/ | OpenAI ↔ Anthropic conversion / streaming variants / SSE framing |
| api_test | 131 | tests/api/ | endpoint integration / caching / rerouting / affinity / admin |
| vendor_test | 111 | tests/vendor/ | vendor management / rate limits / stream timeouts / health & metrics |
| router_test | 97 | tests/routing/ | weighted selection / affinity / hint routing / edges |
| database / sse | 41 / 40 | tests/database/ · tests/protocol/ | persistence & pre-aggregation / stream framing & SDK |
| config / utils_coverage | 28 / 19 | tests/config/ · tests/utils/ | config loading / metrics & limiter edges |
| retry / config_coverage | 23 / 20 | tests/resilience/ · tests/config/ | retry classes & delay policy / loader·manager branches |
| quota / fallback / admission | 20 / 17 / 16 | tests/resilience/ | quota window / fallback chain / admission |
| bandit / model_catalog_admin | 13 / 13 | tests/routing/ · tests/api/ | bandit routing / model catalog admin |
| server·grpc serving / assembly smoke / feature matrix / key pool etc. | remainder (138) | tests/server/ & more | server·grpc serving / assembly smoke / feature matrix / key pool / price factors / model catalog admin / dead-code guard |

---

## ⚡ Performance Design

No official benchmarks have been published yet (tracked on the roadmap). Verified design points: zero-copy passthrough for same-protocol requests, lock-free O(1) config reads via arc-swap, fully asynchronous logging and pre-aggregation (batched transactions; backpressure drops are counted, never blocking), a single pooled reqwest client with a semaphore concurrency cap, and oxcache in-memory backends for session affinity and the response cache. Design details: [🏗️ Architecture · Performance design](docs/ARCHITECTURE.md).

---

## 🔒 Security

### 🛡️ Design

API keys stored as SHA256 digests with constant-time comparison, upstream credentials referenced by env var name only, error responses exposing fixed `error.type` codes only, secrets masked in config snapshots, layered CORS and inbound rate limiting (429), non-root Docker runtime. See the [🔒 Security guide](docs/SECURITY.md) for details.

### 🚨 Reporting a vulnerability

Please do not report security vulnerabilities through public issues. Use the GitHub [Security Advisories](https://github.com/Kirky-X/NexusFlow/security/advisories/new) private disclosure channel instead. The project commits to acknowledging within 48 hours and providing an initial assessment within 7 days.

---

## 🗺️ Roadmap

<table style="width:100%; border-collapse: collapse">
<tr><th style="text-align:center">Status</th><th style="text-align:left">Area</th><th style="text-align:left">Items</th></tr>
<tr><td align="center">✅</td><td>Core gateway</td><td>OpenAI / Anthropic conversion, SSE streaming, zero-copy passthrough, 21 method endpoints (5 data-plane + 15 admin + 1 health)</td></tr>
<tr><td align="center">✅</td><td>Smart routing</td><td>5-factor weighting + temperature smoothing, session affinity, LinUCB, Power-of-Two, optional SLA / LPS layers; difficulty-tiered routing (rule + in-process decision-engine fusion behind the <code>difficulty-classifier</code> feature)<; cache-cost suite (prefix-affinity PoTC / semantic cache / cache-aware pricing, all off by default)</td></tr>
<tr><td align="center">✅</td><td>Resilience</td><td>limiteron circuit / retry / fallback chain, congestion-aware admission, key tiers</td></tr>
<tr><td align="center">✅</td><td>Observability & cost</td><td>Async logging, hourly token pre-aggregation, global metrics, four-tier `[[model_catalog]]` costing and cost breakdown</td></tr>
<tr><td align="center">✅</td><td>Hot reload</td><td>confers DynamicField lock-free reads, transactional writes, snapshots with restart_required reporting</td></tr>
<tr><td align="center">✅</td><td>Internationalization</td><td>Fluent en/zh bundles covering all business messages</td></tr>
<tr><td align="center">✅</td><td>Cost query API</td><td>Cost breakdown persisted (<code>token_usage</code> four-way cost columns, V2 migration) + <code>/admin/cost</code> aggregation endpoint (pre-aggregated table grouped by vendor/model); the in-process CostTracker still accumulates in memory and resets on restart (see known boundaries)</td></tr>
<tr><td align="center">✅</td><td>Rust SDK</td><td>Merged into <code>src/sdk</code> (<code>sdk</code> feature gate; the broken workspace inheritance is gone): chat / stream / embed / admin</td></tr>
<tr><td align="center">🔜</td><td>Performance baselines</td><td>Establish Criterion baselines (passthrough / conversion / routing decision paths)</td></tr>
<tr><td align="center">🔜</td><td>Log query completion</td><td>Push <code>/admin/logs</code> end_time filtering into SQL (known gap, see the <a href="docs/FAQ.md">FAQ</a>)</td></tr>
</table>

---

## 🐳 Docker Deployment

```bash
# Build (multi-stage: rust:1.99-alpine3.24 build → alpine:3.24 runtime, non-root)
# Keep both base images on the same alpine branch: the artifact is statically
# linked against musl, and the runtime stage re-verifies with ldd that every
# dynamic dependency resolves before the image is published
# Note: -f points at the Dockerfile while the build context stays at the repo
# root (bind mounts and .dockerignore resolve against the context root)
docker build -t nexusflow:0.1.0 -f docker/Dockerfile .

# Run (read-only config mount + SQLite data persists under /app/data)
docker run -d -p 8080:8080 \
  -v $(pwd)/config.toml:/app/config.toml:ro \
  -v $(pwd)/data:/app/data nexusflow:0.1.0

# Or one-command deployment with resource limits
# (the compose file lives under docker/)
docker compose -f docker/docker-compose.yml up -d
```

> Health check: provided solely by the **image-level HEALTHCHECK** (compose no
> longer duplicates it), probing the registered path `/api/v1/health`;
> docker-compose ships with resource limits (2 CPUs / 512M) and a non-root
> runtime. The image tag defaults to `nexusflow:0.1.0`, overridable via
> `NEXUSFLOW_IMAGE_TAG` (see [📖 User Guide · Docker](docs/USER_GUIDE.md)).

---

## 🤝 Contributing

See the [🤝 Contributing guide](docs/CONTRIBUTING.md) for the full workflow, commands and commit conventions.

### 🛠️ Development environment

Toolchain baseline: MSRV floor **1.97.1** (declared in `Cargo.toml` via `rust-version`, matching the 7 sibling libraries; ci.yml builds and lints at that floor so the clippy lint set stays reproducible), while `docker/Dockerfile` uses the currently supported stable (1.99). Run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` before committing; commit messages follow Conventional Commits (`feat(scope): ...` / `fix(scope): ...`).

### 💖 Ways to contribute

<table style="width:100%; border-collapse: collapse">
<tr>
<td width="33%" align="center" style="padding: 16px">

### 🐛 Report a bug

Found an issue?<br>
<a href="https://github.com/Kirky-X/NexusFlow/issues/new">Open an issue</a>

</td>
<td width="33%" align="center" style="padding: 16px">

### 💡 Suggest a feature

Have an idea?<br>
<a href="https://github.com/Kirky-X/NexusFlow/discussions">Start a discussion</a>

</td>
<td width="33%" align="center" style="padding: 16px">

### 🔧 Submit a PR

Want to contribute code?<br>
<a href="https://github.com/Kirky-X/NexusFlow/pulls">Fork and open a PR</a>

</td>
</tr>
</table>

---

## 📋 Changelog

The full version history lives in the [📋 Changelog](docs/CHANGELOG.md) (following [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), semantic versioning).

| Version | Date | Highlights |
|---------|------|------------|
| 0.1.0 | 2026-10-12 | Initial release, continuously iterated (63 commits): protocol-converting gateway, 5-factor weighted routing with optional smart layers, limiteron resilience system, async logging & pre-aggregation, cost tracking, confers hot-reload foundation, i18n, 35 optimization recommendations landed |

---

## 📄 License

This project is licensed under the [MIT License](LICENSE).

---

## 🙏 Acknowledgments

### 🌟 Core dependencies (sibling libraries)

NexusFlow integrates the following self-developed libraries through the crates.io registry (versions declared in the root `Cargo.toml`, checksums locked in `Cargo.lock`):

| Library | Purpose |
|---------|---------|
| [confers](https://github.com/Kirky-X/confers) | Config loading guards & hot-reload foundation (DynamicField / SnapshotManager) |
| [dbnexus](https://github.com/Kirky-X/dbnexus) | SQLite persistence (pooling / batched transactions / kit integration) |
| [inklog](https://github.com/Kirky-X/inklog) | Structured logging (HTTP / CLI / SQLite) |
| [limiteron](https://github.com/Kirky-X/limiteron) | Rate limiting / circuit breaking / retry primitives |
| [oxcache](https://github.com/Kirky-X/oxcache) | Unified cache API (storage for affinity & response cache) |
| [sdforge](https://github.com/Kirky-X/sdforge) | HTTP service framework (`#[forge]` macro / route registration) |
| [trait-kit](https://github.com/Kirky-X/trait-kit) | DI container (AsyncKit, 4-module topology) |

### 💝 Special thanks

Thanks to the Rust community and all [contributors](https://github.com/Kirky-X/NexusFlow/graphs/contributors).

---

## 📞 Contact & Support

<table style="width:100%; max-width: 600px">
<tr>
<td align="center" width="33%">
<a href="https://github.com/Kirky-X/NexusFlow/issues"><b style="color:#991B1B">Issues</b></a><br>
<span style="color:#64748B">Report problems and bugs</span>
</td>
<td align="center" width="33%">
<a href="https://github.com/Kirky-X/NexusFlow/discussions"><b style="color:#1E40AF">Discussions</b></a><br>
<span style="color:#64748B">Ask questions and share ideas</span>
</td>
<td align="center" width="33%">
<a href="https://github.com/Kirky-X/NexusFlow"><b style="color:#1E293B">GitHub</b></a><br>
<span style="color:#64748B">Browse the source</span>
</td>
</tr>
</table>

---

## ⭐ Star History

[![Star History Chart](https://api.star-history.com/svg?repos=Kirky-X/NexusFlow&type=Date)](https://star-history.com/#Kirky-X/NexusFlow&Date)

If this project helps you, please consider giving it a ⭐️!

**Built by Kirky.X**

---

<sub>© 2026 Kirky.X. All rights reserved.</sub>
