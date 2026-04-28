#![allow(dead_code, clippy::must_use_candidate, clippy::missing_panics_doc)]

use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use sqlx_otel::QueryAnnotations;

/// Test harness that installs in-memory span and metric exporters as the global providers,
/// collects telemetry in-process, and cleans up on drop.
pub struct TestTelemetry {
    span_exporter: InMemorySpanExporter,
    metric_exporter: InMemoryMetricExporter,
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
}

impl TestTelemetry {
    /// Install in-memory exporters as the global tracer and meter providers.
    #[must_use]
    pub fn install() -> Self {
        let span_exporter = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter.clone())
            .build();
        opentelemetry::global::set_tracer_provider(tracer_provider.clone());

        let metric_exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(metric_exporter.clone()).build();
        let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();
        opentelemetry::global::set_meter_provider(meter_provider.clone());

        Self {
            span_exporter,
            metric_exporter,
            tracer_provider,
            meter_provider,
        }
    }

    /// Return all finished spans, flushing the provider first.
    #[must_use]
    pub fn spans(&self) -> Vec<SpanData> {
        let _ = self.tracer_provider.force_flush();
        self.span_exporter.get_finished_spans().unwrap_or_default()
    }

    /// Return all finished metrics, flushing the provider first.
    #[must_use]
    pub fn metrics(&self) -> Vec<opentelemetry_sdk::metrics::data::ResourceMetrics> {
        let _ = self.meter_provider.force_flush();
        self.metric_exporter
            .get_finished_metrics()
            .unwrap_or_default()
    }

    /// Drain the in-memory exporters so the next call to [`spans`](Self::spans) or
    /// [`metrics`](Self::metrics) sees a fresh window.
    ///
    /// Use this between sections of a single test that want to assert on a bounded set of
    /// spans/metrics, instead of re-installing the global telemetry providers (which is
    /// racy: a fresh `install()` mid-test replaces the global tracer/meter providers and
    /// silently changes which exporter receives subsequent operations).
    pub fn reset(&self) {
        let _ = self.tracer_provider.force_flush();
        let _ = self.meter_provider.force_flush();
        self.span_exporter.reset();
        self.metric_exporter.reset();
    }
}

impl Drop for TestTelemetry {
    fn drop(&mut self) {
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Shared assertion helpers
// ---------------------------------------------------------------------------

/// Find the attribute value for a given key in a span.
pub fn attr(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

/// Assert that a span carries the common attributes every instrumented operation must have.
///
/// `system` is the expected `db.system.name` value (e.g. `"sqlite"`, `"postgresql"`).
///
/// `db.namespace` and `db.query.text` are checked for non-empty string content rather
/// than mere presence, so a regression that emits empty strings or non-string values is
/// caught at the helper level.
pub fn assert_common_span_attributes(span: &SpanData, system: &str) {
    assert_eq!(span.span_kind, SpanKind::Client);
    assert_eq!(
        span.name, system,
        "span name should fall back to db.system.name"
    );
    assert_eq!(
        attr(span, "db.system.name"),
        Some(opentelemetry::Value::String(system.to_owned().into())),
        "db.system.name missing or wrong"
    );
    let namespace = attr(span, "db.namespace");
    assert!(
        matches!(&namespace, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "db.namespace should be a non-empty string, got {namespace:?}",
    );
    let query_text = attr(span, "db.query.text");
    assert!(
        matches!(&query_text, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "db.query.text should be a non-empty string, got {query_text:?}",
    );
}

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Row shape for the shared `macro_users` table used by the macro-form integration tests.
///
/// Generic over the primary-key type so each backend can pick the variant that matches its
/// column type (sqlite `INTEGER` decodes to `i64`; postgres `INTEGER` and mysql `INT`
/// decode to `i32`).
#[derive(Debug, sqlx::FromRow)]
pub struct MacroUser<Id> {
    pub id: Id,
    pub name: String,
}

/// Assert that a span has error status, an `error.type` attribute, and an exception event
/// with `exception.type` and `exception.message`.
///
/// All three attribute values are required to be non-empty strings, so a regression that
/// emits empty exception metadata is caught here rather than slipping past the suite.
pub fn assert_error_span(span: &SpanData) {
    assert!(
        matches!(&span.status, Status::Error { .. }),
        "span status should be Error, got {:?}",
        span.status
    );
    let error_type = attr(span, "error.type");
    assert!(
        matches!(&error_type, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "error.type should be a non-empty string, got {error_type:?}",
    );
    let event = span
        .events
        .iter()
        .find(|e| e.name == "exception")
        .expect("exception event missing");
    let exception_type = event
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "exception.type")
        .map(|kv| kv.value.clone());
    assert!(
        matches!(&exception_type, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "exception.type should be a non-empty string, got {exception_type:?}",
    );
    let exception_message = event
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "exception.message")
        .map(|kv| kv.value.clone());
    assert!(
        matches!(&exception_message, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "exception.message should be a non-empty string, got {exception_message:?}",
    );
}

// ---------------------------------------------------------------------------
// Backend parameterisation
// ---------------------------------------------------------------------------
//
// Why macros and not generic functions: the library's `impl_executor!` macro at
// `src/executor.rs` instantiates `Executor` impls for `&Pool<DB>`, `&mut
// PoolConnection<DB>`, `&mut Transaction<'_, DB>`, and the matching `Annotated` /
// `AnnotatedMut` wrappers, each gated by `for<'a> &'a mut DB::Connection: Executor<'a,
// Database = DB>`. Test bodies generic over `DB` (with that HRTB declared) trigger
// trait-resolution overflow on stable rustc – the compiler tries to satisfy the bound
// against multiple wrapper impls and recurses. Bumping `recursion_limit` does not
// help; the chain genuinely diverges.
//
// `macro_rules!` sidesteps the issue entirely: each invocation expands at the call
// site with concrete types, so the bound chain resolves directly against the upstream
// `sqlx::Sqlite` / `sqlx::Postgres` / `sqlx::MySql` impls. Trade-off: error messages
// point at the expansion site instead of the macro definition; mitigated by keeping
// each macro short and well-documented.

/// Per-backend SQL fragments and metadata used by parameterised test bodies.
///
/// Test bodies that vary only in SQL syntax (column types, upsert form, string concat)
/// read the relevant field from a `&Dialect` argument instead of being duplicated across
/// three backend files. Each backend file passes the matching constant
/// (`SQLITE_DIALECT`, `POSTGRES_DIALECT`, `MYSQL_DIALECT`) to the shared body.
pub struct Dialect {
    /// The OpenTelemetry `db.system.name` value (`"sqlite"`, `"postgresql"`, `"mysql"`).
    pub system: &'static str,
    /// Column definition for an integer primary key in CREATE TABLE: e.g. `"INTEGER
    /// PRIMARY KEY"` for sqlite, `"INT PRIMARY KEY"` for postgres / mysql.
    pub id_pk_column: &'static str,
    /// Column definition for a non-null text column: e.g. `"TEXT NOT NULL"` for sqlite
    /// and postgres, `"VARCHAR(255) NOT NULL"` for mysql.
    pub text_column: &'static str,
    /// Full SQL for an upsert that updates `affected_test`'s row id=1 to a new name.
    /// Each backend's syntax differs (`INSERT OR REPLACE` / `ON CONFLICT … DO UPDATE` /
    /// `ON DUPLICATE KEY UPDATE`).
    pub upsert_sql: &'static str,
    /// Expected `db.response.affected_rows` for the upsert above. Sqlite and postgres
    /// report `1`; mysql reports `2` (it counts match + update).
    pub upsert_affected_rows: i64,
    /// Full SQL for an UPDATE that mutates two rows by appending `_updated` to `name`
    /// using the dialect's string-concat operator (`||` for sqlite/postgres, `CONCAT(...)`
    /// for mysql).
    pub string_concat_update_sql: &'static str,
    /// Full SQL of the form `SELECT (?1 + ?2) AS sum`, accepting two `i32` binds and
    /// returning an `i64` named `sum`. Each backend uses its own placeholder syntax and
    /// (for postgres / mysql) explicit casts so the result fits in `i64` uniformly.
    pub bind_two_sum_sql: &'static str,
    /// Full SQL of the form `SELECT <placeholder>` for `prepare_with` calls that supply
    /// no concrete binds. Each backend uses its own placeholder syntax (`?` for sqlite
    /// and mysql, `$1` for postgres).
    pub prepare_with_select_sql: &'static str,
}

pub const SQLITE_DIALECT: Dialect = Dialect {
    system: "sqlite",
    id_pk_column: "INTEGER PRIMARY KEY",
    text_column: "TEXT NOT NULL",
    upsert_sql: "INSERT OR REPLACE INTO affected_test (id, name) VALUES (1, 'alice_updated')",
    upsert_affected_rows: 1,
    string_concat_update_sql: "UPDATE affected_test SET name = name || '_updated' WHERE id IN (2, 3)",
    bind_two_sum_sql: "SELECT ?1 + ?2 AS sum",
    prepare_with_select_sql: "SELECT ?",
};

pub const POSTGRES_DIALECT: Dialect = Dialect {
    system: "postgresql",
    id_pk_column: "INT PRIMARY KEY",
    text_column: "TEXT NOT NULL",
    upsert_sql: "INSERT INTO affected_test (id, name) VALUES (1, 'alice_updated') \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
    upsert_affected_rows: 1,
    string_concat_update_sql: "UPDATE affected_test SET name = name || '_updated' WHERE id IN (2, 3)",
    bind_two_sum_sql: "SELECT ($1::bigint + $2::bigint) AS sum",
    prepare_with_select_sql: "SELECT $1",
};

pub const MYSQL_DIALECT: Dialect = Dialect {
    system: "mysql",
    id_pk_column: "INT PRIMARY KEY",
    text_column: "VARCHAR(255) NOT NULL",
    upsert_sql: "INSERT INTO affected_test (id, name) VALUES (1, 'alice_updated') \
                 ON DUPLICATE KEY UPDATE name = VALUES(name)",
    // MySQL counts ON DUPLICATE KEY UPDATE as match (1) + update (1) = 2.
    upsert_affected_rows: 2,
    string_concat_update_sql: "UPDATE affected_test SET name = CONCAT(name, '_updated') WHERE id IN (2, 3)",
    bind_two_sum_sql: "SELECT CAST(? + ? AS SIGNED) AS sum",
    prepare_with_select_sql: "SELECT ?",
};

/// `DROP TABLE IF EXISTS` then `CREATE TABLE` at the supplied pool. Used at the top of
/// every parameterised body so sqlite (fresh `:memory:` per pool) and postgres / mysql
/// (shared container) behave identically. Expanded inline at the call site via the
/// `fresh_table!` macro so it operates on concrete pool types without HRTB issues.
#[macro_export]
macro_rules! fresh_table {
    ($pool:expr, $table:expr, $columns:expr) => {{
        use sqlx::Executor as _;
        let drop_sql = format!("DROP TABLE IF EXISTS {}", $table);
        $pool.execute(drop_sql.as_str()).await.unwrap();
        let create_sql = format!("CREATE TABLE {} ({})", $table, $columns);
        $pool.execute(create_sql.as_str()).await.unwrap();
    }};
}

/// The standard annotation set used across most annotation tests:
/// `db.operation.name = "SELECT"`, `db.collection.name = "users"`.
pub fn test_annotations() -> QueryAnnotations {
    QueryAnnotations::new()
        .operation("SELECT")
        .collection("users")
}

/// Assert that a span carries the attributes set by [`test_annotations`].
///
/// The `db.system.name` value is taken from the supplied `Dialect`, so this helper works
/// for every backend without per-file duplication.
pub fn assert_annotated_span(span: &SpanData, dialect: &Dialect) {
    assert_eq!(span.span_kind, SpanKind::Client);
    assert_eq!(span.name, "SELECT users");
    assert_eq!(
        attr(span, "db.system.name"),
        Some(opentelemetry::Value::String(dialect.system.into())),
    );
    assert_eq!(
        attr(span, "db.operation.name"),
        Some(opentelemetry::Value::String("SELECT".into())),
    );
    assert_eq!(
        attr(span, "db.collection.name"),
        Some(opentelemetry::Value::String("users".into())),
    );
}

/// Assert that the exporter contains exactly one span and that it matches the standard
/// annotation shape. Used to collapse the recurring trailing assertion block at the end
/// of every annotation-style test (including the `sqlx::query!()` macro tests, whose
/// bodies must remain backend-specific but whose assertions can reuse this helper).
pub fn assert_one_annotated_span(tel: &TestTelemetry, dialect: &Dialect) {
    let spans = tel.spans();
    assert_eq!(
        spans.len(),
        1,
        "expected exactly one span, got {}",
        spans.len()
    );
    assert_annotated_span(&spans[0], dialect);
}

// ---------------------------------------------------------------------------
// Parameterised test bodies (macro_rules)
// ---------------------------------------------------------------------------
//
// Each macro takes a pool factory expression and a dialect constant and expands to
// the full test body. Backend wrappers invoke with their factory + dialect:
//
//     #[tokio::test]
//     #[serial]
//     async fn execute_creates_span_via_pool() {
//         test_execute_creates_span_via_pool!(test_pool().await, common::SQLITE_DIALECT);
//     }

/// Bound-chain proof: exercises `&Pool<DB>: Executor` (plain), `Annotated<'_,
/// Pool<DB>>: Executor` (`with_annotations`), and the same via the `with_operation`
/// shorthand. Each `pool.execute(...)` runs against a freshly created table, so the
/// macro is safe to invoke against shared postgres / mysql containers.
#[macro_export]
macro_rules! test_execute_creates_span_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;
        $crate::fresh_table!(
            &pool,
            "exec_pool_test",
            &format!("id {}", $dialect.id_pk_column)
        );
        tel.reset();

        (&pool)
            .execute("INSERT INTO exec_pool_test (id) VALUES (1)")
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        assert!($crate::common::attr(&spans[0], "db.response.affected_rows").is_some());

        pool.with_annotations($crate::common::test_annotations())
            .execute("INSERT INTO exec_pool_test (id) VALUES (2)")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .execute("INSERT INTO exec_pool_test (id) VALUES (3)")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// Counterpart to `test_execute_creates_span_via_pool!` for `&mut PoolConnection<DB>`.
/// Acquires a connection from the pool, then exercises plain / annotated / shorthand
/// executes against a freshly created table.
#[macro_export]
macro_rules! test_execute_creates_span_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;
        $crate::fresh_table!(
            &pool,
            "exec_conn_test",
            &format!("id {}", $dialect.id_pk_column)
        );
        tel.reset();

        let mut conn = pool.acquire().await.unwrap();
        (&mut conn)
            .execute("INSERT INTO exec_conn_test (id) VALUES (1)")
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        assert!($crate::common::attr(&spans[0], "db.response.affected_rows").is_some());

        conn.with_annotations($crate::common::test_annotations())
            .execute("INSERT INTO exec_conn_test (id) VALUES (2)")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .execute("INSERT INTO exec_conn_test (id) VALUES (3)")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// Counterpart to `test_execute_creates_span_via_pool!` for `&mut Transaction<'_, DB>`.
/// Begins a transaction, runs three executes, and commits before asserting on the
/// collected spans.
#[macro_export]
macro_rules! test_execute_creates_span_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;
        $crate::fresh_table!(
            &pool,
            "exec_tx_test",
            &format!("id {}", $dialect.id_pk_column)
        );
        tel.reset();

        let mut tx = pool.begin().await.unwrap();
        (&mut tx)
            .execute("INSERT INTO exec_tx_test (id) VALUES (1)")
            .await
            .unwrap();

        tx.with_annotations($crate::common::test_annotations())
            .execute("INSERT INTO exec_tx_test (id) VALUES (2)")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .execute("INSERT INTO exec_tx_test (id) VALUES (3)")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        assert!($crate::common::attr(&spans[0], "db.response.affected_rows").is_some());
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `execute` against invalid SQL records an error span. Exercises plain, annotated, and
/// shorthand annotation paths.
#[macro_export]
macro_rules! test_execute_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = (&pool).execute("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = pool
            .with_annotations($crate::common::test_annotations())
            .execute("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = pool
            .with_operation("SELECT", "users")
            .execute("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `execute_many` over a multi-statement query yields one span per stream consumption.
/// Exercises plain, annotated, and shorthand paths against the wrapped pool.
#[macro_export]
macro_rules! test_execute_many_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = pool
            .with_operation("SELECT", "users")
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `execute_many` against `&mut PoolConnection<DB>`. Same shape as the pool variant
/// but acquires a connection first.
#[macro_export]
macro_rules! test_execute_many_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let mut stream = (&mut conn).execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );

        let mut stream = conn
            .with_annotations($crate::common::test_annotations())
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = conn
            .with_operation("SELECT", "users")
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `execute_many` against `&mut Transaction<'_, DB>`. Asserts on all three spans
/// after commit.
#[macro_export]
macro_rules! test_execute_many_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let mut stream = (&mut tx).execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_annotations($crate::common::test_annotations())
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_operation("SELECT", "users")
            .execute_many("SELECT 1; SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `execute_many` against invalid SQL records an error span on the streaming path.
#[macro_export]
macro_rules! test_execute_many_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).execute_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .execute_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let mut stream = pool
            .with_operation("SELECT", "users")
            .execute_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `fetch` against the wrapped pool. Streams 2 rows, then exercises annotated and
/// shorthand variants.
#[macro_export]
macro_rules! test_fetch_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).fetch("SELECT 1 UNION ALL SELECT 2");
        let mut count = 0u64;
        while stream.next().await.is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = pool
            .with_operation("SELECT", "users")
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_fetch_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let mut stream = (&mut conn).fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );

        let mut stream = conn
            .with_annotations($crate::common::test_annotations())
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = conn
            .with_operation("SELECT", "users")
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_fetch_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let mut stream = (&mut tx).fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_annotations($crate::common::test_annotations())
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_operation("SELECT", "users")
            .fetch("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// Verifies that dropping a `fetch` stream after consuming a single row still
/// finalises and exports the span (with `returned_rows` reflecting the partial read).
#[macro_export]
macro_rules! test_fetch_stream_dropped_early_still_records_span {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        {
            let mut stream = (&pool).fetch("SELECT 1 UNION ALL SELECT 2");
            let _ = stream.next().await;
        }

        let spans = tel.spans();
        assert_eq!(
            spans.len(),
            1,
            "span should be recorded even when stream is dropped early"
        );
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );
    }};
}

/// `fetch` against invalid SQL records an error span on the streaming path.
#[macro_export]
macro_rules! test_fetch_stream_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).fetch("INVALID SQL");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .fetch("INVALID SQL");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let mut stream = pool.with_operation("SELECT", "users").fetch("INVALID SQL");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `fetch_many` against the wrapped pool. Returns rows + result rows on a stream.
#[macro_export]
macro_rules! test_fetch_many_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).fetch_many("SELECT 1 UNION ALL SELECT 2");
        let mut rows = 0u64;
        let mut results = 0u64;
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                sqlx::Either::Left(_) => results += 1,
                sqlx::Either::Right(_) => rows += 1,
            }
        }
        drop(stream);

        assert_eq!(rows, 2);
        assert!(results >= 1, "should have at least one QueryResult");

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = pool
            .with_operation("SELECT", "users")
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_many` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_fetch_many_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let mut stream = (&mut conn).fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );

        let mut stream = conn
            .with_annotations($crate::common::test_annotations())
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        let mut stream = conn
            .with_operation("SELECT", "users")
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_many` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_fetch_many_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let mut stream = (&mut tx).fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_annotations($crate::common::test_annotations())
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        let mut stream = tx
            .with_operation("SELECT", "users")
            .fetch_many("SELECT 1 UNION ALL SELECT 2");
        while stream.next().await.is_some() {}
        drop(stream);

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// Verifies that dropping a `fetch_many` stream after consuming a single row still
/// finalises and exports the span.
#[macro_export]
macro_rules! test_fetch_many_dropped_early_still_records_span {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        {
            let mut stream = (&pool).fetch_many("SELECT 1 UNION ALL SELECT 2");
            let _ = stream.next().await;
        }

        let spans = tel.spans();
        assert_eq!(
            spans.len(),
            1,
            "span should be recorded even when stream is dropped early"
        );
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );
    }};
}

/// `fetch_many` against invalid SQL records an error span on the streaming path.
#[macro_export]
macro_rules! test_fetch_many_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = (&pool).fetch_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );

        let mut stream = pool
            .with_annotations($crate::common::test_annotations())
            .fetch_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let mut stream = pool
            .with_operation("SELECT", "users")
            .fetch_many("INVALID SQL GIBBERISH");
        let result = stream.next().await;
        assert!(result.is_some_and(|r| r.is_err()));
        drop(stream);
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `fetch_all` against the wrapped pool. Returns 3 rows; exercises plain, annotated,
/// and shorthand variants.
#[macro_export]
macro_rules! test_fetch_all_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let rows = (&pool)
            .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(3))
        );

        pool.with_annotations($crate::common::test_annotations())
            .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_all` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_fetch_all_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let rows = (&mut conn)
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );

        conn.with_annotations($crate::common::test_annotations())
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_all` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_fetch_all_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let rows = (&mut tx)
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        tx.with_annotations($crate::common::test_annotations())
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .fetch_all("SELECT 1 UNION ALL SELECT 2")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `fetch_all` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_fetch_all_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = (&pool).fetch_all("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = pool
            .with_annotations($crate::common::test_annotations())
            .fetch_all("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = pool
            .with_operation("SELECT", "users")
            .fetch_all("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `fetch_one` against the wrapped pool. Returns 1 row.
#[macro_export]
macro_rules! test_fetch_one_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _row = (&pool).fetch_one("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );

        pool.with_annotations($crate::common::test_annotations())
            .fetch_one("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .fetch_one("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_one` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_fetch_one_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let _row = (&mut conn).fetch_one("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );

        conn.with_annotations($crate::common::test_annotations())
            .fetch_one("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .fetch_one("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_one` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_fetch_one_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let _row = (&mut tx).fetch_one("SELECT 1").await.unwrap();

        tx.with_annotations($crate::common::test_annotations())
            .fetch_one("SELECT 1")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .fetch_one("SELECT 1")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `fetch_one` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_fetch_one_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = (&pool).fetch_one("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = pool
            .with_annotations($crate::common::test_annotations())
            .fetch_one("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = pool
            .with_operation("SELECT", "users")
            .fetch_one("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `fetch_optional` against the wrapped pool when the query returns one row.
#[macro_export]
macro_rules! test_fetch_optional_records_one_row {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = (&pool).fetch_optional("SELECT 1").await.unwrap();
        assert!(result.is_some());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );

        pool.with_annotations($crate::common::test_annotations())
            .fetch_optional("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .fetch_optional("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_optional` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_fetch_optional_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let result = (&mut conn).fetch_optional("SELECT 1").await.unwrap();
        assert!(result.is_some());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );

        conn.with_annotations($crate::common::test_annotations())
            .fetch_optional("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .fetch_optional("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `fetch_optional` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_fetch_optional_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let result = (&mut tx).fetch_optional("SELECT 1").await.unwrap();
        assert!(result.is_some());

        tx.with_annotations($crate::common::test_annotations())
            .fetch_optional("SELECT 1")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .fetch_optional("SELECT 1")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `fetch_optional` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_fetch_optional_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = (&pool).fetch_optional("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            None
        );

        let result = pool
            .with_annotations($crate::common::test_annotations())
            .fetch_optional("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = pool
            .with_operation("SELECT", "users")
            .fetch_optional("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

// ---------------------------------------------------------------------------
// prepare / prepare_with / describe
// ---------------------------------------------------------------------------

/// `prepare` against the wrapped pool. No rows returned; just verifies the span shape.
#[macro_export]
macro_rules! test_prepare_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _stmt = (&pool).prepare("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        pool.with_annotations($crate::common::test_annotations())
            .prepare("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .prepare("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `prepare` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_prepare_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let _stmt = (&mut conn).prepare("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        conn.with_annotations($crate::common::test_annotations())
            .prepare("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .prepare("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `prepare` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_prepare_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let _stmt = (&mut tx).prepare("SELECT 1").await.unwrap();

        tx.with_annotations($crate::common::test_annotations())
            .prepare("SELECT 1")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .prepare("SELECT 1")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `prepare` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_prepare_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let result = (&mut conn).prepare("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = conn
            .with_annotations($crate::common::test_annotations())
            .prepare("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = conn
            .with_operation("SELECT", "users")
            .prepare("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `prepare_with` against the wrapped pool.
#[macro_export]
macro_rules! test_prepare_with_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _stmt = (&pool)
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        pool.with_annotations($crate::common::test_annotations())
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `prepare_with` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_prepare_with_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let _stmt = (&mut conn)
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        conn.with_annotations($crate::common::test_annotations())
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `prepare_with` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_prepare_with_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let _stmt = (&mut tx)
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();

        tx.with_annotations($crate::common::test_annotations())
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .prepare_with($dialect.prepare_with_select_sql, &[])
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `prepare_with` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_prepare_with_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let result = (&mut conn).prepare_with("INVALID SQL GIBBERISH", &[]).await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = conn
            .with_annotations($crate::common::test_annotations())
            .prepare_with("INVALID SQL GIBBERISH", &[])
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = conn
            .with_operation("SELECT", "users")
            .prepare_with("INVALID SQL GIBBERISH", &[])
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

/// `describe` against the wrapped pool.
#[macro_export]
macro_rules! test_describe_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _desc = (&pool).describe("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        pool.with_annotations($crate::common::test_annotations())
            .describe("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        pool.with_operation("SELECT", "users")
            .describe("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `describe` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_describe_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let _desc = (&mut conn).describe("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        conn.with_annotations($crate::common::test_annotations())
            .describe("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);

        conn.with_operation("SELECT", "users")
            .describe("SELECT 1")
            .await
            .unwrap();
        $crate::common::assert_annotated_span(tel.spans().last().unwrap(), &$dialect);
    }};
}

/// `describe` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_describe_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let _desc = (&mut tx).describe("SELECT 1").await.unwrap();

        tx.with_annotations($crate::common::test_annotations())
            .describe("SELECT 1")
            .await
            .unwrap();

        tx.with_operation("SELECT", "users")
            .describe("SELECT 1")
            .await
            .unwrap();

        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 3);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());
        $crate::common::assert_annotated_span(&spans[1], &$dialect);
        $crate::common::assert_annotated_span(&spans[2], &$dialect);
    }};
}

/// `describe` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_describe_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let result = (&mut conn).describe("INVALID SQL GIBBERISH").await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        $crate::common::assert_error_span(&spans[0]);
        assert!($crate::common::attr(&spans[0], "db.response.returned_rows").is_none());

        let result = conn
            .with_annotations($crate::common::test_annotations())
            .describe("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);

        let result = conn
            .with_operation("SELECT", "users")
            .describe("INVALID SQL GIBBERISH")
            .await;
        assert!(result.is_err());
        let last = tel.spans().last().unwrap().clone();
        $crate::common::assert_annotated_span(&last, &$dialect);
        $crate::common::assert_error_span(&last);
    }};
}

// ---------------------------------------------------------------------------
// Misc: metrics, annotations
// ---------------------------------------------------------------------------

/// `db.client.operation.duration` histogram is populated for any executed query.
#[macro_export]
macro_rules! test_operation_duration_metric_is_recorded {
    ($pool_factory:expr, $dialect:expr) => {{
        use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
        let _ = $dialect; // unused – backend doesn't influence the metric shape
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await.unwrap();

        let resource_metrics = tel.metrics();
        assert!(!resource_metrics.is_empty(), "should have metric data");

        let mut found_duration = false;
        for rm in &resource_metrics {
            for sm in rm.scope_metrics() {
                for metric in sm.metrics() {
                    if metric.name() == "db.client.operation.duration" {
                        found_duration = true;
                        assert_eq!(metric.unit(), "s");
                        if let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() {
                            let dp: Vec<_> = hist.data_points().collect();
                            assert!(!dp.is_empty(), "histogram should have data points");
                            assert!(dp[0].count() > 0, "data point count should be > 0");
                            let has_system = dp[0]
                                .attributes()
                                .any(|kv| kv.key.as_str() == "db.system.name");
                            assert!(has_system, "metric should have db.system.name attribute");
                        } else {
                            panic!("db.client.operation.duration should be an f64 histogram");
                        }
                    }
                }
            }
        }
        assert!(
            found_duration,
            "db.client.operation.duration metric not found"
        );
    }};
}

/// All four annotation fields populated together; summary drives the span name.
#[macro_export]
macro_rules! test_annotation_all_four_fields {
    ($pool_factory:expr, $dialect:expr) => {{
        let _ = $dialect; // dialect.system is checked indirectly via assert_common_span_attributes when present
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        pool.with_annotations(
            sqlx_otel::QueryAnnotations::new()
                .operation("SELECT")
                .collection("users")
                .query_summary("users by id")
                .stored_procedure("sp_get_users"),
        )
        .fetch_all("SELECT 1")
        .await
        .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "users by id");
        assert_eq!(
            $crate::common::attr(&spans[0], "db.operation.name"),
            Some(opentelemetry::Value::String("SELECT".into())),
        );
        assert_eq!(
            $crate::common::attr(&spans[0], "db.collection.name"),
            Some(opentelemetry::Value::String("users".into())),
        );
        assert_eq!(
            $crate::common::attr(&spans[0], "db.query.summary"),
            Some(opentelemetry::Value::String("users by id".into())),
        );
        assert_eq!(
            $crate::common::attr(&spans[0], "db.stored_procedure.name"),
            Some(opentelemetry::Value::String("sp_get_users".into())),
        );
    }};
}

/// `db.query.summary` overrides the span name independently of `db.operation.name` and
/// `db.collection.name`, but does not suppress those attributes.
#[macro_export]
macro_rules! test_query_summary_drives_span_name {
    ($pool_factory:expr, $dialect:expr) => {{
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        pool.with_annotations(
            sqlx_otel::QueryAnnotations::new()
                .operation("SELECT")
                .collection("users")
                .query_summary("users by tenant"),
        )
        .fetch_all("SELECT 1")
        .await
        .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "users by tenant");
        assert_eq!(
            $crate::common::attr(&spans[0], "db.query.summary"),
            Some(opentelemetry::Value::String("users by tenant".into())),
        );
        assert_eq!(
            $crate::common::attr(&spans[0], "db.operation.name"),
            Some(opentelemetry::Value::String("SELECT".into())),
        );
        assert_eq!(
            $crate::common::attr(&spans[0], "db.collection.name"),
            Some(opentelemetry::Value::String("users".into())),
        );
    }};
}

// ---------------------------------------------------------------------------
// query-side annotations: sqlx::query(...).with_annotations(...).<method>(executor)
// ---------------------------------------------------------------------------

/// `sqlx::query(...).with_annotations(...).execute_many(&pool)`.
#[macro_export]
macro_rules! test_query_execute_many_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        #[allow(deprecated)]
        let mut stream = sqlx::query("SELECT 1; SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .execute_many(&pool)
            .await;
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query(...).with_annotations(...).fetch(&pool)`.
#[macro_export]
macro_rules! test_query_fetch_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );
    }};
}

/// `sqlx::query(...).with_annotations(...).fetch_many(&pool)`.
#[macro_export]
macro_rules! test_query_fetch_many_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        #[allow(deprecated)]
        let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch_many(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query(...).with_annotations(...).fetch_all(&pool)`.
#[macro_export]
macro_rules! test_query_fetch_all_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let rows = sqlx::query("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
            .with_annotations($crate::common::test_annotations())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(3))
        );
    }};
}

/// `sqlx::query(...).with_annotations(...).fetch_one(&pool)`.
#[macro_export]
macro_rules! test_query_fetch_one_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let _row = sqlx::query("SELECT 1")
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(1))
        );
    }};
}

/// `sqlx::query("INVALID SQL").with_annotations(...).execute(&pool)` records an error span.
#[macro_export]
macro_rules! test_query_execute_with_annotations_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result = sqlx::query("INVALID SQL GIBBERISH")
            .with_annotations($crate::common::test_annotations())
            .execute(&pool)
            .await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        $crate::common::assert_error_span(&spans[0]);
    }};
}

/// `sqlx::query_as(...).with_annotations(...).fetch(&pool)`.
#[macro_export]
macro_rules! test_query_as_fetch_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = sqlx::query_as::<_, (i32,)>("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_as(...).with_annotations(...).fetch_many(&pool)`.
#[macro_export]
macro_rules! test_query_as_fetch_many_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        #[allow(deprecated)]
        let mut stream = sqlx::query_as::<_, (i32,)>("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch_many(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_as(...).with_annotations(...).fetch_all(&pool)`.
#[macro_export]
macro_rules! test_query_as_fetch_all_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let rows: Vec<(i32,)> = sqlx::query_as("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_as(...).with_annotations(...).fetch_one(&pool)`.
#[macro_export]
macro_rules! test_query_as_fetch_one_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let row: (i32,) = sqlx::query_as("SELECT 7")
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 7);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_as(...).with_annotations(...).fetch_optional(&pool)` returning none.
#[macro_export]
macro_rules! test_query_as_fetch_optional_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let row: Option<(i32,)> = sqlx::query_as("SELECT 1 WHERE 1 = 0")
            .with_annotations($crate::common::test_annotations())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(row.is_none());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_as("INVALID SQL").with_annotations(...).fetch_one(&pool)` records error.
#[macro_export]
macro_rules! test_query_as_fetch_one_with_annotations_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result: Result<(i32,), _> = sqlx::query_as("INVALID SQL")
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        $crate::common::assert_error_span(&spans[0]);
    }};
}

/// `sqlx::query_scalar(...).with_annotations(...).fetch(&pool)`.
#[macro_export]
macro_rules! test_query_scalar_fetch_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = sqlx::query_scalar::<_, i32>("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_scalar(...).with_annotations(...).fetch_many(&pool)`.
#[macro_export]
macro_rules! test_query_scalar_fetch_many_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        #[allow(deprecated)]
        let mut stream = sqlx::query_scalar::<_, i32>("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch_many(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_scalar(...).with_annotations(...).fetch_all(&pool)`.
#[macro_export]
macro_rules! test_query_scalar_fetch_all_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let rows: Vec<i32> = sqlx::query_scalar("SELECT 1 UNION ALL SELECT 2")
            .with_annotations($crate::common::test_annotations())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows, vec![1, 2]);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_scalar(...).with_annotations(...).fetch_one(&pool)`.
#[macro_export]
macro_rules! test_query_scalar_fetch_one_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query_scalar("SELECT 42")
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 42);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `sqlx::query_scalar(...).with_annotations(...).fetch_optional(&pool)` returning none.
#[macro_export]
macro_rules! test_query_scalar_fetch_optional_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: Option<i32> = sqlx::query_scalar("SELECT 1 WHERE 1 = 0")
            .with_annotations($crate::common::test_annotations())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(value.is_none());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

// ---------------------------------------------------------------------------
// query-side annotations: Map (Query::map / Query::try_map)
// ---------------------------------------------------------------------------
//
// The closure parameter type for `.map(|row| ...)` is inferred from the surrounding
// `Query<DB>` chain, so we don't need to spell out per-backend `SqliteRow` / `PgRow` /
// `MySqlRow`.

/// `Query::with_annotations` before `bind`/`map`. Position-1 in the builder pipeline.
#[macro_export]
macro_rules! test_query_map_position_1_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 7")
            .with_annotations($crate::common::test_annotations())
            .map(|row: Row| row.get::<i32, _>(0))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 7);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::with_annotations` after `bind`, before `map`.
#[macro_export]
macro_rules! test_query_map_position_2_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 11")
            .with_annotations($crate::common::test_annotations())
            .map(|row: Row| row.get::<i32, _>(0))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 11);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::with_annotations` after `bind` and `map` – last in the pipeline.
#[macro_export]
macro_rules! test_query_map_position_3_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 13")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 13);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Same as `query_map_position_3` but with `try_map`.
#[macro_export]
macro_rules! test_query_try_map_position_3_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 17")
            .try_map(|row: Row| Ok(row.get::<i32, _>(0)))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 17);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` then `with_annotations` then `fetch(&pool)`.
#[macro_export]
macro_rules! test_map_fetch_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(2))
        );
    }};
}

/// `Query::map` then `with_annotations` then `fetch_many(&pool)`.
#[macro_export]
macro_rules! test_map_fetch_many_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use futures::StreamExt as _;
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        #[allow(deprecated)]
        let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_many(&pool);
        while stream.next().await.is_some() {}
        drop(stream);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` then `with_annotations` then `fetch_all(&pool)`.
#[macro_export]
macro_rules! test_map_fetch_all_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let rows: Vec<i32> = sqlx::query("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows, vec![1, 2, 3]);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` then `with_annotations` then `fetch_one(&pool)`.
#[macro_export]
macro_rules! test_map_fetch_one_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 19")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 19);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` then `with_annotations` then `fetch_optional(&pool)`.
#[macro_export]
macro_rules! test_map_fetch_optional_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: Option<i32> = sqlx::query("SELECT 1 WHERE 1 = 0")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(value.is_none());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Composing two `map` calls, with annotations between them.
#[macro_export]
macro_rules! test_map_compose_after_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 5")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .map(|n| n * 2)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 10);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Composing `map` then `try_map`, with annotations between.
#[macro_export]
macro_rules! test_map_try_map_compose_after_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let value: i32 = sqlx::query("SELECT 6")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .try_map(|n: i32| Ok::<_, sqlx::Error>(n + 100))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, 106);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_query_map_with_annotations_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        let value: i32 = sqlx::query("SELECT 23")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(value, 23);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_query_map_with_annotations_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        let value: i32 = sqlx::query("SELECT 29")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&mut tx)
            .await
            .unwrap();
        assert_eq!(value, 29);
        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `Query::map` against invalid SQL records an error span.
#[macro_export]
macro_rules! test_query_map_with_annotations_records_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result: Result<i32, _> = sqlx::query("INVALID SQL")
            .map(|row: Row| row.get::<i32, _>(0))
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        $crate::common::assert_error_span(&spans[0]);
    }};
}

/// `try_map` returning a mapper-side error: the database round-trip succeeds, the span
/// reports success, but the user-visible `Result` carries the mapper's error.
#[macro_export]
macro_rules! test_query_try_map_with_annotations_propagates_mapper_error {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let result: Result<i64, _> = sqlx::query("SELECT 1")
            .try_map(|_row: Row| {
                Err::<i64, _>(sqlx::Error::Decode(
                    "intentional decode failure".to_string().into(),
                ))
            })
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await;
        assert!(result.is_err());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

// ---------------------------------------------------------------------------
// PoolBuilder with_* methods
// ---------------------------------------------------------------------------
//
// These macros accept a *raw* pool factory (not the wrapped `Pool<DB>`) so the test
// body can configure `PoolBuilder` itself with the override under test.

/// `PoolBuilder::with_database` overrides the inferred `db.namespace`.
#[macro_export]
macro_rules! test_builder_with_database_overrides_namespace {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_database("custom_db")
            .build();

        let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.namespace"),
            Some(opentelemetry::Value::String("custom_db".into()))
        );
    }};
}

/// `PoolBuilder::with_host` overrides the inferred `server.address`.
#[macro_export]
macro_rules! test_builder_with_host_overrides_server_address {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_host("custom-host")
            .build();

        let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "server.address"),
            Some(opentelemetry::Value::String("custom-host".into()))
        );
    }};
}

/// `PoolBuilder::with_port` overrides the inferred `server.port`.
#[macro_export]
macro_rules! test_builder_with_port_overrides_server_port {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw).with_port(9999).build();

        let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "server.port"),
            Some(opentelemetry::Value::I64(9999))
        );
    }};
}

/// `PoolBuilder::with_network_peer_address` populates `network.peer.address`.
#[macro_export]
macro_rules! test_builder_with_network_peer_address {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_network_peer_address("10.0.0.5")
            .build();

        let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "network.peer.address"),
            Some(opentelemetry::Value::String("10.0.0.5".into()))
        );
    }};
}

/// `PoolBuilder::with_network_peer_port` populates `network.peer.port`.
#[macro_export]
macro_rules! test_builder_with_network_peer_port {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_network_peer_port(5433)
            .build();

        let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "network.peer.port"),
            Some(opentelemetry::Value::I64(5433))
        );
    }};
}

/// `Pool::close` and `Pool::is_closed` round-trip.
#[macro_export]
macro_rules! test_pool_close_and_is_closed {
    ($pool_factory:expr, $dialect:expr) => {{
        let _ = $dialect;
        let _tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        assert!(!pool.is_closed());
        pool.close().await;
        assert!(pool.is_closed());
    }};
}

/// `QueryTextMode::Off` suppresses the `db.query.text` attribute.
#[macro_export]
macro_rules! test_query_text_mode_off_suppresses_sql {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        let tel = $crate::common::TestTelemetry::install();
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_query_text_mode(sqlx_otel::QueryTextMode::Off)
            .build();

        let _: Option<(i32,)> = sqlx::query_as("SELECT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_kind, opentelemetry::trace::SpanKind::Client);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.system.name"),
            Some(opentelemetry::Value::String($dialect.system.into()))
        );
        assert!($crate::common::attr(&spans[0], "db.namespace").is_some());
        assert!(
            $crate::common::attr(&spans[0], "db.query.text").is_none(),
            "db.query.text should not be present when QueryTextMode::Off"
        );
    }};
}

// ---------------------------------------------------------------------------
// Dialect-portable test bodies
// ---------------------------------------------------------------------------

/// `execute` records the correct `db.response.affected_rows` for a sequence of
/// INSERT / upsert / UPDATE / DELETE statements. Uses the dialect's `upsert_sql`,
/// `upsert_affected_rows`, and `string_concat_update_sql` to handle backend-specific
/// upsert syntax and string-concat operators.
#[macro_export]
macro_rules! test_execute_records_affected_rows {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;
        $crate::fresh_table!(
            &pool,
            "affected_test",
            &format!("id {}, name {}", $dialect.id_pk_column, $dialect.text_column)
        );
        tel.reset();

        // --- Bulk insert via VALUES list ---
        (&pool)
            .execute(
                "INSERT INTO affected_test (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
            )
            .await
            .unwrap();
        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.affected_rows"),
            Some(opentelemetry::Value::I64(3)),
            "inserting 3 rows should affect 3 rows"
        );
        tel.reset();

        // --- Upsert (dialect-specific) ---
        (&pool).execute($dialect.upsert_sql).await.unwrap();
        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.affected_rows"),
            Some(opentelemetry::Value::I64($dialect.upsert_affected_rows)),
            "upsert affected_rows differs per backend"
        );
        tel.reset();

        // --- Update multiple rows (dialect-specific concat) ---
        (&pool)
            .execute($dialect.string_concat_update_sql)
            .await
            .unwrap();
        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.affected_rows"),
            Some(opentelemetry::Value::I64(2)),
            "updating two rows should affect 2 rows"
        );
        tel.reset();

        // --- Delete multiple rows ---
        (&pool)
            .execute("DELETE FROM affected_test WHERE id IN (1, 2, 3)")
            .await
            .unwrap();
        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.affected_rows"),
            Some(opentelemetry::Value::I64(3)),
            "deleting three rows should affect 3 rows"
        );
        tel.reset();

        // --- Delete with no matching rows ---
        (&pool)
            .execute("DELETE FROM affected_test WHERE id = 999")
            .await
            .unwrap();
        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.affected_rows"),
            Some(opentelemetry::Value::I64(0)),
            "deleting non-existent rows should affect 0 rows"
        );
    }};
}

/// Transaction rollback emits a single CREATE TABLE span and discards the table.
/// Uses `fresh_table!` so the test is repeatable against the shared postgres / mysql
/// containers.
#[macro_export]
macro_rules! test_transaction_rollback {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let pool = $pool_factory;
        // Pre-clean any leftover table before installing telemetry, so the rollback test
        // sees only its own span.
        let drop_sql = "DROP TABLE IF EXISTS rollback_test";
        (&pool).execute(drop_sql).await.unwrap();

        let tel = $crate::common::TestTelemetry::install();

        let mut tx = pool.begin().await.unwrap();
        let create_sql = format!("CREATE TABLE rollback_test (id {})", $dialect.id_pk_column);
        (&mut tx).execute(create_sql.as_str()).await.unwrap();
        tx.rollback().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
    }};
}

/// `QueryTextMode::Obfuscated` rewrites string and numeric literals in `db.query.text`.
/// The query under test is dialect-neutral (`SELECT 1, 'alice', 3.14`), so the only
/// dialect input is the raw pool factory used to build a custom-configured pool.
#[macro_export]
macro_rules! test_query_text_mode_obfuscated_replaces_literals {
    ($raw_pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let _ = $dialect;
        let raw = $raw_pool_factory;
        let pool = sqlx_otel::PoolBuilder::from(raw)
            .with_query_text_mode(sqlx_otel::QueryTextMode::Obfuscated)
            .build();

        let tel = $crate::common::TestTelemetry::install();
        let _row = (&pool)
            .fetch_optional("SELECT 1, 'alice', 3.14")
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.query.text"),
            Some(opentelemetry::Value::String("SELECT ?, ?, ?".into()))
        );
    }};
}

/// `fetch_optional` against an empty table returns `None` and records `returned_rows = 0`.
/// Uses `fresh_table!` to set up a guaranteed-empty table.
#[macro_export]
macro_rules! test_fetch_optional_records_zero_rows {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Executor as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;
        $crate::fresh_table!(
            &pool,
            "empty_table",
            &format!("id {}", $dialect.id_pk_column)
        );
        tel.reset();

        let result = (&pool)
            .fetch_optional("SELECT id FROM empty_table")
            .await
            .unwrap();
        assert!(result.is_none());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_common_span_attributes(&spans[0], $dialect.system);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );
    }};
}

/// `sqlx::query(...).bind(...).bind(...).with_annotations(...).fetch_one(&pool)` with a
/// dialect-specific SELECT that adds two bound `i32` arguments and returns an `i64`.
#[macro_export]
macro_rules! test_query_bind_first_then_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let row = sqlx::query($dialect.bind_two_sum_sql)
            .bind(2_i32)
            .bind(3_i32)
            .with_annotations($crate::common::test_annotations())
            .fetch_one(&pool)
            .await
            .unwrap();
        let sum: i64 = row.try_get("sum").unwrap();
        assert_eq!(sum, 5);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Same as `test_query_bind_first_then_annotations_via_pool` but with `with_annotations`
/// applied before the binds.
#[macro_export]
macro_rules! test_query_annotations_first_then_bind_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx::Row as _;
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let row = sqlx::query($dialect.bind_two_sum_sql)
            .with_annotations($crate::common::test_annotations())
            .bind(10_i32)
            .bind(20_i32)
            .fetch_one(&pool)
            .await
            .unwrap();
        let sum: i64 = row.try_get("sum").unwrap();
        assert_eq!(sum, 30);

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Annotated `execute` against the wrapped pool. Uses `SELECT 1` so the test is
/// portable; the executor records `affected_rows` regardless of statement kind.
#[macro_export]
macro_rules! test_query_execute_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        sqlx::query("SELECT 1")
            .with_annotations($crate::common::test_annotations())
            .execute(&pool)
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert!($crate::common::attr(&spans[0], "db.response.affected_rows").is_some());
    }};
}

/// Annotated `execute` against `&mut PoolConnection<DB>`.
#[macro_export]
macro_rules! test_query_execute_with_annotations_via_connection {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("SELECT 1")
            .with_annotations($crate::common::test_annotations())
            .execute(&mut conn)
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Annotated `execute` against `&mut Transaction<'_, DB>`.
#[macro_export]
macro_rules! test_query_execute_with_annotations_via_transaction {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT 1")
            .with_annotations($crate::common::test_annotations())
            .execute(&mut tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// `with_operation` shorthand attaching the same annotations as the manual
/// `with_annotations(test_annotations())` call.
#[macro_export]
macro_rules! test_query_with_operation_shorthand_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        sqlx::query("SELECT 1")
            .with_operation("SELECT", "users")
            .execute(&pool)
            .await
            .unwrap();

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
    }};
}

/// Annotated `fetch_optional` returning `None`. Uses `SELECT 1 WHERE 1 = 0` to express
/// the empty-row case without dialect-specific table setup.
#[macro_export]
macro_rules! test_query_fetch_optional_with_annotations_via_pool {
    ($pool_factory:expr, $dialect:expr) => {{
        use sqlx_otel::QueryAnnotateExt as _;
        let tel = $crate::common::TestTelemetry::install();
        let pool = $pool_factory;

        let row = sqlx::query("SELECT 1 WHERE 1 = 0")
            .with_annotations($crate::common::test_annotations())
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(row.is_none());

        let spans = tel.spans();
        assert_eq!(spans.len(), 1);
        $crate::common::assert_annotated_span(&spans[0], &$dialect);
        assert_eq!(
            $crate::common::attr(&spans[0], "db.response.returned_rows"),
            Some(opentelemetry::Value::I64(0))
        );
    }};
}
