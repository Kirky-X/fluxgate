[中文](README.md) | [English](README_EN.md)

# NexusFlow

Intelligent LLM Routing Proxy System - Multi-vendor unified management, format conversion, intelligent scheduling

Built with Rust 2021 edition + Axum 0.8 + sdforge, providing a unified gateway for OpenAI / Anthropic APIs. Through a unified protocol conversion layer and intelligent routing scheduling, multiple upstream LLM vendors are abstracted into a single access point, supporting weighted routing, rate limiting and circuit breaking, request logging, token pre-aggregation statistics, and other production-grade capabilities.

## Features

- Multi-vendor unified access: OpenAI / Anthropic protocol interconversion, clients can access any vendor using either format
- Intelligent routing scheduling: weighted random selection based on the 3D weight formula `model.weight × 0.4 + queue_factor × 0.3 + window_factor × 0.3`
- Zero-copy passthrough: directly forwards when inbound and outbound protocols match, skipping conversion overhead
- Intelligent retry: classified by status code (429 exponential backoff, 500 fixed interval)
- Rate limiting and circuit breaking: GCRA token bucket + circuit breaker based on limiteron, supporting global rate limiting and per-endpoint rate limiting
- Protocol conversion: bidirectional conversion between OpenAI Chat / Embeddings ↔ Anthropic Messages
- Request logging: asynchronous batch writes to SQLite, supporting time/vendor/model filter queries
- Token pre-aggregation: request-level token usage statistics aggregation, avoiding full table scans
- Observability: global metrics (request count / latency / error rate), vendor status, model list management
- Hot configuration reload: runtime configuration changes via the `/admin/config` endpoint
- Security: SHA256 API Key authentication, CORS, rate limit three-layer protection
- Production-ready: Docker multi-stage build, docker-compose one-click deployment, GitHub Actions CI/CD

## Architecture

```
                          ┌─────────────────────────────────────────────┐
                          │              NexusFlow Gateway              │
                          │            (Axum 0.8 + sdforge)             │
                          │                                             │
   Client ──HTTP────────► │  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
   (OpenAI /              │  │ Auth MW  │─►│ Rate MW  │─►│ Router   │  │
    Anthropic             │  │ (SHA256) │  │(limiteron)│  │(3D weight)│  │
    format)               │  └──────────┘  └──────────┘  └────┬─────┘  │
                          │                                   │       │
                          │                ┌──────────────────┼─────┐ │
                          │                ▼                  ▼     │ │
                          │  ┌──────────────┐      ┌──────────────┐  │ │
                          │  │ Protocol Conv│      │VendorManager │  │ │
                          │  │  (zero-copy  │      │ (health/     │  │ │
                          │  │  passthrough)│      │  retry)      │  │ │
                          │  └──────┬───────┘      └──────┬───────┘  │ │
                          └─────────┼─────────────────────┼──────────┘ │
                                    │                     │            │
                          ┌─────────▼─────────────────────▼──────────┐ │
                          │           Database (dbnexus)              │ │
                          │     ┌──────────────────────────────┐      │ │
                          │     │ SQLite (sqlx/sea-orm)        │      │ │
                          │     │ - request_logs (batch write) │      │ │
                          │     │ - token_usage (pre-aggregation)│     │ │
                          │     │ - vendor_stats               │      │ │
                          │     └──────────────────────────────┘      │ │
                          └────────────────────────────────────────────┘ │
                                    │                     │            │
                      ┌─────────────▼──────┐    ┌──────────▼─────────┐  │
                      │  OpenAI Vendor     │    │  Anthropic Vendor   │  │
                      │  (gpt-4, etc.)     │    │  (claude-3, etc.)   │  │
                      └────────────────────┘    └─────────────────────┘  │
```

## Quick Start

### Requirements

- Rust 1.75+ (edition 2021)
- SQLite (system library or compiled via rustls)
- Sister library source code must be in sibling directories (path dependencies)

### Build and Run

```bash
# Default config file config.toml
cargo run --release

# Specify config file
cargo run --release -- /path/to/config.toml

# Server listens on 0.0.0.0:8080
```

### First Request

```bash
# Health check (no auth required)
curl http://127.0.0.1:8080/health

# OpenAI-compatible chat completion
curl -X POST http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4",
    "messages": [{"role": "user", "content": "Hello"}]
  }'

# Anthropic-compatible message
curl -X POST http://127.0.0.1:8080/v1/messages \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-3-opus",
    "messages": [{"role": "user", "content": "Hello"}],
    "max_tokens": 1024
  }'
```

## Configuration

Runtime configuration file `config.toml`, key configuration items:

```toml
[server]
host = "0.0.0.0"
port = 8080

[database]
path = "./data/llm_proxy.db"
pool_size = 5

# Auth: API Key stored as SHA256 hash
# Generate: echo -n "your-api-key" | sha256sum
[auth]
enabled = true
api_keys = []  # Fill in SHA256 hashes

[auth.header]
name = "Authorization"
prefix = "Bearer"

[cors]
enabled = true
allowed_origins = ["*"]
allowed_methods = ["GET", "POST", "OPTIONS", "PUT", "DELETE"]

[rate_limit]
enabled = true
requests_per_second = 100
burst_size = 200
window_seconds = 60

# Per-endpoint rate limiting
[[rate_limit.endpoints]]
path = "/v1/chat/completions"
requests_per_minute = 1000

[[rate_limit.endpoints]]
path = "/v1/embeddings"
requests_per_minute = 500

[[rate_limit.endpoints]]
path = "/admin/*"
requests_per_minute = 100

# Vendor configuration (supports multiple)
[[vendors]]
id = "mock-openai"
name = "Mock OpenAI"
type_ = "openai"
api_key_env = "MOCK_OPENAI_KEY"   # Read actual key from environment variable
api_base = "http://127.0.0.1:8001/v1"
enabled = true
concurrency = { requests_per_second = 100 }
rate_limit = { type = "token", max_tokens = 10000000 }

[[vendors.models]]
model_id = "gpt-4"
enabled = true
weight = 1.0    # Routing weight, participates in 3D weight calculation

[global_model]
enabled = true
strategy = "weighted"   # Global weighted routing strategy
```

Environment variables: The name specified via the `api_key_env` field is used to read the upstream vendor's API Key from environment variables, avoiding hardcoding into the configuration file.

## API Endpoints

14 routes total (`/admin/config` single route supports both GET + POST methods):

### Business Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/v1/chat/completions` | OpenAI-compatible chat completion |
| POST | `/v1/embeddings` | OpenAI embeddings |
| POST | `/v1/messages` | Anthropic-compatible message |

### Admin Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/logs` | Request log query (supports time/vendor/model filtering) |
| GET | `/admin/logs/search` | Log keyword search |
| GET | `/admin/stats/tokens` | Token usage statistics (pre-aggregated) |
| GET | `/admin/metrics` | Global metrics (request count/latency/error rate) |
| GET | `/admin/vendors` | Vendor list |
| GET | `/admin/vendors/:id/status` | Vendor status |
| POST | `/admin/vendors/:id/reset` | Reset vendor (circuit breaker/rate limiter) |
| GET | `/admin/config` | Get current configuration |
| POST | `/admin/config` | Update configuration |
| GET | `/admin/models` | Model list |
| POST | `/admin/models/:id` | Update model configuration |

### Health Check

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Service health check (no auth required) |

## Docker Deployment

```bash
# Build image
docker build -t nexusflow:latest .

# Run container
docker run -d -p 8080:8080 -v $(pwd)/data:/app/data nexusflow:latest

# Or use docker-compose for one-click deployment
docker-compose up -d
```

Data persistence: The SQLite database file is mounted in the `/app/data` directory, mapped to the host via `-v $(pwd)/data:/app/data`.

## Development Guide

### Project Structure

```
src/
├── api/               # REST endpoints (axum handlers)
│   ├── admin.rs       # Admin endpoints
│   ├── anthropic.rs   # Anthropic message endpoint
│   ├── openai.rs      # OpenAI chat/embeddings endpoint
│   ├── health.rs      # Health check (sdforge #[service_api])
│   ├── router_builder.rs  # Route building + rate limit middleware
│   └── state.rs       # Shared state
├── config/            # TOML config loading
├── database/          # SQLite persistence (dbnexus)
├── middleware/         # Auth/CORS middleware
├── protocol/          # OpenAI ↔ Anthropic protocol conversion
├── router/            # Intelligent routing + 3D weight calculation
├── vendor/            # Vendor management (rate limit, retry, health)
├── infrastructure/    # DI container (trait-kit AsyncKit)
└── utils/             # Utility functions
```

### Cargo Features

Default enables `full`, including all 8 features:

| Feature | Dependencies | Description |
|---------|--------------|-------------|
| `web-server` | axum, tower, tower-http, async-stream | HTTP server |
| `database` | dbnexus, sea-orm, async-channel | Database layer |
| `vendor-management` | reqwest, limiteron | Vendor management |
| `router` | - | Routing scheduling (depends on vendor-management) |
| `protocol` | - | Protocol conversion |
| `infrastructure` | oxcache, trait-kit | Cache + DI container |
| `sdforge-integration` | sdforge | HTTP framework integration |
| `logging` | inklog | Structured logging |

### Compilation Check

```bash
cargo clippy -- -D warnings   # Zero-warning check (currently 0 warnings)
cargo build --release         # Production build (LTO + opt-level 3)
```

### Code Conventions

- Error handling: `anyhow` + `thiserror`, using `?` operator, errors must be explicitly propagated
- Async: Tokio runtime, all I/O must be async
- Naming: snake_case (Rust convention)
- Testing: `tests/` integration tests + `#[cfg(test)]` inline unit tests

## Testing

```bash
# Run all tests
cargo test

# Baseline: 307 passed + 0 ignored + 0 failed
# clippy: 0 warnings
```

Test coverage:

| Module | Test Count | Coverage |
|--------|-----------|----------|
| config | 8 | Config loading/validation |
| protocol | 7 | OpenAI ↔ Anthropic conversion |
| vendor | 13 | rate limit / retry / health |
| router | 11 | 3D weight / weighted selection / boundary conditions |
| database | 15 | Log write/query/Token aggregation |
| api | 22 | Endpoint integration tests |
| middleware | - | Auth/CORS |

### Security Audit Status

| Audit Item | Result |
|------------|--------|
| diting code review | ✅ 0 Critical / 0 High |
| tiangang SAST scan | ✅ 0 CRITICAL vulnerabilities |
| cargo-audit | ✅ 0 vulnerabilities |

## Sister Library Ecosystem

This project integrates the following local source libraries via Cargo path dependencies (must be in sibling directories with NexusFlow):

| Library | Purpose | Alternative |
|---------|---------|-------------|
| [dbnexus](../dbnexus) | SQLite database layer (with kit integration) | Direct sqlx usage |
| [inklog](../inklog) | Structured logging (HTTP/CLI/SQLite) | tracing-subscriber |
| [limiteron](../limiteron) | Rate limiting / circuit breaking (GCRA + circuit breaker) | Hand-written rate limiting |
| [oxcache](../oxcache) | Unified cache API (memory backend) | Hand-written cache |
| [sdforge](../sdforge) | HTTP server framework + API gateway | Raw axum usage |
| [trait-kit](../trait-kit) | DI container (AsyncKit, async dependency injection) | Manual wiring |

All sister libraries are assembled via the `trait-kit` 0.3 AsyncKit DI container. On `main.rs` startup, it builds and verifies `4/4 modules resolved`.

## License

MIT License — see source file headers `Licensed under the MIT License`.
