//! Lightweight [`SQLx`](https://docs.rs/sqlx) wrapper that emits OpenTelemetry-native spans
//! and metrics following the [database client semantic conventions][semconv].
//!
//! `sqlx-otel` talks to the [`opentelemetry`] API directly – there is no `tracing` bridge
//! indirection. The wrapper is **zero-cost when no `TracerProvider` or `MeterProvider` is
//! installed** because the global `opentelemetry` API resolves to no-op instruments in that
//! configuration; the only overhead is one `Arc` clone per acquired connection or transaction.
//!
//! [semconv]: https://opentelemetry.io/docs/specs/semconv/database/
//!
//! # Quick start
//!
//! Wrap an existing `sqlx::Pool` with [`PoolBuilder`] and use the result anywhere a `sqlx::Pool`
//! is accepted – every operation through the wrapper is instrumented.
//!
//! ```no_run
//! # #[cfg(feature = "sqlite")]
//! # async fn _doc() -> Result<(), sqlx::Error> {
//! use sqlx_otel::PoolBuilder;
//!
//! // Wrap an existing sqlx pool. Connection-level attributes (host, port, db namespace) are
//! // auto-extracted from the underlying connect options.
//! let raw = sqlx::SqlitePool::connect(":memory:").await?;
//! let pool = PoolBuilder::from(raw).build();
//!
//! // Use it exactly like a sqlx pool – `&pool` is an `sqlx::Executor`.
//! let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await?;
//! assert_eq!(row.0, 1);
//!
//! // Transactions work via `&mut tx` (note: not `&mut *tx`; the wrapper does not deref).
//! let mut tx = pool.begin().await?;
//! sqlx::query("CREATE TABLE users (name TEXT)")
//!     .execute(&mut tx)
//!     .await?;
//! tx.commit().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Every operation through the pool emits a [`SpanKind::Client`][SpanKind] span, records the
//! operation duration, and tracks pool-level metrics. No code changes are required beyond the
//! one-line wrap.
//!
//! [SpanKind]: https://docs.rs/opentelemetry/latest/opentelemetry/trace/enum.SpanKind.html
//!
//! # Setting up an OpenTelemetry SDK
//!
//! `sqlx-otel` produces telemetry but **does not install a provider** – that is the application's
//! responsibility. Pair this crate with the [`opentelemetry_sdk`] (or any other compliant SDK) and
//! set up exporters for your traces and metrics. Until a provider is installed via
//! [`opentelemetry::global::set_tracer_provider`] / [`set_meter_provider`][m], the instrumentation
//! is a no-op.
//!
//! [`opentelemetry_sdk`]: https://docs.rs/opentelemetry_sdk
//! [m]: https://docs.rs/opentelemetry/latest/opentelemetry/global/fn.set_meter_provider.html
//!
//! # What gets emitted
//!
//! ## Spans
//!
//! Every [`sqlx::Executor`] method (`execute`, `fetch`, `fetch_all`, `fetch_one`, `fetch_optional`,
//! `fetch_many`, `execute_many`, `prepare`, `prepare_with`, `describe`) produces a
//! `SpanKind::Client` span. The span name follows the [semantic-convention hierarchy][naming]:
//! `db.query.summary` (when set) → `"{operation} {collection}"` → `"{operation}"` → `"{db.system.name}"`.
//!
//! [naming]: https://opentelemetry.io/docs/specs/semconv/database/database-spans/#name
//!
//! | Attribute                   | Source                                                   | Condition                   |
//! |-----------------------------|----------------------------------------------------------|-----------------------------|
//! | `db.system.name`            | Backend (`"postgresql"`, `"sqlite"`, `"mysql"`)          | Always                      |
//! | `db.namespace`              | Database name extracted from connect options             | When available              |
//! | `server.address`            | Hostname extracted from connect options                  | When available              |
//! | `server.port`               | Port extracted from connect options                      | When available              |
//! | `network.peer.address`      | Resolved IP address                                      | When set via builder        |
//! | `network.peer.port`         | Resolved port                                            | When set via builder        |
//! | `db.query.text`             | The SQL query string with inter-token whitespace collapsed | Unless [`QueryTextMode::Off`] |
//! | `db.operation.name`         | Database operation (e.g. `SELECT`)                       | When [annotated]            |
//! | `db.collection.name`        | Target table or collection                               | When [annotated]            |
//! | `db.query.summary`          | Low-cardinality query summary                            | When [annotated]            |
//! | `db.stored_procedure.name`  | Stored procedure name                                    | When [annotated]            |
//! | `db.response.returned_rows` | Row count                                                | On `fetch*` methods         |
//! | `db.response.affected_rows` | Rows affected (`QueryResult::rows_affected()`)           | On `execute` ([note](#a-note-on-dbresponseaffected_rows)) |
//! | `db.response.status_code`   | SQLSTATE / driver error code                             | On database errors          |
//! | `error.type`                | Error variant name                                       | On any error                |
//!
//! [annotated]: #per-query-annotations
//!
//! On error, the span status is set to `Error` and an `exception` event is added carrying
//! `exception.type` and `exception.message`.
//!
//! ## Operation metrics
//!
//! | Instrument                         | Type      | Unit | Description                           |
//! |------------------------------------|-----------|------|---------------------------------------|
//! | `db.client.operation.duration`     | Histogram | `s`  | Duration of each database operation   |
//! | `db.client.response.returned_rows` | Histogram |      | Number of rows returned per operation |
//!
//! These carry the connection-level attributes (`db.system.name`, `db.namespace`, `server.address`,
//! `server.port`).
//!
//! ## Connection-pool metrics
//!
//! | Instrument                              | Type            | Unit | Description                                          |
//! |-----------------------------------------|-----------------|------|------------------------------------------------------|
//! | `db.client.connection.wait_time`        | Histogram       | `s`  | Time spent waiting for a connection in `acquire()`   |
//! | `db.client.connection.use_time`         | Histogram       | `s`  | Time a connection was held before being returned     |
//! | `db.client.connection.timeouts`         | Counter         |      | Number of acquire attempts that timed out            |
//! | `db.client.connection.pending_requests` | `UpDownCounter` |      | Number of callers currently waiting in `acquire()`   |
//! | `db.client.connection.count`            | Gauge           |      | Current connections by state (`idle` / `used`)       |
//! | `db.client.connection.max`              | Gauge           |      | Maximum number of connections allowed                |
//! | `db.client.connection.idle.max`         | Gauge           |      | Maximum idle connections (equals `max` in `SQLx`)    |
//! | `db.client.connection.idle.min`         | Gauge           |      | Configured minimum connections                       |
//!
//! The first four are recorded inline on every `acquire()` and connection drop – no sampling
//! gaps. `connection.count` is polled by a background task and requires both
//! [`PoolBuilder::with_pool_name`] and a runtime feature (`runtime-tokio` or
//! `runtime-async-std`); without either, the gauge is silent. The remaining three are static
//! gauges recorded once at [`PoolBuilder::build`].
//!
//! ## A note on `db.response.affected_rows`
//!
//! `db.response.affected_rows` is **not part of the OpenTelemetry semantic conventions** at
//! the time of writing; it is a custom attribute we find useful. It carries the
//! database-confirmed count from `QueryResult::rows_affected()` on every `execute()` call,
//! using the same connection-level attributes as the standard `db.response.returned_rows`.
//! It is not recorded for `execute_many`, which is [considered deprecated upstream][exec-many].
//!
//! [exec-many]: https://github.com/launchbadge/sqlx/issues/3108
//!
//! # Per-query annotations
//!
//! `sqlx-otel` does **not** parse SQL. The four per-query semantic-convention attributes –
//! `db.operation.name`, `db.collection.name`, `db.query.summary`, and
//! `db.stored_procedure.name` – are the caller's responsibility, supplied through the
//! annotation API. There are two equivalent surfaces depending on whether you prefer the
//! annotation to live next to the executor or next to the query:
//!
//! **Executor-side** ([`Pool::with_annotations`], [`PoolConnection::with_annotations`],
//! [`Transaction::with_annotations`]) returns a borrowed wrapper that is itself an
//! `sqlx::Executor`. Use it when the same query text is reused with one set of annotations,
//! or when you want to annotate `prepare` / `describe` flows.
//!
//! ```no_run
//! # #[cfg(feature = "sqlite")]
//! # async fn _doc() -> Result<(), sqlx::Error> {
//! # use sqlx_otel::PoolBuilder;
//! use sqlx::Executor as _; // brings `fetch_all` / `execute` into scope.
//! use sqlx_otel::QueryAnnotations;
//! # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
//!
//! pool.with_annotations(
//!     QueryAnnotations::new()
//!         .operation("SELECT")
//!         .collection("users"),
//! )
//! .fetch_all("SELECT * FROM users")
//! .await?;
//!
//! // Shorthand for the common two-attribute case.
//! pool.with_operation("INSERT", "orders")
//!     .execute("INSERT INTO orders (id) VALUES (1)")
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! **Query-side** ([`QueryAnnotateExt`]) attaches the annotation directly to the
//! `sqlx::query` / `sqlx::query_as` / `sqlx::query_scalar` builder. Use it when annotations
//! belong with the query text – the typical case. Works with `Query::map` /
//! `Query::try_map` and with the compile-time validated macro forms.
//!
//! ```no_run
//! # #[cfg(feature = "sqlite")]
//! # async fn _doc() -> Result<(), sqlx::Error> {
//! # use sqlx_otel::PoolBuilder;
//! use sqlx_otel::QueryAnnotateExt;
//! # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
//!
//! sqlx::query("INSERT INTO orders (user_id) VALUES (?)")
//!     .bind(7_i64)
//!     .with_operation("INSERT", "orders")
//!     .execute(&pool)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! See [`QueryAnnotations`] for the full set of fields and [`QueryAnnotateExt`] for the
//! query-side surface (including the three valid annotation positions on `Query::map`
//! chains).
//!
//! # Error handling
//!
//! All wrapper methods return `sqlx::Error` unchanged – there is no error wrapping or
//! retranslation. When an error surfaces, the active span's status is set to `Error`, an
//! `exception` event is added with `exception.type` and `exception.message`, and –
//! whenever the error is a [`sqlx::Error::Database`] with a SQLSTATE/driver code – the
//! `db.response.status_code` attribute is recorded. The instrumentation is otherwise
//! transparent: the caller sees exactly the same `Result` it would see without the wrapper.
//!
//! # Feature flags
//!
//! `sqlx-otel` ships with **no default features** – pick at least one backend.
//!
//! | Feature             | Effect                                                              |
//! |---------------------|---------------------------------------------------------------------|
//! | `sqlite`            | Enable the `sqlx::Sqlite` backend.                                  |
//! | `postgres`          | Enable the `sqlx::Postgres` backend.                                |
//! | `mysql`             | Enable the `sqlx::MySql` backend.                                   |
//! | `runtime-tokio`     | Spawn the `db.client.connection.count` polling task with `tokio`.   |
//! | `runtime-async-std` | Same as above, but with `async-std`. Ignored if `tokio` is enabled. |
//!
//! ```toml
//! [dependencies]
//! sqlx-otel = { version = "0.1", features = ["postgres", "runtime-tokio"] }
//! ```
//!
//! All operation- and pool-level metrics other than `db.client.connection.count` work
//! without a runtime feature.
//!
//! # MSRV
//!
//! Minimum supported Rust version: **1.85.0**.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
mod annotations;
pub(crate) mod attributes;
mod compact;
mod connection;
mod database;
mod executor;
mod metrics;
mod obfuscate;
mod pool;
mod pool_metrics;
mod query_ext;
mod runtime;
mod transaction;

pub use annotations::{Annotated, AnnotatedMut, QueryAnnotations};
pub use attributes::QueryTextMode;
pub use connection::PoolConnection;
pub use database::Database;
pub use pool::{Pool, PoolBuilder};
pub use query_ext::{AnnotatedQuery, QueryAnnotateExt};
pub use transaction::Transaction;
