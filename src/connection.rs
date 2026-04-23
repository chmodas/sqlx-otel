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

/// A borrowed connection reference instrumented for OpenTelemetry.
///
/// Obtained from [`Transaction::executor()`](crate::Transaction::executor). This type
/// exists so that queries can be executed within a transaction while still getting full
/// span and metric instrumentation.
pub struct Connection<'c, DB: sqlx::Database> {
    pub(crate) inner: &'c mut DB::Connection,
    pub(crate) state: SharedState,
}

impl<DB: sqlx::Database> std::fmt::Debug for Connection<'_, DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}
