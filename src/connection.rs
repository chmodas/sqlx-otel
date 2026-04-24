use crate::pool::SharedState;

/// A pooled connection instrumented for OpenTelemetry.
///
/// Acquired via [`Pool::acquire()`](crate::Pool::acquire). Implements `sqlx::Executor` so
/// it can be used directly with `SQLx` query builders.
#[derive(Debug)]
pub struct PoolConnection<DB: sqlx::Database> {
    pub(crate) inner: sqlx::pool::PoolConnection<DB>,
    pub(crate) state: SharedState,
}
