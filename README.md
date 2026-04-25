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

## Feature flags

### Backends

```toml
[dependencies]
sqlx-otel = { version = "0.0.0", features = ["postgres"] }
# or "sqlite", "mysql"
```

### Runtime (optional)

Enable a runtime to get `db.client.connection.count` polling via a background task:

```toml
sqlx-otel = { version = "0.0.0", features = ["postgres", "runtime-tokio"] }
# or "runtime-async-std"
```

All other metrics work without a runtime feature.

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

### Operation metrics

| Instrument                         | Type      | Unit | Description                           |
|------------------------------------|-----------|------|---------------------------------------|
| `db.client.operation.duration`     | Histogram | `s`  | Duration of each database operation   |
| `db.client.response.returned_rows` | Histogram |      | Number of rows returned per operation |

These carry the connection-level attributes (`db.system.name`, `db.namespace`, `server.address`, `server.port`).

### Connection pool metrics

| Instrument                                | Type            | Unit | Description                                           |
|-------------------------------------------|-----------------|------|-------------------------------------------------------|
| `db.client.connection.wait_time`          | Histogram       | `s`  | Time spent waiting for a connection in `acquire()`    |
| `db.client.connection.use_time`           | Histogram       | `s`  | Time a connection was held before being returned      |
| `db.client.connection.timeouts`           | Counter         |      | Number of acquire attempts that timed out             |
| `db.client.connection.pending_requests`   | UpDownCounter   |      | Number of callers currently waiting in `acquire()`    |
| `db.client.connection.count`              | Gauge           |      | Current connections by state (`idle`/`used`)          |
| `db.client.connection.max`                | Gauge           |      | Maximum number of connections allowed                 |
| `db.client.connection.idle.max`           | Gauge           |      | Maximum idle connections (equals `max` in SQLx)       |
| `db.client.connection.idle.min`           | Gauge           |      | Configured minimum connections                        |

The first four are recorded inline on every `acquire()` / connection drop – no sampling gaps. `connection.count` is polled by a background task and requires a runtime feature (`runtime-tokio` or `runtime-async-std`). The remaining three are static gauges recorded once at pool construction.

## Configuration

`PoolBuilder` supports overriding auto-extracted attributes and controlling query text capture:

```rust
use sqlx_otel::{PoolBuilder, QueryTextMode};
use std::time::Duration;

let pool = PoolBuilder::from(raw_pool)
    .with_database("mydb")
    .with_host("db.example.com")
    .with_port(5432)
    .with_network_peer_address("10.0.0.5")
    .with_network_peer_port(5432)
    .with_query_text_mode(QueryTextMode::Off)
    .with_pool_name("my-service-db")
    .with_pool_metrics_interval(Duration::from_secs(5))
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
