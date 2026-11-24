use crate::database::Database;
use crate::pool::SharedState;
use crate::transaction_metrics::{TransactionLifecycle, TransactionTermination};

/// An in-progress database transaction instrumented for OpenTelemetry.
///
/// Wraps a `sqlx::Transaction` and propagates the connection-level attributes and metric
/// instruments. Pass `&mut transaction` directly to `SQLx` query builders – the
/// `sqlx::Executor` impl on `&mut Transaction` produces the same per-operation spans and
/// metrics as the parent pool.
///
/// Started by [`Pool::begin`](crate::Pool::begin). Call [`commit`](Self::commit) or
/// [`rollback`](Self::rollback) to finish the transaction; dropping the value without
/// committing rolls it back implicitly. Use [`with_annotations`](Self::with_annotations) /
/// [`with_operation`](Self::with_operation) to attach per-query semantic-convention
/// attributes.
///
/// `sqlx_otel.transaction.duration` runs from the client observing a successful `BEGIN` to the
/// terminal client observation, and its `sqlx_otel.transaction.outcome` attribute reports what the
/// client saw, not what the server did:
///
/// | Outcome              | The client observed                                                  |
/// |----------------------|----------------------------------------------------------------------|
/// | `commit_ok`          | `commit()` returned `Ok`                                             |
/// | `commit_error`       | `commit()` returned `Err`                                            |
/// | `commit_abandoned`   | the `commit()` future was dropped mid-flight: server outcome unknown |
/// | `rollback_ok`        | `rollback()` returned `Ok`                                           |
/// | `rollback_error`     | `rollback()` returned `Err`                                          |
/// | `rollback_abandoned` | the `rollback()` future was dropped mid-flight                       |
/// | `dropped`            | dropped with no terminal call started: a rollback was only queued    |
///
/// `dropped` is an observation, not an error: guard release or an early return can produce it
/// normally. It confirms only that `SQLx` queued rollback, not that server cleanup completed.
///
/// Telemetry adds no await, round trip or `Drop` behaviour to the transaction itself.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// # use sqlx_otel::PoolBuilder;
/// # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
/// let mut tx = pool.begin().await?;
/// sqlx::query("INSERT INTO orders (id) VALUES (1)")
///     .execute(&mut tx)
///     .await?;
/// tx.commit().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Transaction<'c, DB: sqlx::Database> {
    pub(crate) inner: sqlx::Transaction<'c, DB>,
    pub(crate) state: SharedState,
    pub(crate) usage: crate::connection::ConnectionUsage,
    pub(crate) lifecycle: TransactionLifecycle,
}

impl<DB> Transaction<'_, DB>
where
    DB: Database,
{
    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if the commit fails.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let result = self
            .lifecycle
            .finish(TransactionTermination::Commit, self.inner.commit())
            .await;
        drop(self.usage);
        result
    }

    /// Roll back the transaction.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if the rollback fails.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        let result = self
            .lifecycle
            .finish(TransactionTermination::Rollback, self.inner.rollback())
            .await;
        drop(self.usage);
        result
    }

    impl_with_annotations_mut!();
}
