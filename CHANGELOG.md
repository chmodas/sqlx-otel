# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
