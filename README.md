# sqlx-otel

[![CI build](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml/badge.svg)](https://github.com/chmodas/sqlx-otel/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/chmodas/sqlx-otel/graph/badge.svg?token=EFVNRZB3WN)](https://codecov.io/gh/chmodas/sqlx-otel)

Lightweight [SQLx](https://github.com/launchbadge/sqlx) wrapper that emits OpenTelemetry-native spans and metrics following the [database client semantic conventions](https://opentelemetry.io/docs/specs/semconv/database/).

## Status

Under development – not yet ready for use.

## Planned features

- Database client spans with semconv-compliant naming and attributes
- `db.client.operation.duration` histogram and other operation/connection pool metrics
- Exception events (`db.client.operation.exception`)
- SQL query text capture with configurable sanitisation
- Feature-gated backends (`sqlite`, `postgres`, `mysql`)
- Zero-cost when no tracer/meter provider is installed

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
