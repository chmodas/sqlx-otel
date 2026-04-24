# sqlx-otel

[![CI build](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml/badge.svg)](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/chmodas/sqlx-otel/graph/badge.svg?token=EFVNRZB3WN)](https://codecov.io/gh/chmodas/sqlx-otel)

Lightweight [SQLx](https://github.com/launchbadge/sqlx) wrapper that emits OpenTelemetry-native spans and metrics following the [database client semantic conventions](https://opentelemetry.io/docs/specs/semconv/database/).

Uses the `opentelemetry` API directly – no `tracing` bridge indirection. Zero-cost when no tracer or meter provider is installed.

## Quick start

```rust
use sqlx_otel::PoolBuilder;

// Wrap an existing sqlx pool.
let raw = sqlx::PgPool::connect("postgres://localhost/mydb").await?;
let pool = PoolBuilder::from(raw).build();

// Use it exactly like a sqlx pool.
let row = sqlx::query("SELECT 1").fetch_one( & pool).await?;

// Transactions work with &mut tx.
let mut tx = pool.begin().await?;
sqlx::query("INSERT INTO users (name) VALUES ($1)")
.bind("Alice")
.execute( & mut tx)
.await?;
tx.commit().await?;
```

Every operation through the pool automatically emits an OpenTelemetry span and records metrics. No code changes are required beyond wrapping the pool.

## Backends

Enable one or more via feature flags:

```toml
[dependencies]
sqlx-otel = { version = "0.0.0", features = ["sqlite", "postgres", "mysql"] }
```

## What you get out of the box

### Spans

Every `Executor` method (`execute`, `fetch`, `fetch_all`, `fetch_one`, `fetch_optional`, `fetch_many`, `execute_many`, `prepare`, `prepare_with`, `describe`) creates a `SpanKind::Client` span with:

| Attribute                   | Source                                          | Condition                   |
|-----------------------------|-------------------------------------------------|-----------------------------|
| `db.system.name`            | Backend (`"postgresql"`, `"sqlite"`, `"mysql"`) | Always                      |
| `db.namespace`              | Database name, extracted from connect options   | Always                      |
| `server.address`            | Hostname, extracted from connect options        | When available              |
| `server.port`               | Port, extracted from connect options            | When available              |
| `network.peer.address`      | Resolved IP address                             | When set via builder        |
| `network.peer.port`         | Resolved port                                   | When set via builder        |
| `db.query.text`             | The SQL query string                            | Unless `QueryTextMode::Off` |
| `db.response.returned_rows` | Row count                                       | On `fetch*` methods         |
| `db.response.status_code`   | SQLSTATE error code                             | On database errors          |
| `error.type`                | Error variant name                              | On any error                |

On error, the span status is set to `Error` and an `exception` event is added with `exception.type` and `exception.message` attributes.

### Metrics

| Instrument                         | Type      | Unit | Description                           |
|------------------------------------|-----------|------|---------------------------------------|
| `db.client.operation.duration`     | Histogram | `s`  | Duration of each database operation   |
| `db.client.response.returned_rows` | Histogram |      | Number of rows returned per operation |

Metrics carry the same connection-level attributes (`db.system.name`, `db.namespace`, `server.address`, `server.port`).

## Configuration

`PoolBuilder` supports overriding auto-extracted attributes and controlling query text capture:

```rust
use sqlx_otel::{PoolBuilder, QueryTextMode};

let pool = PoolBuilder::from(raw_pool)
.with_database("mydb")
.with_host("db.example.com")
.with_port(5432)
.with_network_peer_address("10.0.0.5")
.with_network_peer_port(5432)
.with_query_text_mode(QueryTextMode::Off)
.build();
```

### Query text modes

| Mode             | Behaviour                                                                      |
|------------------|--------------------------------------------------------------------------------|
| `Full` (default) | Capture the parameterised query as-is. Safe because SQLx uses bind parameters. |
| `Obfuscated`     | Suppress query text (obfuscation not yet implemented).                         |
| `Off`            | Do not capture `db.query.text`.                                                |

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
