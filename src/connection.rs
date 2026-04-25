use std::sync::Arc;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;

use crate::database::Database;
use crate::pool::SharedState;

/// A pooled connection instrumented for OpenTelemetry.
///
/// Acquired via [`Pool::acquire()`](crate::Pool::acquire). Implements `sqlx::Executor` so
/// it can be used directly with `SQLx` query builders.
///
/// When dropped, records `db.client.connection.use_time` – the time between acquiring the
/// connection and returning it to the pool.
pub struct PoolConnection<DB: sqlx::Database> {
    pub(crate) inner: sqlx::pool::PoolConnection<DB>,
    pub(crate) state: SharedState,
    pub(crate) use_time: Arc<Histogram<f64>>,
    pub(crate) acquired_at: Instant,
    pub(crate) base_attrs: Vec<KeyValue>,
}

impl<DB: sqlx::Database> Drop for PoolConnection<DB> {
    fn drop(&mut self) {
        self.use_time
            .record(self.acquired_at.elapsed().as_secs_f64(), &self.base_attrs);
    }
}

impl<DB: sqlx::Database> std::fmt::Debug for PoolConnection<DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolConnection").finish_non_exhaustive()
    }
}

impl<DB: Database> PoolConnection<DB> {
    impl_with_annotations_mut!();
}
