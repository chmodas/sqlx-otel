use crate::pool::SharedState;

/// Generate `with_annotations` and `with_operation` inherent methods for wrapper types
/// that hold a `SharedState` and return `AnnotatedMut` via mutable borrows.
///
/// Invoke inside an `impl` block – the macro emits only method items, not the enclosing
/// `impl`.
macro_rules! impl_with_annotations_mut {
    () => {
        /// Return an annotated executor that attaches per-query semantic convention
        /// attributes to every span created by the next operation.
        ///
        /// The returned wrapper borrows `self` mutably and implements `sqlx::Executor`
        /// with the same instrumentation, but with annotation values threaded through to
        /// span creation.
        #[must_use]
        pub fn with_annotations(
            &mut self,
            annotations: crate::annotations::QueryAnnotations,
        ) -> crate::annotations::AnnotatedMut<'_, Self> {
            crate::annotations::AnnotatedMut {
                state: self.state.clone(),
                annotations,
                inner: self,
            }
        }

        /// Shorthand for annotating the next operation with `db.operation.name` and
        /// `db.collection.name`.
        ///
        /// Equivalent to
        /// `self.with_annotations(QueryAnnotations::new().operation(op).collection(coll))`.
        #[must_use]
        pub fn with_operation(
            &mut self,
            operation: impl Into<String>,
            collection: impl Into<String>,
        ) -> crate::annotations::AnnotatedMut<'_, Self> {
            self.with_annotations(
                crate::annotations::QueryAnnotations::new()
                    .operation(operation)
                    .collection(collection),
            )
        }
    };
}

/// Per-query annotation values that enrich OpenTelemetry spans with semantic-convention
/// attributes the library cannot derive automatically (because it does not parse SQL).
///
/// Use the builder methods to set whichever attributes apply to a given query, then pass
/// the result through one of the equivalent annotation surfaces:
///
/// - **Executor-side** – [`Pool::with_annotations`](crate::Pool::with_annotations),
///   [`PoolConnection::with_annotations`](crate::PoolConnection::with_annotations), or
///   [`Transaction::with_annotations`](crate::Transaction::with_annotations). Returns a
///   borrowed wrapper that is itself an `sqlx::Executor`.
/// - **Query-side** – [`QueryAnnotateExt::with_annotations`](crate::QueryAnnotateExt) on
///   the builder produced by `sqlx::query`, `sqlx::query_as`, `sqlx::query_scalar`, or
///   their `_!` macro forms.
///
/// Both surfaces produce identical telemetry; pick whichever keeps the annotation closer to
/// the thing it describes.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// # use sqlx_otel::PoolBuilder;
/// use sqlx::Executor as _;
/// use sqlx_otel::QueryAnnotations;
/// # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
///
/// pool.with_annotations(
///     QueryAnnotations::new()
///         .operation("SELECT")
///         .collection("users"),
/// )
/// .fetch_all("SELECT * FROM users")
/// .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryAnnotations {
    /// `db.operation.name` – the database operation (e.g. `"SELECT"`, `"INSERT"`).
    pub(crate) operation: Option<String>,
    /// `db.collection.name` – the target table or collection (e.g. `"users"`).
    pub(crate) collection: Option<String>,
    /// `db.query.summary` – a low-cardinality summary of the query (e.g. `"SELECT users"`).
    pub(crate) query_summary: Option<String>,
    /// `db.stored_procedure.name` – the name of a stored procedure being called.
    pub(crate) stored_procedure: Option<String>,
}

impl QueryAnnotations {
    /// Create a new, empty set of annotations. All fields default to `None`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `db.operation.name` attribute – the database operation being performed
    /// (e.g. `"SELECT"`, `"INSERT"`, `"findAndModify"`).
    ///
    /// The OpenTelemetry semantic conventions require this value to be low cardinality,
    /// since it is used to construct span names when [`query_summary`](Self::query_summary)
    /// is not set. Callers who cannot guarantee low cardinality should set
    /// `query_summary` instead – the library uses that path without a low-cardinality
    /// assumption.
    #[must_use]
    pub fn operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    /// Set the `db.collection.name` attribute – the table or collection being operated on
    /// (e.g. `"users"`, `"orders"`).
    #[must_use]
    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    /// Set the `db.query.summary` attribute – a low-cardinality summary of the query
    /// (e.g. `"SELECT users"`, `"INSERT orders"`).
    ///
    /// When set, this value also drives the span name (level 1 of the OpenTelemetry
    /// database span name hierarchy), overriding the `{operation} {collection}`
    /// synthesis. Cardinality control is the caller's responsibility – a high-cardinality
    /// summary will produce high-cardinality span names.
    #[must_use]
    pub fn query_summary(mut self, summary: impl Into<String>) -> Self {
        self.query_summary = Some(summary.into());
        self
    }

    /// Set the `db.stored_procedure.name` attribute – the name of a stored procedure
    /// being called (e.g. `"get_user"`, `"sp_update_orders"`).
    #[must_use]
    pub fn stored_procedure(mut self, name: impl Into<String>) -> Self {
        self.stored_procedure = Some(name.into());
        self
    }
}

/// A shared-reference annotation wrapper that carries per-query attributes alongside a
/// borrowed executor. Returned by [`Pool::with_annotations`](crate::Pool::with_annotations)
/// and [`Pool::with_operation`](crate::Pool::with_operation).
///
/// Implements `sqlx::Executor` with the same instrumentation as the underlying type, but
/// with annotation values threaded through to span creation.
pub struct Annotated<'a, E> {
    pub(crate) inner: &'a E,
    pub(crate) annotations: QueryAnnotations,
    pub(crate) state: SharedState,
}

impl<E: std::fmt::Debug> std::fmt::Debug for Annotated<'_, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Annotated")
            .field("annotations", &self.annotations)
            .finish_non_exhaustive()
    }
}

/// A mutable-reference annotation wrapper that carries per-query attributes alongside a
/// mutably borrowed executor. Returned by
/// [`PoolConnection::with_annotations`](crate::PoolConnection::with_annotations),
/// [`Transaction::with_annotations`](crate::Transaction::with_annotations), and their
/// `with_operation` shorthands.
///
/// Implements `sqlx::Executor` with the same instrumentation as the underlying type, but
/// with annotation values threaded through to span creation.
pub struct AnnotatedMut<'a, E> {
    pub(crate) inner: &'a mut E,
    pub(crate) annotations: QueryAnnotations,
    pub(crate) state: SharedState,
}

impl<E: std::fmt::Debug> std::fmt::Debug for AnnotatedMut<'_, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnnotatedMut")
            .field("annotations", &self.annotations)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each setter sets exactly its own field and leaves the others untouched. We test
    /// every permutation (2^4 = 16) including the "none" and "all" cases.
    #[test]
    fn setter_permutations() {
        type Setter = fn(QueryAnnotations) -> QueryAnnotations;
        type Getter = fn(&QueryAnnotations) -> Option<&str>;

        let fields: &[(&str, Setter, Getter)] = &[
            (
                "operation",
                |a| a.operation("OP"),
                |a| a.operation.as_deref(),
            ),
            (
                "collection",
                |a| a.collection("COLL"),
                |a| a.collection.as_deref(),
            ),
            (
                "query_summary",
                |a| a.query_summary("SUM"),
                |a| a.query_summary.as_deref(),
            ),
            (
                "stored_procedure",
                |a| a.stored_procedure("SP"),
                |a| a.stored_procedure.as_deref(),
            ),
        ];

        for mask in 0u8..16 {
            let mut ann = QueryAnnotations::new();
            for (i, &(_, setter, _)) in fields.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    ann = setter(ann);
                }
            }
            for (i, &(name, _, getter)) in fields.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    assert!(
                        getter(&ann).is_some(),
                        "{name} should be Some for mask {mask:#06b}"
                    );
                } else {
                    assert!(
                        getter(&ann).is_none(),
                        "{name} should be None for mask {mask:#06b}"
                    );
                }
            }
        }
    }

    #[test]
    fn clone_produces_independent_copy() {
        let original = QueryAnnotations::new()
            .operation("SELECT")
            .collection("users");
        let cloned = original.clone();
        let modified = original.query_summary("SELECT users");
        assert_eq!(cloned.query_summary, None);
        assert_eq!(modified.query_summary.as_deref(), Some("SELECT users"));
    }

    #[test]
    fn debug_impl_is_non_empty() {
        let ann = QueryAnnotations::new().operation("SELECT");
        let debug = format!("{ann:?}");
        assert!(debug.contains("SELECT"));
    }

    fn test_state() -> SharedState {
        use std::sync::Arc;

        use crate::attributes::{ConnectionAttributes, QueryTextMode};
        use crate::metrics::Metrics;

        SharedState {
            attrs: Arc::new(ConnectionAttributes {
                system: "sqlite",
                host: None,
                port: None,
                namespace: None,
                network_peer_address: None,
                network_peer_port: None,
                network_protocol_name: None,
                network_transport: None,
                pool_name: None,
                query_text_mode: QueryTextMode::Off,
            }),
            metrics: Arc::new(Metrics::new()),
        }
    }

    #[test]
    fn annotated_debug() {
        let inner = "pool";
        let wrapper = Annotated {
            inner: &inner,
            annotations: QueryAnnotations::new().operation("SELECT"),
            state: test_state(),
        };
        let debug = format!("{wrapper:?}");
        assert!(debug.contains("Annotated"));
        assert!(debug.contains("SELECT"));
    }

    #[test]
    fn annotated_mut_debug() {
        let mut inner = "conn";
        let wrapper = AnnotatedMut {
            inner: &mut inner,
            annotations: QueryAnnotations::new().collection("users"),
            state: test_state(),
        };
        let debug = format!("{wrapper:?}");
        assert!(debug.contains("AnnotatedMut"));
        assert!(debug.contains("users"));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Calling a setter twice in succession leaves the field equal to the second
        /// value: each setter is unconditional and overwrites whatever was there.
        #[test]
        fn operation_last_write_wins(a in ".{0,64}", b in ".{0,64}") {
            let ann = QueryAnnotations::new().operation(a).operation(b.clone());
            prop_assert_eq!(ann.operation.as_deref(), Some(b.as_str()));
        }

        #[test]
        fn collection_last_write_wins(a in ".{0,64}", b in ".{0,64}") {
            let ann = QueryAnnotations::new().collection(a).collection(b.clone());
            prop_assert_eq!(ann.collection.as_deref(), Some(b.as_str()));
        }

        #[test]
        fn query_summary_last_write_wins(a in ".{0,64}", b in ".{0,64}") {
            let ann = QueryAnnotations::new().query_summary(a).query_summary(b.clone());
            prop_assert_eq!(ann.query_summary.as_deref(), Some(b.as_str()));
        }

        #[test]
        fn stored_procedure_last_write_wins(a in ".{0,64}", b in ".{0,64}") {
            let ann = QueryAnnotations::new().stored_procedure(a).stored_procedure(b.clone());
            prop_assert_eq!(ann.stored_procedure.as_deref(), Some(b.as_str()));
        }

        /// Setting one field never alters any of the others. Complements the
        /// example-based `setter_permutations` test by stressing arbitrary unicode.
        #[test]
        fn operation_does_not_affect_other_fields(s in ".{0,64}") {
            let ann = QueryAnnotations::new().operation(s);
            prop_assert!(ann.collection.is_none());
            prop_assert!(ann.query_summary.is_none());
            prop_assert!(ann.stored_procedure.is_none());
        }

        #[test]
        fn collection_does_not_affect_other_fields(s in ".{0,64}") {
            let ann = QueryAnnotations::new().collection(s);
            prop_assert!(ann.operation.is_none());
            prop_assert!(ann.query_summary.is_none());
            prop_assert!(ann.stored_procedure.is_none());
        }

        #[test]
        fn query_summary_does_not_affect_other_fields(s in ".{0,64}") {
            let ann = QueryAnnotations::new().query_summary(s);
            prop_assert!(ann.operation.is_none());
            prop_assert!(ann.collection.is_none());
            prop_assert!(ann.stored_procedure.is_none());
        }

        #[test]
        fn stored_procedure_does_not_affect_other_fields(s in ".{0,64}") {
            let ann = QueryAnnotations::new().stored_procedure(s);
            prop_assert!(ann.operation.is_none());
            prop_assert!(ann.collection.is_none());
            prop_assert!(ann.query_summary.is_none());
        }

        /// All four setters accept arbitrary unicode without panicking, including null
        /// bytes, surrogate-adjacent code points, and zero-length input.
        #[test]
        fn no_panic_setting_all_fields(
            op in any::<String>(),
            coll in any::<String>(),
            summary in any::<String>(),
            sp in any::<String>(),
        ) {
            let _ann = QueryAnnotations::new()
                .operation(op)
                .collection(coll)
                .query_summary(summary)
                .stored_procedure(sp);
        }

        /// `Clone` produces a value structurally equal to the original. Combined with
        /// the `String`-backed fields, this means cloning is a deep copy.
        #[test]
        fn clone_equals_original(
            op in proptest::option::of(".{0,64}"),
            coll in proptest::option::of(".{0,64}"),
            summary in proptest::option::of(".{0,64}"),
            sp in proptest::option::of(".{0,64}"),
        ) {
            let mut ann = QueryAnnotations::new();
            if let Some(s) = op { ann = ann.operation(s); }
            if let Some(s) = coll { ann = ann.collection(s); }
            if let Some(s) = summary { ann = ann.query_summary(s); }
            if let Some(s) = sp { ann = ann.stored_procedure(s); }
            let cloned = ann.clone();
            prop_assert_eq!(ann, cloned);
        }
    }
}
