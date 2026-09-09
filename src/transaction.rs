use crate::database::Database;
use crate::pool::SharedState;

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
        let result = self.inner.commit().await;
        drop(self.usage);
        result
    }

    /// Roll back the transaction.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if the rollback fails.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        let result = self.inner.rollback().await;
        drop(self.usage);
        result
    }

    impl_with_annotations_mut!();
}
