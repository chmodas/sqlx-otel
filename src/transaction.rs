use crate::database::Database;
use crate::pool::SharedState;

/// An in-progress database transaction instrumented for OpenTelemetry.
///
/// Wraps a `sqlx::Transaction` and propagates shared attributes and metric instruments.
/// Pass `&mut transaction` directly to `SQLx` query builders – the `Executor` impl on
/// `&mut Transaction` provides full span and metric instrumentation.
#[derive(Debug)]
pub struct Transaction<'c, DB: sqlx::Database> {
    pub(crate) inner: sqlx::Transaction<'c, DB>,
    pub(crate) state: SharedState,
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
        self.inner.commit().await
    }

    /// Roll back the transaction.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if the rollback fails.
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.inner.rollback().await
    }

    impl_with_annotations_mut!();
}
