//! Acquire through the instrumented pool, then execute on the raw connection.
//! Query spans/metrics remain owned by executor.rs, so each query is recorded once.

use crate::{Database, Pool};
use futures::{TryStreamExt, future::BoxFuture, stream::BoxStream};
use sqlx::{Describe, Either, Error, Execute, Executor};

/// Routes an implicit pool query through [`Pool::acquire`] instead of the raw `SQLx` pool.
///
/// Queries written against a pool (`query(..).fetch_one(&pool)`) never call `acquire()`
/// themselves, so without this indirection they would borrow a connection through
/// `sqlx::Pool` directly and contribute nothing to `db.client.connection.wait_time`,
/// `use_time`, `pending_requests`, or `timeouts`. This wrapper restores that accounting
/// while leaving query-level spans and metrics to `executor.rs`.
///
/// Holds an owned [`Pool`] rather than a borrow. The `Executor` impls for `&Pool` and
/// `Annotated<'_, Pool>` are generic over a lifetime unrelated to the pool reference, so
/// borrowing here would tie returned streams and futures to the pool's lifetime and break
/// callers that keep them alive independently. `sqlx::Pool`'s own executor clones for the same
/// reason; it costs a few `Arc` bumps per query.
#[derive(Debug)]
pub(crate) struct PoolExecutor<DB: sqlx::Database>(pub Pool<DB>);

impl<'c, DB> Executor<'c> for PoolExecutor<DB>
where
    DB: Database,
    for<'a> &'a mut DB::Connection: Executor<'a, Database = DB>,
{
    type Database = DB;

    // Only `fetch_many`, `fetch_optional`, `prepare_with` and `describe` are implemented here,
    // on purpose. `SQLx`'s default `execute`, `execute_many`, `fetch`, `fetch_all`, `fetch_one`
    // and `prepare` are all defined in terms of these four, so every entry point ends up at
    // exactly one `acquire()`. Adding an override that acquires again would double every
    // acquisition metric for that path, with no compile error and no obvious test failure.
    // `every_pool_executor_path_acquires_once_without_duplicate_query_spans` in
    // `tests/pool_metrics.rs` guards this by asserting one wait-time observation per query
    // across every one of those entry points.

    /// Acquire, then stream rows from the borrowed connection.
    ///
    /// The connection is acquired on first poll, not when the stream is created, and is held
    /// until the stream completes or is dropped – so `use_time` covers the full lease, and a
    /// partially consumed stream still returns its connection to the pool.
    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<'e, Result<Either<DB::QueryResult, DB::Row>, Error>>
    where
        E: 'q + Execute<'q, DB>,
        'c: 'e,
    {
        Box::pin(async_stream::try_stream! {
            let mut connection = self.0.acquire().await?;
            let mut rows = connection.inner.as_mut().fetch_many(query);
            while let Some(row) = rows.try_next().await? {
                yield row;
            }
        })
    }

    /// Acquire, run the query, and release the connection before returning.
    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<DB::Row>, Error>>
    where
        E: 'q + Execute<'q, DB>,
        'c: 'e,
    {
        Box::pin(async move {
            self.0
                .acquire()
                .await?
                .inner
                .as_mut()
                .fetch_optional(query)
                .await
        })
    }

    /// Acquire, prepare the statement, and release the connection before returning.
    ///
    /// The returned statement is owned, so it outlives the connection that produced it. Any
    /// driver-side statement cache entry stays with that connection, exactly as it does when
    /// preparing through `sqlx::Pool`.
    fn prepare_with<'e>(
        self,
        sql: sqlx::SqlStr,
        parameters: &'e [DB::TypeInfo],
    ) -> BoxFuture<'e, Result<DB::Statement, Error>>
    where
        'c: 'e,
    {
        Box::pin(async move {
            self.0
                .acquire()
                .await?
                .inner
                .as_mut()
                .prepare_with(sql, parameters)
                .await
        })
    }

    /// Acquire, describe the query, and release the connection before returning.
    fn describe<'e>(self, sql: sqlx::SqlStr) -> BoxFuture<'e, Result<Describe<DB>, Error>>
    where
        'c: 'e,
    {
        Box::pin(async move { self.0.acquire().await?.inner.as_mut().describe(sql).await })
    }
}
