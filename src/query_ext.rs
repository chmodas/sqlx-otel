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
//! # Map and macro queries
//!
//! [`Query::map`](Query::map) / [`Query::try_map`](Query::try_map) return
//! [`sqlx::query::Map<'q, DB, F, A>`](sqlx::query::Map), which is also covered by the trait.
//! `with_annotations` and `with_operation` may be applied at any of the three positions on a
//! hand-written `Query::map()` chain – before `bind`, between `bind` and `map`, or after
//! `map`:
//!
//! ```ignore
//! use sqlx_otel::QueryAnnotateExt;
//!
//! sqlx::query("SELECT id FROM users WHERE name = ?")
//!     .bind("alice")
//!     .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>("id"))
//!     .with_operation("SELECT", "users")
//!     .fetch_one(&pool)
//!     .await?;
//! ```
//!
//! The compile-time validated macro forms (`sqlx::query!()`, `sqlx::query_as!()`,
//! `sqlx::query_scalar!()`) expand to either `Query<'q, DB, _>` (for no-result-column shapes)
//! or `Map<'q, DB, _, _>` (for any shape that decodes columns). Both are covered:
//!
//! ```ignore
//! sqlx::query_as!(User, "SELECT id, name FROM users WHERE id = ?", 42_i64)
//!     .with_operation("SELECT", "users")
//!     .fetch_one(&pool)
//!     .await?;
//! ```
//!
//! Macro queries can only carry annotations *after* the macro returns – the macro itself
//! pre-applies `bind` and `try_map`, so positions 1 and 2 are not reachable by the user.

use futures::stream::BoxStream;
use sqlx::query::{Map, Query, QueryAs, QueryScalar};

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

impl<DB: sqlx::Database, F, A> sealed::Sealed for Map<'_, DB, F, A> {}
impl<DB: sqlx::Database, F, A> QueryAnnotateExt for Map<'_, DB, F, A> {
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
    /// existing executor-side surface. Only `Query` exposes this method – `QueryAs`,
    /// `QueryScalar`, and `Map` have no `execute_many` upstream.
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

    /// Map each row to another type. Mirrors [`sqlx::query::Query::map`] and carries the
    /// existing annotations forward unchanged onto the resulting `AnnotatedQuery<Map<...>>`,
    /// so `with_annotations` can be applied either before or after `.map()`.
    #[allow(clippy::type_complexity)]
    pub fn map<F, O>(
        self,
        f: F,
    ) -> AnnotatedQuery<Map<'q, DB, impl FnMut(DB::Row) -> Result<O, sqlx::Error> + Send, A>>
    where
        F: FnMut(DB::Row) -> O + Send,
        O: Unpin,
    {
        AnnotatedQuery {
            inner: self.inner.map(f),
            annotations: self.annotations,
        }
    }

    /// Map each row to a `Result`. Mirrors [`sqlx::query::Query::try_map`] and carries the
    /// existing annotations forward unchanged.
    pub fn try_map<F, O>(self, f: F) -> AnnotatedQuery<Map<'q, DB, F, A>>
    where
        F: FnMut(DB::Row) -> Result<O, sqlx::Error> + Send,
        O: Unpin,
    {
        AnnotatedQuery {
            inner: self.inner.try_map(f),
            annotations: self.annotations,
        }
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

// --- AnnotatedQuery<Map<'q, DB, F, A>> -------------------------------------

impl<'q, DB, F, A, O> AnnotatedQuery<Map<'q, DB, F, A>>
where
    DB: Database,
    F: FnMut(DB::Row) -> Result<O, sqlx::Error> + Send,
    O: Send + Unpin,
    A: 'q + Send + sqlx::IntoArguments<'q, DB>,
{
    impl_annotated_query_fetch_forwarders!(row = O, extra_bounds = (DB: 'e, F: 'e, O: 'e,));

    /// Compose a further mapping on top of this annotated map. Mirrors
    /// [`sqlx::query::Map::map`] (which itself composes via `f(row).and_then(&mut g)`) and
    /// preserves the existing annotations on the wrapper.
    #[allow(clippy::type_complexity)]
    pub fn map<G, P>(
        self,
        g: G,
    ) -> AnnotatedQuery<Map<'q, DB, impl FnMut(DB::Row) -> Result<P, sqlx::Error> + Send, A>>
    where
        G: FnMut(O) -> P + Send,
        P: Unpin,
    {
        AnnotatedQuery {
            inner: self.inner.map(g),
            annotations: self.annotations,
        }
    }

    /// Fallible variant of [`map`](Self::map). Mirrors [`sqlx::query::Map::try_map`] and
    /// preserves the existing annotations on the wrapper.
    #[allow(clippy::type_complexity)]
    pub fn try_map<G, P>(
        self,
        g: G,
    ) -> AnnotatedQuery<Map<'q, DB, impl FnMut(DB::Row) -> Result<P, sqlx::Error> + Send, A>>
    where
        G: FnMut(O) -> Result<P, sqlx::Error> + Send,
        P: Unpin,
    {
        AnnotatedQuery {
            inner: self.inner.try_map(g),
            annotations: self.annotations,
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use sqlx::Execute as _;
    use sqlx::Sqlite;

    use super::*;

    // --- query() / query_as() / query_scalar() --------------------

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

    // --- Map / Query::map / Query::try_map composition --------------------

    #[test]
    fn query_with_annotations_map_preserves_annotations() {
        // Position 1: annotate before `.map()`. Annotations must survive the wrap.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .map(|_row: sqlx::sqlite::SqliteRow| 42_i64);
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn query_bind_with_annotations_map_preserves_annotations() {
        // Position 2: annotate between `.bind()` and `.map()`.
        let q = sqlx::query::<Sqlite>("SELECT ?1")
            .bind(1_i32)
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .map(|_row: sqlx::sqlite::SqliteRow| 42_i64);
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn query_map_with_annotations_replaces_previous() {
        // Position 3 + last-call-wins on the new `Map` wrapper.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .map(|_row: sqlx::sqlite::SqliteRow| 42_i64)
            .with_annotations(QueryAnnotations::new().operation("FIRST"))
            .with_annotations(QueryAnnotations::new().operation("SECOND"));
        assert_eq!(q.annotations.operation.as_deref(), Some("SECOND"));
    }

    #[test]
    fn query_try_map_with_annotations_compose() {
        // `try_map` stores `F` directly; sanity-check the non-opaque closure branch.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .try_map(|_row: sqlx::sqlite::SqliteRow| Ok::<_, sqlx::Error>(42_i64))
            .with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
    }

    #[test]
    fn annotated_query_try_map_preserves_annotations() {
        // Exercise `AnnotatedQuery<Query>::try_map` (the wrapper's own method, not sqlx's).
        // Position-1-then-fallible-mapper.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .try_map(|_row: sqlx::sqlite::SqliteRow| Ok::<_, sqlx::Error>(42_i64));
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn map_compose_after_annotations() {
        // Multi-map composition: annotations survive across two `.map()` calls.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .with_annotations(QueryAnnotations::new().operation("SELECT"))
            .map(|_row: sqlx::sqlite::SqliteRow| 1_i64)
            .map(|n| n + 1);
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
    }

    #[test]
    fn map_then_with_operation_replaces_via_wrapper() {
        // `with_operation` shorthand on the new `AnnotatedQuery<Map<...>>` wrapper.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .map(|_row: sqlx::sqlite::SqliteRow| 1_i64)
            .with_annotations(QueryAnnotations::new().query_summary("legacy"))
            .with_operation("SELECT", "users");
        assert_eq!(q.annotations.operation.as_deref(), Some("SELECT"));
        assert_eq!(q.annotations.collection.as_deref(), Some("users"));
        assert!(q.annotations.query_summary.is_none());
    }

    #[test]
    fn debug_impl_for_annotated_map_includes_annotations() {
        // The manual `Debug` impl on the new `Map` wrapper still prints annotations.
        let q = sqlx::query::<Sqlite>("SELECT 1")
            .map(|_row: sqlx::sqlite::SqliteRow| 1_i64)
            .with_annotations(QueryAnnotations::new().operation("DEBUG_MAP"));
        let debug = format!("{q:?}");
        assert!(debug.contains("AnnotatedQuery"));
        assert!(debug.contains("DEBUG_MAP"));
    }
}
