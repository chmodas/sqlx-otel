# sqlx-otel

[![CI build](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml/badge.svg)](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/chmodas/sqlx-otel/graph/badge.svg?token=EFVNRZB3WN)](https://codecov.io/gh/chmodas/sqlx-otel)
[![crates.io](https://img.shields.io/crates/v/sqlx-otel.svg)](https://crates.io/crates/sqlx-otel)
[![docs.rs](https://img.shields.io/docsrs/sqlx-otel)](https://docs.rs/sqlx-otel)

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
sqlx-otel = { version = "0.1.0", features = ["postgres"] }
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
| `db.operation.name`         | Database operation (e.g. `SELECT`)              | When annotated              |
| `db.collection.name`        | Target table or collection                      | When annotated              |
| `db.query.summary`          | Low-cardinality query summary                   | When annotated              |
| `db.stored_procedure.name`  | Stored procedure name                           | When annotated              |
| `db.response.returned_rows` | Row count                                       | On `fetch*` methods         |
| `db.response.affected_rows` | Rows affected (`rows_affected()`)               | On `execute`                |
| `db.response.status_code`   | SQLSTATE error code                             | On database errors          |
| `error.type`                | Error variant name                              | On any error                |

`db.response.affected_rows` is not part of the OpenTelemetry semantic conventions but we find it useful so have included it. It is a custom attribute that reports the database-confirmed count from `QueryResult::rows_affected()`, carrying the same connection-level attributes as `db.response.returned_rows`. It is not recorded for `execute_many`, which is [considered deprecated by the SQLx team](https://github.com/launchbadge/sqlx/issues/3108).

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

## Per-query annotations

The library does not parse SQL. Per-query attributes like the operation name and target table are the caller's responsibility via the annotation API:

```rust
use sqlx_otel::QueryAnnotations;

// Full builder – set whichever fields apply.
pool.with_annotations(
        QueryAnnotations::new()
            .operation("SELECT")
            .collection("users"))
    .fetch_all("SELECT * FROM users WHERE active = true")
    .await?;

// Shorthand for the common two-attribute case.
pool.with_operation("INSERT", "orders")
    .execute("INSERT INTO orders (id) VALUES ($1)")
    .await?;
```

Annotations work on `Pool`, `PoolConnection`, and `Transaction`. The wrapper borrows the underlying executor for a single operation and is then dropped.

### Query-side annotations

For locality, the same `with_annotations` / `with_operation` methods are also available on the query builder produced by `sqlx::query`, `sqlx::query_as`, and `sqlx::query_scalar`. Bring `QueryAnnotateExt` into scope and chain the annotation directly on the query – binds may appear before or after `with_annotations`:

```rust
use sqlx_otel::{QueryAnnotateExt, QueryAnnotations};

sqlx::query("SELECT * FROM users WHERE id = ?")
    .bind(42_i64)
    .with_annotations(QueryAnnotations::new().operation("SELECT").collection("users"))
    .execute(&pool)
    .await?;

// Shorthand and bind-after-annotate also work:
sqlx::query("INSERT INTO orders (user_id) VALUES (?)")
    .with_operation("INSERT", "orders")
    .bind(7_i64)
    .execute(&pool)
    .await?;
```

#### Limitations

- `Query::map` / `Query::try_map` returns [`sqlx::query::Map<'q, DB, F, A>`](https://docs.rs/sqlx/0.8/sqlx/query/struct.Map.html), which is **not yet** covered by the trait. Apply `with_annotations` *before* `map` / `try_map` if you need both. Support for `Map` is planned.
- The compile-time validated macro forms expand to one of two public types and inherit support accordingly:
    - `sqlx::query!("INSERT/UPDATE/DELETE …")` (no result columns) expands to `Query<'q, DB, _>` and **works today**.
    - `sqlx::query!("SELECT …")`, `sqlx::query_as!()`, and `sqlx::query_scalar!()` expand to `Map<'q, DB, _, _>` and are **not yet** annotatable on the query side. For now, use the executor-side `pool.with_annotations(...)` form with these macros, or apply annotations to a hand-written `sqlx::query_as` builder.

When annotations are provided the span name follows the [semantic convention hierarchy](https://opentelemetry.io/docs/specs/semconv/database/database-spans/#name):

1. `db.query.summary` – the caller-supplied summary, e.g. `"users by tenant"`
2. `"{db.operation.name} {db.collection.name}"` – e.g. `"SELECT users"`
3. `"{db.operation.name}"` – e.g. `"INSERT"`
4. `"{db.system.name}"` – fallback when no annotations are set

`db.query.summary` wins unconditionally when set – this is the spec's escape hatch for callers who cannot guarantee a low-cardinality `db.operation.name` (dynamic SQL, complex pipelines).

| Attribute                   | Builder method      |
|-----------------------------|---------------------|
| `db.operation.name`         | `.operation()`      |
| `db.collection.name`        | `.collection()`     |
| `db.query.summary`          | `.query_summary()`  |
| `db.stored_procedure.name`  | `.stored_procedure()` |

### Query text modes

| Mode             | Behaviour                                                                                          |
|------------------|----------------------------------------------------------------------------------------------------|
| `Full` (default) | Capture the parameterised query as-is. Safe because SQLx uses bind parameters.                     |
| `Obfuscated`     | Replace literal values (string, numeric, hex, boolean, dollar-quoted) with `?` in `db.query.text`. |
| `Off`            | Do not capture `db.query.text`.                                                                    |

`Obfuscated` is useful when SQL is constructed via string interpolation rather than bind parameters – the structure of the query is preserved while sensitive literal values are redacted. Comments, identifiers (quoted or otherwise), operators, and `NULL` are kept verbatim.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
