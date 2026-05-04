# sqlx-otel

[![CI build](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml/badge.svg)](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/chmodas/sqlx-otel/graph/badge.svg?token=EFVNRZB3WN)](https://codecov.io/gh/chmodas/sqlx-otel)
[![crates.io](https://img.shields.io/crates/v/sqlx-otel.svg)](https://crates.io/crates/sqlx-otel)
[![docs.rs](https://img.shields.io/docsrs/sqlx-otel)](https://docs.rs/sqlx-otel)

Lightweight [SQLx](https://github.com/launchbadge/sqlx) wrapper that emits OpenTelemetry-native spans and metrics following the [database client semantic conventions](https://opentelemetry.io/docs/specs/semconv/database/).

The wrapper talks to the [`opentelemetry`](https://docs.rs/opentelemetry) API directly – there is no `tracing` bridge in the path. That means a smaller dependency tree, attributes set per the spec rather than translated through a second model, and zero cost when no `TracerProvider` or `MeterProvider` is installed.

> Full API documentation, including runnable examples for every public type, is on [docs.rs/sqlx-otel](https://docs.rs/sqlx-otel).

## Highlights

- **Spans on every operation.** Every `sqlx::Executor` method emits a `SpanKind::Client` span carrying the OTel database-spans recommended set: `db.system.name`, `db.namespace`, `server.address`/`port`, `network.peer.address`/`port`, `network.protocol.name`, `network.transport`, `db.client.connection.pool.name`, `db.query.text`, returned/affected row counts, and SQLSTATE / `error.type` on failure.
- **Span/metric attribute parity.** The `db.client.operation.duration` histogram carries the same bounded attribute set as spans – every annotation, every error-path attribute, every connection-level attribute – so dashboards can slice query latency by operation verb, target collection, error class, or pool name. `db.query.text` is the only span attribute deliberately excluded from metrics for cardinality.
- **Caller-supplied annotations.** The library does not parse SQL. Per-query attributes (`db.operation.name`, `db.collection.name`, `db.query.summary`, `db.stored_procedure.name`) come from a small annotation API.
- **Three backends, one API.** Postgres, SQLite, and MySQL behind feature flags; the wrapper API is identical across all three.
- **Drop-in.** `&Pool<DB>` implements `sqlx::Executor`, so existing call sites keep working unchanged.

## Quick start

| Feature flag                            | Purpose                                                                  |
|-----------------------------------------|--------------------------------------------------------------------------|
| `sqlite` / `postgres` / `mysql`         | Per-backend support – enable at least one.                               |
| `runtime-tokio` / `runtime-async-std`   | Background polling for `db.client.connection.count`. Optional.           |

```toml
[dependencies]
sqlx-otel = { version = "0.2.0", features = ["postgres", "runtime-tokio"] }
```

```rust
use sqlx_otel::PoolBuilder;

// Wrap an existing sqlx pool. Connection-level attributes (host, port, namespace)
// are auto-extracted from the underlying connect options.
let raw = sqlx::PgPool::connect("postgres://localhost/mydb").await?;
let pool = PoolBuilder::from(raw).build();

// Use it exactly like a sqlx pool.
let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await?;

// Transactions work via `&mut tx`.
let mut tx = pool.begin().await?;
sqlx::query("INSERT INTO users (name) VALUES ($1)")
    .bind("Alice")
    .execute(&mut tx)
    .await?;
tx.commit().await?;
```

The wrapper itself is a no-op until your application installs an OpenTelemetry `TracerProvider` and/or `MeterProvider`. Setting that up is the application's responsibility – pair this crate with the [`opentelemetry_sdk`](https://docs.rs/opentelemetry_sdk) (or any other compliant SDK) and your chosen exporter.

> **Annotations matter.** Without the per-query annotation API ([below](#per-query-annotations)), spans carry `db.system.name` and `db.query.text` but no `db.operation.name` or `db.collection.name` – the most useful filtering attributes. Plan to annotate your queries.

For a complete runnable end-to-end example – pool wrap, in-memory SDK, queries, and printed telemetry – see [`examples/sqlite.rs`](examples/sqlite.rs):

```bash
cargo run --example sqlite --features sqlite,runtime-tokio
```

## Configuration

`PoolBuilder` overrides the auto-extracted attributes and tunes capture behaviour:

```rust
use sqlx_otel::{PoolBuilder, QueryTextMode};
use std::time::Duration;

let pool = PoolBuilder::from(raw_pool)
    .with_database("mydb")                  // override db.namespace
    .with_host("db.example.com")            // override server.address
    .with_port(5432)                        // override server.port
    .with_network_peer_address("10.0.0.5")  // network.peer.address (not auto-extracted)
    .with_network_peer_port(5432)           // network.peer.port    (not auto-extracted)
    .with_network_protocol_name("postgresql") // override network.protocol.name (defaulted per backend)
    .with_network_transport("tcp")          // network.transport    (not inferred from connect string)
    .with_query_text_mode(QueryTextMode::Off)
    .with_pool_name("my-service-db")        // db.client.connection.pool.name + connection-count polling
    .with_pool_metrics_interval(Duration::from_secs(5))
    .build();
```

`with_pool_name` populates `db.client.connection.pool.name` on every span and per-operation metric, and is the switch that activates `db.client.connection.count` polling – the gauge is silent until both a pool name and a runtime feature are present.

`network.protocol.name` defaults to the backend's wire protocol (`"postgresql"` for Postgres, `"mysql"` for MySQL, absent for SQLite); override only when the connection is tunnelled through a different application-layer protocol. `network.transport` is not inferred from the connect string – callers who want this attribute on spans / metrics must declare it explicitly so the value reflects the deployment configuration rather than a guess.

## Per-query annotations

Because the library does not parse SQL, per-query attributes are the caller's responsibility:

```rust
use sqlx_otel::{QueryAnnotateExt, QueryAnnotations};

sqlx::query("SELECT * FROM users WHERE id = ?")
    .bind(42_i64)
    .with_annotations(QueryAnnotations::new().operation("SELECT").collection("users"))
    .execute(&pool)
    .await?;

// Shorthand for the common operation/collection pair:
sqlx::query("INSERT INTO orders (user_id) VALUES (?)")
    .with_operation("INSERT", "orders")
    .bind(7_i64)
    .execute(&pool)
    .await?;
```

The same `with_annotations` / `with_operation` methods are available on:

- The query builders returned by `sqlx::query`, `sqlx::query_as`, and `sqlx::query_scalar` (via the `QueryAnnotateExt` trait).
- The `Map` returned by `query::map` / `query::try_map`, and the compile-time-validated macro forms (`sqlx::query!()`, `sqlx::query_as!()`, `sqlx::query_scalar!()`) – which expand to either `Query` or `Map`.
- `Pool`, `PoolConnection`, and `Transaction` directly, via `pool.with_annotations(…).fetch_all(…)`.

When annotations are set, the span name follows the [semantic-convention hierarchy](https://opentelemetry.io/docs/specs/semconv/database/database-spans/#name): `db.query.summary` → `"{operation} {collection}"` → `"{operation}"` → `"{db.system.name}"`.

| Attribute                  | Builder method        |
|----------------------------|-----------------------|
| `db.operation.name`        | `.operation()`        |
| `db.collection.name`       | `.collection()`       |
| `db.query.summary`         | `.query_summary()`    |
| `db.stored_procedure.name` | `.stored_procedure()` |

See [`QueryAnnotations`](https://docs.rs/sqlx-otel/latest/sqlx_otel/struct.QueryAnnotations.html) and [`QueryAnnotateExt`](https://docs.rs/sqlx-otel/latest/sqlx_otel/trait.QueryAnnotateExt.html) on docs.rs for the full surface and worked examples.

## Query text modes

| Mode             | Behaviour                                                                                          |
|------------------|----------------------------------------------------------------------------------------------------|
| `Full` (default) | Capture the parameterised query as-is. Safe because SQLx uses bind parameters.                     |
| `Obfuscated`     | Replace literal values (string, numeric, hex, boolean, dollar-quoted) with `?` in `db.query.text`. |
| `Off`            | Do not capture `db.query.text`.                                                                    |

`Obfuscated` is useful when SQL is constructed via string interpolation rather than bind parameters – the structure of the query is preserved while sensitive literal values are redacted. Comments, identifiers (quoted or otherwise), operators, and `NULL` are kept verbatim.

## Reference

### Span attributes

Set on every `Executor` method (`execute`, `fetch`, `fetch_all`, `fetch_one`, `fetch_optional`, `fetch_many`, `execute_many`, `prepare`, `prepare_with`, `describe`):

| Attribute                        | Source                                                      | Condition                   |
|----------------------------------|-------------------------------------------------------------|-----------------------------|
| `db.system.name`                 | Backend (`"postgresql"`, `"sqlite"`, `"mysql"`)             | Always                      |
| `db.namespace`                   | Database name, extracted from connect options               | When available              |
| `server.address`                 | Hostname, extracted from connect options                    | When available              |
| `server.port`                    | Port, extracted from connect options                        | When available              |
| `network.peer.address`           | Resolved IP address                                         | When set via builder        |
| `network.peer.port`              | Resolved port                                               | When set via builder        |
| `network.protocol.name`          | Wire protocol; defaults per backend, overridable on builder | When applicable             |
| `network.transport`              | OSI L4 transport (`"tcp"`, `"unix"`, `"pipe"`, `"inproc"`)  | When set via builder        |
| `db.client.connection.pool.name` | Pool identifier set via `with_pool_name`                    | When set via builder        |
| `db.query.text`                  | The SQL query string                                        | Unless `QueryTextMode::Off` |
| `db.operation.name`              | Database operation (e.g. `SELECT`)                          | When annotated              |
| `db.collection.name`             | Target table or collection                                  | When annotated              |
| `db.query.summary`               | Low-cardinality query summary                               | When annotated              |
| `db.stored_procedure.name`       | Stored procedure name                                       | When annotated              |
| `db.response.returned_rows`      | Row count                                                   | On `fetch*` methods         |
| `db.response.affected_rows`      | Rows affected (`rows_affected()`)                           | On `execute`                |
| `db.response.status_code`        | SQLSTATE error code                                         | On database errors          |
| `error.type`                     | Error variant name                                          | On any error                |

`db.response.affected_rows` is not part of the OpenTelemetry semantic conventions, but we find it useful so have included it. It is a custom attribute that reports the database-confirmed count from `QueryResult::rows_affected()`, carrying the same connection-level attributes as `db.response.returned_rows`. It is not recorded for `execute_many`, which is [considered deprecated by the SQLx team](https://github.com/launchbadge/sqlx/issues/3108).

On error, the span status is set to `Error` and an `exception` event is added with `exception.type` and `exception.message` attributes.

### Operation metrics

| Instrument                         | Type      | Unit | Description                           |
|------------------------------------|-----------|------|---------------------------------------|
| `db.client.operation.duration`     | Histogram | `s`  | Duration of each database operation   |
| `db.client.response.returned_rows` | Histogram |      | Number of rows returned per operation |

These mirror the bounded portion of the span attribute set: connection-level attributes (`db.system.name`, `db.namespace`, `server.address`/`port`, `network.peer.address`/`port`, `network.protocol.name`, `network.transport`, `db.client.connection.pool.name` – wherever set), plus annotation-derived attributes (`db.operation.name`, `db.collection.name`, `db.query.summary`, `db.stored_procedure.name`) when present, plus error-path attributes (`error.type`, plus `db.response.status_code` for `sqlx::Error::Database`) on the error path. `db.query.text` is deliberately excluded for cardinality; `db.query.summary` is caller-controlled and inherits its cardinality cost from the span side.

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

The first four are recorded inline on every `acquire()` / connection drop – no sampling gaps. `db.client.connection.count` is polled by a background task and requires both a runtime feature (`runtime-tokio` or `runtime-async-std`) and a pool name set via `PoolBuilder::with_pool_name`. The remaining three are static gauges recorded once at pool construction.

## Backend support

| Backend    | `db.namespace`              | `server.address` / `server.port` | Default `network.protocol.name`     | Notes                            |
|------------|-----------------------------|----------------------------------|-------------------------------------|----------------------------------|
| `postgres` | Database name               | Yes (from connect URL)           | `"postgresql"`                      | –                                |
| `mysql`    | Database name               | Yes (from connect URL)           | `"mysql"`                           | –                                |
| `sqlite`   | File path or `:memory:` URI | n/a                              | absent (embedded; no wire protocol) | No host/port; file or in-memory. |

`db.response.returned_rows`, `db.response.affected_rows`, `db.response.status_code`, and `error.type` are recorded uniformly across all three backends.

## Compatibility

- **MSRV:** Rust **1.85.0**.
- **SQLx:** `0.8.x`.
- **OpenTelemetry:** `0.31.x`.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
