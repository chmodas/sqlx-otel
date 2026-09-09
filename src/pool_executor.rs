//! Acquire through the instrumented pool, then execute on the raw connection.
//! Query spans/metrics remain owned by executor.rs, so each query is recorded once.

use crate::{Database, Pool};
use futures::{TryStreamExt, future::BoxFuture, stream::BoxStream};
use sqlx::{Describe, Either, Error, Execute, Executor};

#[derive(Debug)]
pub(crate) struct PoolExecutor<DB: sqlx::Database>(pub Pool<DB>);

impl<'c, DB> Executor<'c> for PoolExecutor<DB>
where
    DB: Database,
    for<'a> &'a mut DB::Connection: Executor<'a, Database = DB>,
{
    type Database = DB;

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

    fn describe<'e>(self, sql: sqlx::SqlStr) -> BoxFuture<'e, Result<Describe<DB>, Error>>
    where
        'c: 'e,
    {
        Box::pin(async move { self.0.acquire().await?.inner.as_mut().describe(sql).await })
    }
}
