use crate::connection::Connection;
use crate::database::Database;
use crate::pool::SharedState;

/// An in-progress database transaction instrumented for OpenTelemetry.
///
/// Wraps a `sqlx::Transaction` and propagates shared attributes and metric instruments.
/// Use [`executor()`](Self::executor) to obtain a [`Connection`] for running queries
/// within the transaction, or pass `&mut transaction` directly to `SQLx` query builders.
#[derive(Debug)]
pub struct Transaction<'c, DB: sqlx::Database> {
    pub(crate) inner: sqlx::Transaction<'c, DB>,
    pub(crate) state: SharedState,
}

impl<DB> Transaction<'_, DB>
where
    DB: Database,
{
    /// Obtain a [`Connection`] executor for running instrumented queries within this
    /// transaction.
    pub fn executor(&mut self) -> Connection<'_, DB> {
        Connection {
            inner: &mut *self.inner,
            state: self.state.clone(),
        }
    }

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
}
