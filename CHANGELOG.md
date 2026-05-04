# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `db.client.response.affected_rows` histogram – a custom metric (no OTel semconv equivalent) mirroring the existing `db.response.affected_rows` span attribute. Recorded on `execute()` calls; carries the same connection / annotation / error attribute set as the duration histogram so dashboards can slice mutation throughput by the same dimensions ([#33](https://github.com/chmodas/sqlx-otel/pull/33)).
- `PoolBuilder::with_network_protocol_name` and `with_network_transport` builder methods, plus a per-backend `Database::DEFAULT_NETWORK_PROTOCOL_NAME` constant (Postgres → `"postgresql"`, MySQL → `"mysql"`, SQLite → `None`). `network.protocol.name`, `network.transport`, and `db.client.connection.pool.name` now surface on every span and per-operation metric data point so dashboards can slice query latency by the same dimensions OTel's database-spans semconv recommends ([#32](https://github.com/chmodas/sqlx-otel/pull/32)).

### Changed

- `db.client.connection.pool.name` previously appeared only on the `db.client.connection.count` gauge; it now also propagates to spans, the `db.client.operation.duration` / `db.client.response.returned_rows` histograms, and the rest of the `db.client.connection.*` family via the shared connection-attribute set. The `count` gauge's attribute set is unchanged ([#32](https://github.com/chmodas/sqlx-otel/pull/32)).

### Fixed

- Query-side `with_annotations` / `with_operation` now compile inside `Send`-required async contexts (axum handlers, `tokio::spawn`, `tower::Service`-bounded futures). The `AnnotatedQuery::fetch_*` / `execute` forwarders were rewritten from `async fn` into `fn -> impl Future + Send + 'e`, so the HRTB carried by the internal `IntoAnnotatedExecutor` impls no longer leaks into auto-trait inference of an opaque coroutine. Span output is byte-identical to the executor-side surface; metrics emission is unchanged by construction because both surfaces continue to funnel through the same `Annotated<'_, Pool<DB>>` `Executor` impl ([#31](https://github.com/chmodas/sqlx-otel/pull/31)).
- `db.client.operation.duration` histogram now carries annotation-derived attributes (`db.operation.name`, `db.collection.name`, `db.query.summary`, `db.stored_procedure.name`) and error-path attributes (`error.type`, plus `db.response.status_code` for `sqlx::Error::Database`). Previously only connection-level attributes (`db.system.name`, `db.namespace`) reached the histogram, so dashboards could not slice DB latency by operation verb, target collection, or error class. The `InstrumentedStream` poll loop also gains a single-error latch so streams that yield multiple `Err` items before terminating cannot append duplicate `error.type` / `db.response.status_code` keys ([#32](https://github.com/chmodas/sqlx-otel/pull/32)).

## [0.2.0] – 2026-04-28

### Added

- Query-side `with_annotations` / `with_operation` via the new `QueryAnnotateExt` trait on `sqlx::query`, `sqlx::query_as`, and `sqlx::query_scalar`, so per-query attributes can sit next to the query text. `bind` and `with_annotations` compose in either order ([#22](https://github.com/chmodas/sqlx-otel/pull/22)).
- `Query::map` / `Query::try_map` chains and the `sqlx::query!()` / `query_as!()` / `query_scalar!()` macros now support `with_annotations` / `with_operation`. On hand-written chains the annotation may sit before `bind`, between `bind` and `map`, or after `map`; macro queries carry annotations after the macro returns ([#23](https://github.com/chmodas/sqlx-otel/pull/23)).

### Changed

- **BREAKING:** Sealed the `Database` trait. Only `sqlx::Sqlite`, `sqlx::Postgres`, and `sqlx::MySql` can implement it; downstream crates cannot. In practice the trait was already constrained to these three backends because it requires an upstream `sqlx::Database` impl, but the bound is now enforced at the type level.

## [0.1.0] – 2026-04-26

### Added

- Span name follows the OpenTelemetry [Database Spans § Name](https://opentelemetry.io/docs/specs/semconv/database/database-spans/) hierarchy in full: `db.query.summary` (set via `QueryAnnotations::query_summary()`) takes precedence over the `{operation} {collection}` synthesis ([#18](https://github.com/chmodas/sqlx-otel/pull/18)).
- Obfuscation mode for `db.query.text`: `QueryTextMode::Obfuscated` now replaces string, numeric, hex, boolean, and dollar-quoted literals with `?` while preserving comments, identifiers, operators, and `NULL` ([#16](https://github.com/chmodas/sqlx-otel/pull/16)).
- Per-query annotation API for the semantic convention attributes the library cannot derive from SQL: `db.operation.name`, `db.collection.name`, `db.query.summary`, `db.stored_procedure.name` ([#15](https://github.com/chmodas/sqlx-otel/pull/15)).
- `db.response.affected_rows` recorded on `execute()` spans ([#13](https://github.com/chmodas/sqlx-otel/pull/13)).
- Connection pool metrics (`db.client.connection.count`, `db.client.connection.idle.max`, etc.) with a runtime abstraction over `tokio` and `async-std` ([#12](https://github.com/chmodas/sqlx-otel/pull/12)).
- MySQL backend ([#6](https://github.com/chmodas/sqlx-otel/pull/6)).
- PostgreSQL backend ([#4](https://github.com/chmodas/sqlx-otel/pull/4)).
- SQLite backend ([#3](https://github.com/chmodas/sqlx-otel/pull/3)).
- Backend-agnostic instrumentation core: `Pool`, `PoolBuilder`, `Transaction`, `PoolConnection`, and the `Executor` trait wiring that emits OpenTelemetry-native spans and metrics following the [database calls and systems](https://opentelemetry.io/docs/specs/semconv/db/) semantic conventions ([#2](https://github.com/chmodas/sqlx-otel/pull/2)).

[Unreleased]: https://github.com/chmodas/sqlx-otel/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/chmodas/sqlx-otel/releases/tag/v0.1.0
[0.2.0]: https://github.com/chmodas/sqlx-otel/releases/tag/v0.2.0
