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
// trait-resolution overflow on stable rustc — the compiler tries to satisfy the bound
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
}

pub const SQLITE_DIALECT: Dialect = Dialect {
    system: "sqlite",
    id_pk_column: "INTEGER PRIMARY KEY",
    text_column: "TEXT NOT NULL",
};

pub const POSTGRES_DIALECT: Dialect = Dialect {
    system: "postgresql",
    id_pk_column: "INT PRIMARY KEY",
    text_column: "TEXT NOT NULL",
};

pub const MYSQL_DIALECT: Dialect = Dialect {
    system: "mysql",
    id_pk_column: "INT PRIMARY KEY",
    text_column: "VARCHAR(255) NOT NULL",
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
