//! Query-side annotation surface – mirror of the executor-side
//! [`with_annotations`](crate::Pool::with_annotations) /
//! [`with_operation`](crate::Pool::with_operation) methods, but attached to the query builder
//! produced by [`sqlx::query`], [`sqlx::query_as`], and [`sqlx::query_scalar`].
//!
//! The motivation is locality: per-query OpenTelemetry attributes describe the query, so the
//! caller often wants to colocate them with the query text rather than with the executor:
//!
//! ```ignore
//! use sqlx_otel::{QueryAnnotateExt, QueryAnnotations};
//!
//! sqlx::query("SELECT * FROM users WHERE id = ?")
//!     .bind(42_i64)
//!     .with_annotations(QueryAnnotations::new().operation("SELECT").collection("users"))
//!     .execute(&pool)
//!     .await?;
//! ```
//!
//! The wrapper [`AnnotatedQuery`] exposes the same `execute`/`fetch*` surface as the inner
//! query and threads the annotations into span creation by wrapping the executor with the
//! existing [`Annotated`](Annotated) / [`AnnotatedMut`](AnnotatedMut) executor wrappers.
//!
//! # Limitations
//!
//! [`Query::map`](Query::map) / [`Query::try_map`](Query::try_map) return
//! [`sqlx::query::Map<'q, DB, F, A>`](sqlx::query::Map), which is **not yet** covered by the
//! trait. Apply `with_annotations` *before* `map` / `try_map` if you need both. Support for
//! `Map` is planned.
//!
//! The compile-time validated macro forms (`sqlx::query!()`, `sqlx::query_as!()`,
//! `sqlx::query_scalar!()`) expand to one of two public types and inherit support
//! accordingly:
//!
//! * `sqlx::query!("INSERT/UPDATE/DELETE ...")` (no result columns) expands to `Query<'q, DB,
//!   _>` and **works today**.
//! * `sqlx::query!("SELECT ...")`, `sqlx::query_as!()`, and `sqlx::query_scalar!()` expand to
//!   `Map<'q, DB, _, _>` and are **not yet** annotatable on the query side. For now, use the
//!   executor-side [`Pool::with_annotations`](crate::Pool::with_annotations) form with these
//!   macros, or apply annotations to a hand-written `sqlx::query_as` builder.

use futures::stream::BoxStream;
use sqlx::query::{Query, QueryAs, QueryScalar};

use crate::annotations::{Annotated, AnnotatedMut, QueryAnnotations};
use crate::database::Database;

/// Sealing module for [`QueryAnnotateExt`]. Downstream crates cannot implement the trait
/// because the wrapper types it produces (`Annotated` / `AnnotatedMut`) have private fields
/// that only this crate can construct.
mod sealed {
    /// Marker trait that prevents external impls of [`super::QueryAnnotateExt`].
    pub trait Sealed {}
}

/// Extension trait that attaches OpenTelemetry per-query annotations to the function-form
/// `SQLx` query builders ([`sqlx::query`], [`sqlx::query_as`], [`sqlx::query_scalar`]).
///
/// The trait is sealed and cannot be implemented downstream.
///
/// # Example
///
/// ```ignore
/// use sqlx_otel::QueryAnnotateExt;
///
/// sqlx::query("INSERT INTO orders (user_id) VALUES (?)")
///     .bind(42_i64)
///     .with_operation("INSERT", "orders")
///     .execute(&pool)
///     .await?;
/// ```
pub trait QueryAnnotateExt: sealed::Sealed + Sized {
    /// Wrap the query with the given per-query annotations.
    ///
    /// Returns an [`AnnotatedQuery`] that exposes the same `execute`/`fetch*` surface as the
    /// inner query but threads the annotations through to OpenTelemetry span creation.
    fn with_annotations(self, annotations: QueryAnnotations) -> AnnotatedQuery<Self>;

    /// Shorthand that sets `db.operation.name` and `db.collection.name`.
    ///
    /// Equivalent to
    /// `self.with_annotations(QueryAnnotations::new().operation(op).collection(coll))`.
    fn with_operation(
        self,
        operation: impl Into<String>,
        collection: impl Into<String>,
    ) -> AnnotatedQuery<Self> {
        self.with_annotations(
            QueryAnnotations::new()
                .operation(operation)
                .collection(collection),
        )
    }
}

impl<DB: sqlx::Database, A> sealed::Sealed for Query<'_, DB, A> {}
impl<DB: sqlx::Database, A> QueryAnnotateExt for Query<'_, DB, A> {
    fn with_annotations(self, annotations: QueryAnnotations) -> AnnotatedQuery<Self> {
        AnnotatedQuery {
            inner: self,
            annotations,
        }
    }
}

impl<DB: sqlx::Database, O, A> sealed::Sealed for QueryAs<'_, DB, O, A> {}
impl<DB: sqlx::Database, O, A> QueryAnnotateExt for QueryAs<'_, DB, O, A> {
    fn with_annotations(self, annotations: QueryAnnotations) -> AnnotatedQuery<Self> {
        AnnotatedQuery {
            inner: self,
            annotations,
        }
    }
}

impl<DB: sqlx::Database, O, A> sealed::Sealed for QueryScalar<'_, DB, O, A> {}
impl<DB: sqlx::Database, O, A> QueryAnnotateExt for QueryScalar<'_, DB, O, A> {
    fn with_annotations(self, annotations: QueryAnnotations) -> AnnotatedQuery<Self> {
        AnnotatedQuery {
            inner: self,
            annotations,
        }
    }
}

/// A `SQLx` query builder paired with OpenTelemetry per-query annotations.
///
/// Produced by [`QueryAnnotateExt::with_annotations`] / [`QueryAnnotateExt::with_operation`].
/// Each invocation of the executor-driven methods (`execute`, `fetch_all`, etc.) wraps the
/// executor with the annotations so the resulting span carries them.
///
/// The wrapper exposes [`bind`](AnnotatedQuery::bind) so the caller can chain
/// `.bind(...).with_annotations(...)` or `.with_annotations(...).bind(...)` interchangeably.
#[must_use = "annotated queries do nothing until you call execute / fetch* on them"]
pub struct AnnotatedQuery<Q> {
    inner: Q,
    annotations: QueryAnnotations,
}

impl<Q> std::fmt::Debug for AnnotatedQuery<Q> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnotatedQuery")
            .field("annotations", &self.annotations)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Internal trait: convert an executor argument into the existing Annotated /
// AnnotatedMut wrappers. Implemented for the same three borrow shapes that the
// `impl_executor!` macro covers.
// ---------------------------------------------------------------------------

/// Internal conversion trait. Implemented for the three executor borrow shapes that
/// `impl_executor!` already supports (`&Pool`, `&mut PoolConnection`, `&mut Transaction`),
/// each producing the matching [`Annotated`] / [`AnnotatedMut`] wrapper.
///
/// The HRTB `for<'a> &'a mut DB::Connection: sqlx::Executor<'a, Database = DB>` lives on
/// each individual impl rather than on the trait, to avoid trait-resolution recursion when
/// the user's executor type is itself constructed via the same HRTB.
///
/// Users do not call this trait directly – they pass `&pool`, `&mut conn`, or `&mut tx` to
/// [`AnnotatedQuery::execute`] / `fetch*` and the trait dispatches internally. The trait is
/// effectively sealed because only this crate can construct the [`Annotated`] /
/// [`AnnotatedMut`] wrapper types (their fields are crate-private).
#[doc(hidden)]
pub trait IntoAnnotatedExecutor<'e, DB: Database> {
    /// The annotated executor wrapper type produced by [`into_annotated`](Self::into_annotated).
    type Wrapper: sqlx::Executor<'e, Database = DB>;

    /// Consume `self` together with the per-query annotations to produce the wrapper.
    fn into_annotated(self, annotations: QueryAnnotations) -> Self::Wrapper;
}

impl<'e, DB> IntoAnnotatedExecutor<'e, DB> for &'e crate::Pool<DB>
where
    DB: Database,
    for<'a> &'a mut DB::Connection: sqlx::Executor<'a, Database = DB>,
{
    type Wrapper = Annotated<'e, crate::Pool<DB>>;

    fn into_annotated(self, annotations: QueryAnnotations) -> Self::Wrapper {
        Annotated {
            inner: self,
            annotations,
            state: self.state.clone(),
        }
    }
}

impl<'e, DB> IntoAnnotatedExecutor<'e, DB> for &'e mut crate::PoolConnection<DB>
where
    DB: Database,
    for<'a> &'a mut DB::Connection: sqlx::Executor<'a, Database = DB>,
{
    type Wrapper = AnnotatedMut<'e, crate::PoolConnection<DB>>;

    fn into_annotated(self, annotations: QueryAnnotations) -> Self::Wrapper {
        AnnotatedMut {
            state: self.state.clone(),
            annotations,
            inner: self,
        }
    }
}

impl<'e, 'tx, DB> IntoAnnotatedExecutor<'e, DB> for &'e mut crate::Transaction<'tx, DB>
where
    DB: Database,
    for<'a> &'a mut DB::Connection: sqlx::Executor<'a, Database = DB>,
{
    type Wrapper = AnnotatedMut<'e, crate::Transaction<'tx, DB>>;

    fn into_annotated(self, annotations: QueryAnnotations) -> Self::Wrapper {
        AnnotatedMut {
            state: self.state.clone(),
            annotations,
            inner: self,
        }
    }
}

// ---------------------------------------------------------------------------
// Forwarder macros: emit method bodies inside a caller-supplied `impl` block.
//
// The macros expand to method items only (no `impl` header), so each call site
// declares its own generics and where-clauses. This avoids the local-ambiguity
// that arises when the macro tries to consume an `impl<...>` header via `tt`
// repetition, while still removing the per-builder duplication.
//
// Symbols referenced in the bodies (`Self`, `DB`, `A`, `O`, `QueryAnnotations`,
// `IntoAnnotatedExecutor`, `BoxStream`) resolve in the expansion context.
// ---------------------------------------------------------------------------

/// Emit the methods that every `AnnotatedQuery<Inner>` impl block has in common:
/// `with_annotations`, `with_operation`, and the five `fetch*` forwarders.
///
/// Parameters:
/// * `row` – the row type of the inner builder (`DB::Row` for `Query`, `O` for
///   `QueryAs` and `QueryScalar`).
/// * `extra_bounds` – additional lifetime bounds threaded onto each fetch-forwarder's
///   where-clause (`DB: 'e, O: 'e,` for `QueryAs` / `QueryScalar`; empty for `Query`).
macro_rules! impl_annotated_query_fetch_forwarders {
    (row = $row:ty, extra_bounds = ($($extra_bounds:tt)*)) => {
        /// Replace the wrapper's annotations. Last-call-wins – any previously set annotations
        /// on this wrapper are discarded.
        pub fn with_annotations(mut self, annotations: QueryAnnotations) -> Self {
            self.annotations = annotations;
            self
        }

        /// Shorthand replacement that sets `db.operation.name` and `db.collection.name`.
        /// Discards any previously set annotations on this wrapper.
        pub fn with_operation(
            self,
            operation: impl Into<String>,
            collection: impl Into<String>,
        ) -> Self {
            self.with_annotations(
                QueryAnnotations::new()
                    .operation(operation)
                    .collection(collection),
            )
        }

        /// Stream the resulting rows.
        pub fn fetch<'e, E>(self, executor: E) -> BoxStream<'e, Result<$row, sqlx::Error>>
        where
            'q: 'e,
            A: 'e,
            $($extra_bounds)*
            E: 'e + IntoAnnotatedExecutor<'e, DB>,
        {
            let wrapper = executor.into_annotated(self.annotations);
            self.inner.fetch(wrapper)
        }

        /// Stream a mix of `QueryResult`s and rows for multi-statement queries.
        ///
        /// `fetch_many` is `#[deprecated]` in `SQLx` 0.8 but kept for parity with the
        /// executor-side surface.
        #[allow(deprecated, clippy::type_complexity)]
        pub fn fetch_many<'e, E>(
            self,
            executor: E,
        ) -> BoxStream<'e, Result<sqlx::Either<DB::QueryResult, $row>, sqlx::Error>>
        where
            'q: 'e,
            A: 'e,
            $($extra_bounds)*
            E: 'e + IntoAnnotatedExecutor<'e, DB>,
        {
            let wrapper = executor.into_annotated(self.annotations);
            self.inner.fetch_many(wrapper)
        }

        /// Collect every row into a `Vec`.
        ///
        /// # Errors
        ///
        /// Returns any [`sqlx::Error`] surfaced by the underlying driver, including row
        /// decoding errors.
        pub async fn fetch_all<'e, E>(self, executor: E) -> Result<Vec<$row>, sqlx::Error>
        where
            'q: 'e,
            A: 'e,
            $($extra_bounds)*
            E: 'e + IntoAnnotatedExecutor<'e, DB>,
        {
            let wrapper = executor.into_annotated(self.annotations);
            self.inner.fetch_all(wrapper).await
        }

        /// Return exactly one row, erroring if none or more than one.
        ///
        /// # Errors
        ///
        /// Returns [`sqlx::Error::RowNotFound`] when the result set is empty, or any other
        /// [`sqlx::Error`] surfaced by the underlying driver.
        pub async fn fetch_one<'e, E>(self, executor: E) -> Result<$row, sqlx::Error>
        where
            'q: 'e,
            A: 'e,
            $($extra_bounds)*
            E: 'e + IntoAnnotatedExecutor<'e, DB>,
        {
            let wrapper = executor.into_annotated(self.annotations);
            self.inner.fetch_one(wrapper).await
        }

        /// Return at most one row.
        ///
        /// # Errors
        ///
        /// Returns any [`sqlx::Error`] surfaced by the underlying driver.
        pub async fn fetch_optional<'e, E>(
            self,
            executor: E,
        ) -> Result<Option<$row>, sqlx::Error>
        where
            'q: 'e,
            A: 'e,
            $($extra_bounds)*
            E: 'e + IntoAnnotatedExecutor<'e, DB>,
        {
            let wrapper = executor.into_annotated(self.annotations);
            self.inner.fetch_optional(wrapper).await
        }
    };
}

/// Emit the `bind` method on a specialised impl block.
///
/// `SQLx` restricts `bind` to the default `<DB as Database>::Arguments<'q>` parameter, so
/// each builder family has its own dedicated impl block keyed on the default arguments.
macro_rules! impl_annotated_query_bind {
    () => {
        /// Append a parameter binding, forwarding to the inner query. Mirrors the inner
        /// builder's own `bind` method.
        pub fn bind<T>(mut self, value: T) -> Self
        where
            T: 'q + sqlx::Encode<'q, DB> + sqlx::Type<DB>,
        {
            self.inner = self.inner.bind(value);
            self
        }
    };
}

// --- AnnotatedQuery<Query<'q, DB, A>> --------------------------------------

impl<'q, DB, A> AnnotatedQuery<Query<'q, DB, A>>
where
    DB: Database,
    A: 'q + Send + sqlx::IntoArguments<'q, DB>,
{
    impl_annotated_query_fetch_forwarders!(row = DB::Row, extra_bounds = ());

    /// Execute the query and return the number of rows affected. Wraps the executor with
    /// the carried annotations so the resulting span is annotated.
    ///
    /// # Errors
    ///
    /// Returns any [`sqlx::Error`] surfaced by the underlying driver.
    pub async fn execute<'e, E>(self, executor: E) -> Result<DB::QueryResult, sqlx::Error>
    where
        'q: 'e,
        A: 'e,
        E: 'e + IntoAnnotatedExecutor<'e, DB>,
    {
        let wrapper = executor.into_annotated(self.annotations);
        self.inner.execute(wrapper).await
    }

    /// Execute multiple statements separated by `;` and return their results as a stream.
    ///
    /// `execute_many` is `#[deprecated]` in `SQLx` 0.8 but kept here for parity with the
    /// existing executor-side surface. Only `Query` exposes this method – `QueryAs` and
    /// `QueryScalar` have no `execute_many` upstream.
    #[allow(deprecated)]
    pub async fn execute_many<'e, E>(
        self,
        executor: E,
    ) -> BoxStream<'e, Result<DB::QueryResult, sqlx::Error>>
    where
        'q: 'e,
        A: 'e,
        E: 'e + IntoAnnotatedExecutor<'e, DB>,
    {
        let wrapper = executor.into_annotated(self.annotations);
        self.inner.execute_many(wrapper).await
    }
}

impl<'q, DB> AnnotatedQuery<Query<'q, DB, <DB as sqlx::Database>::Arguments<'q>>>
where
    DB: sqlx::Database,
{
    impl_annotated_query_bind!();
}

// --- AnnotatedQuery<QueryAs<'q, DB, O, A>> ---------------------------------

impl<'q, DB, O, A> AnnotatedQuery<QueryAs<'q, DB, O, A>>
where
    DB: Database,
    A: 'q + Send + sqlx::IntoArguments<'q, DB>,
    O: Send + Unpin + for<'r> sqlx::FromRow<'r, DB::Row>,
{
    impl_annotated_query_fetch_forwarders!(row = O, extra_bounds = (DB: 'e, O: 'e,));
}

impl<'q, DB, O> AnnotatedQuery<QueryAs<'q, DB, O, <DB as sqlx::Database>::Arguments<'q>>>
where
    DB: sqlx::Database,
{
    impl_annotated_query_bind!();
}

// --- AnnotatedQuery<QueryScalar<'q, DB, O, A>> -----------------------------

impl<'q, DB, O, A> AnnotatedQuery<QueryScalar<'q, DB, O, A>>
where
    DB: Database,
    A: 'q + Send + sqlx::IntoArguments<'q, DB>,
    O: Send + Unpin,
    (O,): Send + Unpin + for<'r> sqlx::FromRow<'r, DB::Row>,
{
    impl_annotated_query_fetch_forwarders!(row = O, extra_bounds = (DB: 'e, O: 'e,));
}

impl<'q, DB, O> AnnotatedQuery<QueryScalar<'q, DB, O, <DB as sqlx::Database>::Arguments<'q>>>
where
    DB: sqlx::Database,
{
    impl_annotated_query_bind!();
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use sqlx::Execute as _;
    use sqlx::Sqlite;

    use super::*;

    #[test]
    fn with_annotations_replaces_previous() {
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("FIRST"))
            .with_annotations(QueryAnnotations::new().operation("SECOND"));
        assert_eq!(q.annotations.operation.as_deref(), Some("SECOND"));
    }

    #[test]
    fn with_operation_sets_both_fields() {
        let q = sqlx::query::<Sqlite>("SELECT 1").with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
        assert!(q.annotations.query_summary.is_none());
        assert!(q.annotations.stored_procedure.is_none());
    }

    #[test]
    fn with_operation_replaces_previous_annotations() {
        // Documents the last-call-wins behaviour: with_operation discards earlier fields.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().query_summary("legacy summary"))
            .with_operation("SELECT", "users");
        assert!(
            q.annotations.query_summary.is_none(),
            "with_operation must replace, not merge"
        );
    }

    #[test]
    fn debug_impl_includes_annotations() {
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("DEBUG_OP"));
        let debug = format!("{q:?}");
        assert!(debug.contains("AnnotatedQuery"));
        assert!(debug.contains("DEBUG_OP"));
    }

    #[test]
    fn bind_then_with_annotations_compose() {
        let q = sqlx::query::<Sqlite>("SELECT ?1, ?2")
            .bind(1_i32)
            .with_annotations(QueryAnnotations::new().operation("SELECT"));
        assert_eq!(q.inner.sql(), "SELECT ?1, ?2");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn with_annotations_first_then_bind_compose() {
        let q = sqlx::query::<Sqlite>("SELECT ?1, ?2")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .bind(1_i32);
        assert_eq!(q.inner.sql(), "SELECT ?1, ?2");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn query_as_supports_with_annotations() {
        let q = sqlx::query_as::<Sqlite, (i32,)>("SELECT 1").with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
    }

    #[test]
    fn query_as_wrapper_with_annotations_replaces() {
        // Exercise the inherent `with_annotations` on AnnotatedQuery<QueryAs<_>>.
        let q = sqlx::query_as::<Sqlite, (i32,)>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("FIRST"))
            .with_annotations(QueryAnnotations::new().operation("SECOND"));
        assert_eq!(q.annotations.operation.as_deref(), Some("SECOND"));
    }

    #[test]
    fn query_as_wrapper_with_operation_chains_on_wrapper() {
        // First call uses the trait method (on QueryAs); second uses the inherent method
        // on AnnotatedQuery, which exercises the wrapper-level shorthand.
        let q = sqlx::query_as::<Sqlite, (i32,)>("SELECT 1")
            .with_annotations(QueryAnnotations::new().query_summary("legacy"))
            .with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
        assert!(q.annotations.query_summary.is_none());
    }

    #[test]
    fn query_as_wrapper_bind_after_annotations() {
        let q = sqlx::query_as::<Sqlite, (i32,)>("SELECT ?1")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .bind(7_i32);
        assert_eq!(q.inner.sql(), "SELECT ?1");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn query_scalar_supports_with_annotations() {
        let q = sqlx::query_scalar::<Sqlite, i32>("SELECT 1").with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
    }

    #[test]
    fn query_scalar_wrapper_with_annotations_replaces() {
        let q = sqlx::query_scalar::<Sqlite, i32>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("FIRST"))
            .with_annotations(QueryAnnotations::new().operation("SECOND"));
        assert_eq!(q.annotations.operation.as_deref(), Some("SECOND"));
    }

    #[test]
    fn query_scalar_wrapper_with_operation_chains_on_wrapper() {
        let q = sqlx::query_scalar::<Sqlite, i32>("SELECT 1")
            .with_annotations(QueryAnnotations::new().query_summary("legacy"))
            .with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
        assert!(q.annotations.query_summary.is_none());
    }

    #[test]
    fn query_scalar_wrapper_bind_after_annotations() {
        let q = sqlx::query_scalar::<Sqlite, i32>("SELECT ?1")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .bind(7_i32);
        assert_eq!(q.inner.sql(), "SELECT ?1");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }
}
