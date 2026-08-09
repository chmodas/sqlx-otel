//! Taking a [`sqlx::Execute`] value apart, so the SQL text can be read for `db.query.text` while
//! the query is still forwarded to the inner executor. [`sqlx::Execute::sql`] consumes the query,
//! which would otherwise make those two mutually exclusive.
//!
//! A driver reads four things off an `Execute` value: `sql`, `statement`, `take_arguments` and
//! `persistent`. [`RebuiltQuery`] holds all four and reports each one exactly as the original
//! query would have, so the driver cannot tell it apart from what the caller passed in. Rebuilding
//! through `sqlx::query_with` / `sqlx::raw_sql` instead would not manage that: `raw_sql` forces
//! `persistent` to `false`, and neither constructor can carry a cached statement handle.

use sqlx::error::BoxDynError;

use crate::database::Database;

/// A [`sqlx::Execute`] value taken apart into the four values a driver reads from it, retaining
/// the SQL text for span and metric attributes.
///
/// Constructed by [`RebuiltQuery::split`] and immediately handed to the inner executor. It is never
/// held across an await point and never escapes the `Executor` method that built it.
pub(crate) struct RebuiltQuery<DB: Database> {
    /// The query text, moved out of the original query. Also the source for `db.query.text`.
    sql: sqlx::SqlStr,
    /// A cached statement handle, when the caller built the query from a prepared statement.
    /// Postgres reads it to skip re-resolving statement metadata.
    statement: Option<DB::Statement>,
    /// The arguments, already taken from the original query. `None` once taken by the driver;
    /// `Some(Ok(None))` means "use the simple query protocol and do not prepare"; `Some(Err(_))`
    /// carries an encoding failure that must surface at execution time, not here.
    arguments: Option<Result<Option<DB::Arguments>, BoxDynError>>,
    /// Whether the driver should cache the prepared statement.
    persistent: bool,
}

impl<DB: Database> RebuiltQuery<DB> {
    /// Take a query apart, retaining everything the driver will ask for.
    ///
    /// Do not reorder the reads below. `Map::persistent()` is `self.inner.arguments.is_some()`, so
    /// it reports `false` once the arguments have been taken. Drivers take the arguments first, so
    /// `false` is what they see; reading `persistent` before `take_arguments` would capture `true`
    /// instead and switch on statement caching that sqlx does not do for `Map` – the shape every
    /// `query!()` / `query_as!()` expands to.
    pub(crate) fn split<'q, E>(mut query: E) -> Self
    where
        E: sqlx::Execute<'q, DB>,
    {
        // 1. Borrowed, so it must be cloned before the query is consumed in step 4. `Statement` is
        //    `Clone` by trait bound and the concrete impls are cheap (an `SqlStr` plus an `Arc`).
        let statement = query.statement().cloned();
        // 2. Must precede the `persistent` read – see the note above.
        let arguments = query.take_arguments();
        // 3. Reflects the post-take state, matching what the driver would have observed.
        let persistent = query.persistent();
        // 4. Consumes `query`; nothing may touch it afterwards.
        let sql = query.sql();

        Self {
            sql,
            statement,
            arguments: Some(arguments),
            persistent,
        }
    }

    /// Borrow the query text for span and metric attributes.
    pub(crate) fn sql_str(&self) -> &str {
        self.sql.as_str()
    }
}

impl<DB: Database> sqlx::Execute<'_, DB> for RebuiltQuery<DB> {
    fn sql(self) -> sqlx::SqlStr {
        self.sql
    }

    fn statement(&self) -> Option<&DB::Statement> {
        self.statement.as_ref()
    }

    /// Yield the arguments taken in [`RebuiltQuery::split`], exactly once. A second call returns
    /// `Ok(None)`, as `sqlx::query::Query` does.
    fn take_arguments(&mut self) -> Result<Option<DB::Arguments>, BoxDynError> {
        self.arguments.take().unwrap_or(Ok(None))
    }

    fn persistent(&self) -> bool {
        self.persistent
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use proptest::prelude::*;
    use sqlx::{Arguments as _, Execute as _, Sqlite, Statement as _};

    use super::*;

    /// The four values a driver reads off a [`sqlx::Execute`], flattened so an original and its
    /// rebuilt counterpart can be compared directly.
    ///
    /// Arguments are reduced to a classification plus a length rather than compared structurally,
    /// because `Arguments` exposes no equality and the encoded buffer is driver-private. The
    /// distinction that matters – `Some` vs `None` vs `Err` – is preserved exactly, and `None` is
    /// the one that decides between the prepared and simple query protocols.
    #[derive(Debug, PartialEq, Eq)]
    struct Observables {
        sql: String,
        statement_sql: Option<String>,
        arguments: ArgClass,
        persistent: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ArgClass {
        /// Prepared, carrying this many encoded arguments.
        Some(usize),
        /// Simple query protocol – the driver must not prepare this statement.
        None,
        /// Argument encoding failed before execution.
        Err,
    }

    /// Read all four in the order every sqlx driver uses. Consumes the query, because
    /// `sql()` does.
    fn observe<'q, E: sqlx::Execute<'q, Sqlite>>(mut query: E) -> Observables {
        let statement_sql = query
            .statement()
            .map(|s| sqlx::Statement::sql(s).as_str().to_owned());
        let arguments = match query.take_arguments() {
            Ok(Some(a)) => ArgClass::Some(a.len()),
            Ok(None) => ArgClass::None,
            Err(_) => ArgClass::Err,
        };
        let persistent = query.persistent();
        Observables {
            sql: query.sql().as_str().to_owned(),
            statement_sql,
            arguments,
            persistent,
        }
    }

    /// The shapes of `Execute` value this crate can be handed. Each maps to a different
    /// combination of those four values, so covering all six is what makes the parity property
    /// meaningful.
    #[derive(Debug, Clone)]
    enum Kind {
        /// `sqlx::query(..)` with no binds.
        Plain,
        /// `sqlx::query(..)` with `n` binds.
        Prepared(usize),
        /// `sqlx::raw_sql(..)` – the `Ok(None)` + `persistent == false` case.
        Raw,
        /// `.map(..)`, which `query!()` / `query_as!()` expand to. Its `persistent()` is derived
        /// from whether arguments are still present, so it is the only shape that detects a
        /// reordering of the reads in `split`. Do not drop it from this generator.
        Mapped,
        /// An `AssertSqlSafe(String)`, routed through the blanket `Execute for T: SqlSafeStr` impl
        /// – the same path a bare `&'static str` takes.
        Bare,
        /// A query carrying an argument that fails to encode, so `take_arguments` yields `Err`.
        FailedEncode,
    }

    /// A bind value whose `Encode` impl always fails, so `Query::bind` stores an `Err` in place of
    /// the arguments. Nothing in sqlx's own `SQLite` types can be made to fail on demand, and the
    /// `Err` branch is the one that must not be silently swallowed into `Ok(None)` – which would
    /// turn a failed encode into a successful unprepared execution.
    struct AlwaysFailsToEncode;

    impl sqlx::Type<Sqlite> for AlwaysFailsToEncode {
        fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
            <i32 as sqlx::Type<Sqlite>>::type_info()
        }
    }

    impl sqlx::Encode<'_, Sqlite> for AlwaysFailsToEncode {
        fn encode_by_ref(
            &self,
            _buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer,
        ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
            Err("deliberate encode failure".into())
        }
    }

    fn kind_strategy() -> impl Strategy<Value = Kind> {
        prop_oneof![
            Just(Kind::Plain),
            (0usize..4).prop_map(Kind::Prepared),
            Just(Kind::Raw),
            Just(Kind::Mapped),
            Just(Kind::Bare),
            Just(Kind::FailedEncode),
        ]
    }

    /// Observe a freshly built query twice – once directly, once through [`RebuiltQuery::split`] –
    /// and require the two to agree.
    ///
    /// The builder expression is evaluated twice rather than the value being cloned, because
    /// `Execute` is consumed by inspection and is not `Clone`. Kept as a macro rather than a
    /// function taking `impl Fn`: the concrete query types (`Query<'q, ..>`, `Map<'q, ..>`) borrow
    /// from their SQL, so a trait-object or HRTB erasure would not admit them.
    macro_rules! assert_split_parity {
        ($build:expr) => {{
            let direct = observe($build);
            let split = observe(RebuiltQuery::<Sqlite>::split($build));
            prop_assert_eq!(direct, split);
        }};
    }

    /// Non-empty, non-control SQL text. The content is never parsed – only carried – so the
    /// generator targets byte-level fidelity rather than syntactic validity.
    fn sql_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("SELECT 1".to_owned()),
            Just(String::new()),
            Just("SELECT '\u{1F600} πŸ¦€ \u{202E}'".to_owned()),
            Just("SELECT ?1, ?2, ?3".to_owned()),
            "[ -~]{0,64}".prop_map(|s| s),
            any::<String>(),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Algorithmic parity: splitting a query and observing the rebuilt value must yield
        /// exactly what observing the original would have. This is the property that proves all
        /// three argument branches – `Some`, `None`, and `Err` – round-trip, and that the cached
        /// statement handle and `persistent` flag survive with them. `Kind::FailedEncode` is what
        /// reaches the `Err` arm; without it the branch would be unreachable from this generator.
        #[test]
        fn split_preserves_all_observables(kind in kind_strategy(), sql in sql_strategy()) {
            let owned = || sqlx::AssertSqlSafe(sql.clone());
            match kind {
                Kind::Plain => assert_split_parity!(sqlx::query::<Sqlite>(owned())),
                Kind::Prepared(n) => assert_split_parity!({
                    let mut q = sqlx::query::<Sqlite>(owned());
                    for i in 0..n {
                        q = q.bind(i32::try_from(i).unwrap_or(i32::MAX));
                    }
                    q
                }),
                Kind::Raw => assert_split_parity!(sqlx::raw_sql(owned())),
                Kind::Mapped => assert_split_parity!(sqlx::query::<Sqlite>(owned()).map(|_| ())),
                Kind::Bare => assert_split_parity!(owned()),
                Kind::FailedEncode => {
                    assert_split_parity!(sqlx::query::<Sqlite>(owned()).bind(AlwaysFailsToEncode));
                }
            }
        }

        /// The SQL text survives byte-for-byte, whatever the input contains. `db.query.text` is
        /// derived from this, so any lossiness here would corrupt exported spans.
        #[test]
        fn sql_text_round_trips_byte_for_byte(sql in any::<String>()) {
            let q = sqlx::query::<Sqlite>(sqlx::AssertSqlSafe(sql.clone()));
            let rebuilt = RebuiltQuery::<Sqlite>::split(q);
            prop_assert_eq!(rebuilt.sql_str(), sql.as_str());
        }

        /// Splitting never panics, whatever the SQL and bind count.
        #[test]
        fn split_never_panics(sql in any::<String>(), binds in 0usize..8) {
            let mut q = sqlx::query::<Sqlite>(sqlx::AssertSqlSafe(sql));
            for i in 0..binds {
                q = q.bind(i32::try_from(i).unwrap_or(i32::MAX));
            }
            let _ = RebuiltQuery::<Sqlite>::split(q);
        }
    }

    /// A statement handle from `query_statement` is preserved rather than downgraded to plain
    /// text. Postgres reads it to skip re-resolving statement metadata.
    #[tokio::test]
    async fn statement_handle_survives_split() {
        use sqlx::{Connection as _, Executor as _};

        let mut conn = sqlx::SqliteConnection::connect(":memory:").await.unwrap();
        let stmt = conn
            .prepare(sqlx::SqlStr::from_static("SELECT 1"))
            .await
            .unwrap();
        let rebuilt = RebuiltQuery::<Sqlite>::split(stmt.query());

        let carried = sqlx::Execute::<Sqlite>::statement(&rebuilt);
        assert!(carried.is_some(), "cached statement handle must survive");
        assert_eq!(carried.map(|s| s.sql().as_str()), Some("SELECT 1"));
    }

    /// Arguments are yielded exactly once. A second take degrades to `Ok(None)` rather than
    /// panicking, matching `sqlx::query::Query`, whose `take_arguments` is
    /// `self.arguments.take().transpose()`.
    #[test]
    fn take_arguments_is_idempotent_after_the_first_call() {
        let mut rebuilt = RebuiltQuery::<Sqlite>::split(sqlx::query::<Sqlite>("SELECT ?1").bind(1));

        assert!(matches!(rebuilt.take_arguments(), Ok(Some(_))));
        assert!(matches!(rebuilt.take_arguments(), Ok(None)));
        assert!(matches!(rebuilt.take_arguments(), Ok(None)));
    }

    /// An encoding failure surfaces once, at execution time, and does not resurrect on a second
    /// take. Constructed directly because provoking a real encode failure through the public
    /// builder is driver-specific; `RebuiltQuery` is crate-private, so this is legitimate.
    #[test]
    fn argument_encoding_error_surfaces_once_then_degrades_to_none() {
        let mut rebuilt = RebuiltQuery::<Sqlite> {
            sql: sqlx::SqlStr::from_static("SELECT 1"),
            statement: None,
            arguments: Some(Err("encode failure".into())),
            persistent: true,
        };

        assert!(rebuilt.take_arguments().is_err());
        assert!(matches!(rebuilt.take_arguments(), Ok(None)));
    }

    /// `raw_sql` is the one shape carrying `Ok(None)` *and* `persistent == false`. Collapsing it
    /// into a `query_with` rebuild would silently promote an unprepared statement to a prepared
    /// one, so the combination is pinned explicitly.
    #[test]
    fn raw_sql_keeps_simple_protocol_and_non_persistent() {
        let rebuilt = RebuiltQuery::<Sqlite>::split(sqlx::raw_sql("SELECT 1; SELECT 2;"));

        assert!(!sqlx::Execute::<Sqlite>::persistent(&rebuilt));
        assert_eq!(rebuilt.sql_str(), "SELECT 1; SELECT 2;");
    }
}
