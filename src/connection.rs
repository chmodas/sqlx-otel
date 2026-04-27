use std::sync::Arc;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;

use crate::database::Database;
use crate::pool::SharedState;

/// A pooled connection instrumented for OpenTelemetry.
///
/// Acquired via [`Pool::acquire`](crate::Pool::acquire). `&mut PoolConnection<DB>` implements
/// [`sqlx::Executor`], so the value plugs straight into the standard `SQLx` query builders. When
/// the value is dropped, the connection is returned to the pool and `db.client.connection.use_time`
/// is recorded with the elapsed time between `acquire` and `drop`.
///
/// Use [`with_annotations`](Self::with_annotations) / [`with_operation`](Self::with_operation) to
/// attach per-query semantic-convention attributes – the methods are inherited via the same macro
/// that powers the equivalents on [`Pool`](crate::Pool) and [`Transaction`](crate::Transaction).
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// # use sqlx_otel::PoolBuilder;
/// # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
/// let mut conn = pool.acquire().await?;
/// let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&mut conn).await?;
/// assert_eq!(row.0, 1);
/// # Ok(())
/// # }
/// ```
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
