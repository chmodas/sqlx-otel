use std::ops::{Deref, DerefMut};
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
    pub(crate) usage: ConnectionUsage,
}

/// Moves with an owned connection into a transaction, preserving the complete lease duration.
#[derive(Debug)]
pub(crate) struct ConnectionUsage {
    /// `None` once [`discard`](Self::discard)ed: the lease end is not observable.
    pub(crate) use_time: Option<Arc<Histogram<f64>>>,
    pub(crate) acquired_at: Instant,
    pub(crate) base_attrs: Vec<KeyValue>,
}

impl ConnectionUsage {
    /// Give up on recording this lease, for a connection handed to a type that does not report
    /// when it releases the connection.
    pub(crate) fn discard(mut self) {
        self.use_time = None;
    }
}

impl Drop for ConnectionUsage {
    fn drop(&mut self) {
        if let Some(use_time) = &self.use_time {
            use_time.record(self.acquired_at.elapsed().as_secs_f64(), &self.base_attrs);
        }
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

/// A pooled connection acquired through the [`sqlx::Acquire`] implementation on `&Pool`.
///
/// It exists so that `SQLx` APIs generic over [`sqlx::Acquire`] – such as
/// `sqlx::migrate::Migrator::run` – accept an instrumented [`Pool`](crate::Pool). It dereferences
/// to the backend connection, the type those APIs expect.
///
/// The acquisition is instrumented exactly like [`Pool::acquire`](crate::Pool::acquire)
/// (`db.client.connection.wait_time`, `pending_requests`, `timeouts`), and
/// `db.client.connection.use_time` is recorded when the value is dropped. Queries run on the
/// dereferenced connection are **not** instrumented: no spans, no
/// `db.client.operation.duration`. Use [`Pool::acquire`](crate::Pool::acquire) to run
/// instrumented queries on a dedicated connection.
pub struct AcquiredConnection<DB: sqlx::Database> {
    inner: sqlx::pool::PoolConnection<DB>,
    _usage: ConnectionUsage,
}

impl<DB: sqlx::Database> AcquiredConnection<DB> {
    pub(crate) fn new(connection: PoolConnection<DB>) -> Self {
        let PoolConnection { inner, usage, .. } = connection;
        Self {
            inner,
            _usage: usage,
        }
    }
}

impl<DB: sqlx::Database> std::fmt::Debug for AcquiredConnection<DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquiredConnection").finish_non_exhaustive()
    }
}

impl<DB: sqlx::Database> Deref for AcquiredConnection<DB> {
    type Target = DB::Connection;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<DB: sqlx::Database> DerefMut for AcquiredConnection<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
