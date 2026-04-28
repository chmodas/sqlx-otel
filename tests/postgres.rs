#![cfg(feature = "postgres")]

mod common;

use std::sync::OnceLock;
use std::time::Duration;

use common::{
    assert_annotated_span, assert_common_span_attributes, assert_error_span, attr, test_annotations,
};
use opentelemetry::trace::SpanKind;
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::Postgres;
use sqlx::Row as _;
use sqlx_otel::{Pool, PoolBuilder, QueryAnnotateExt, Transaction};
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

const SYSTEM: &str = "postgresql";

/// Backend row type used by parameterised map test macros (see `tests/sqlite.rs`).
type Row = sqlx::postgres::PgRow;

/// Shared container and connection URL, initialised once across all tests.
struct SharedContainer {
    _container: ContainerAsync<GenericImage>,
    url: String,
}

static CONTAINER: OnceLock<OnceCell<SharedContainer>> = OnceLock::new();

async fn shared_container() -> &'static SharedContainer {
    CONTAINER
        .get_or_init(OnceCell::new)
        .get_or_init(|| async {
            let container = GenericImage::new("postgres", "15-alpine")
                .with_wait_for(testcontainers::core::WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_exposed_port(5432.tcp())
                .with_env_var("POSTGRES_USER", "postgres")
                .with_env_var("POSTGRES_DB", "testdb")
                .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
                .with_startup_timeout(Duration::from_secs(60))
                .start()
                .await
                .expect("starting postgres container");

            let port = container.get_host_port_ipv4(5432).await.unwrap();
            let url = format!("postgres://postgres@localhost:{port}/testdb");
            SharedContainer {
                _container: container,
                url,
            }
        })
        .await
}

/// Return an instrumented pool connected to the shared container.
async fn test_pool() -> Pool<Postgres> {
    PoolBuilder::from(raw_pool().await).build()
}

/// Raw (un-instrumented) sqlx pool, used by parameterised builder / query-text-mode
/// tests that need to apply specific `PoolBuilder` configurations themselves.
async fn raw_pool() -> sqlx::PgPool {
    let shared = shared_container().await;
    sqlx::PgPool::connect(&shared.url).await.unwrap()
}

// ===========================================================================
// execute
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_creates_span_via_pool() {
    test_execute_creates_span_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_connection() {
    test_execute_creates_span_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_transaction() {
    test_execute_creates_span_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_error() {
    test_execute_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_affected_rows() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS affected_test (id INT PRIMARY KEY, name TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM affected_test")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();

    // --- Bulk insert ---
    sqlx::query(
        "INSERT INTO affected_test (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.response.affected_rows"),
        Some(opentelemetry::Value::I64(3)),
        "inserting 3 rows in one statement should affect 3 rows"
    );
    tel.reset();

    // --- Upsert (INSERT ON CONFLICT) ---
    sqlx::query(
        "INSERT INTO affected_test (id, name) VALUES (1, 'alice_updated') \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
    )
    .execute(&pool)
    .await
    .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.response.affected_rows"),
        Some(opentelemetry::Value::I64(1)),
        "upsert should affect 1 row"
    );
    tel.reset();

    // --- Update multiple rows ---
    sqlx::query("UPDATE affected_test SET name = name || '_updated' WHERE id IN (2, 3)")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.response.affected_rows"),
        Some(opentelemetry::Value::I64(2)),
        "updating two rows should affect 2 rows"
    );
    tel.reset();

    // --- Delete multiple rows ---
    sqlx::query("DELETE FROM affected_test WHERE id IN (1, 2, 3)")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.response.affected_rows"),
        Some(opentelemetry::Value::I64(3)),
        "deleting three rows should affect 3 rows"
    );
    tel.reset();

    // --- Delete with no matching rows ---
    sqlx::query("DELETE FROM affected_test WHERE id = 999")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.response.affected_rows"),
        Some(opentelemetry::Value::I64(0)),
        "deleting non-existent rows should affect 0 rows"
    );
}

// ===========================================================================
// execute_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_many_via_pool() {
    test_execute_many_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_connection() {
    test_execute_many_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_transaction() {
    test_execute_many_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_records_error() {
    test_execute_many_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// fetch
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_via_pool() {
    test_fetch_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_connection() {
    test_fetch_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_transaction() {
    test_fetch_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_stream_dropped_early_still_records_span() {
    test_fetch_stream_dropped_early_still_records_span!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn fetch_stream_records_error() {
    test_fetch_stream_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// fetch_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_many_via_pool() {
    test_fetch_many_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_connection() {
    test_fetch_many_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_transaction() {
    test_fetch_many_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_dropped_early_still_records_span() {
    test_fetch_many_dropped_early_still_records_span!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_records_error() {
    test_fetch_many_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// fetch_all
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_all_via_pool() {
    test_fetch_all_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_connection() {
    test_fetch_all_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_transaction() {
    test_fetch_all_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_records_error() {
    test_fetch_all_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// fetch_one
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_one_via_pool() {
    test_fetch_one_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_connection() {
    test_fetch_one_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_transaction() {
    test_fetch_one_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_records_error() {
    test_fetch_one_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// fetch_optional
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_optional_records_one_row() {
    test_fetch_optional_records_one_row!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_zero_rows() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE IF NOT EXISTS empty_table (id SERIAL PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM empty_table")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let result = (&pool)
        .fetch_optional("SELECT id FROM empty_table")
        .await
        .unwrap();
    assert!(result.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_connection() {
    test_fetch_optional_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_transaction() {
    test_fetch_optional_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_error() {
    test_fetch_optional_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// prepare
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_via_pool() {
    test_prepare_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_connection() {
    test_prepare_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_transaction() {
    test_prepare_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_records_error() {
    test_prepare_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// prepare_with
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_with_via_pool() {
    test_prepare_with_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_connection() {
    test_prepare_with_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_transaction() {
    test_prepare_with_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_records_error() {
    test_prepare_with_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// describe
// ===========================================================================

#[tokio::test]
#[serial]
async fn describe_via_pool() {
    test_describe_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_connection() {
    test_describe_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_transaction() {
    test_describe_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_records_error() {
    test_describe_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// Postgres-specific: connection attributes
// ===========================================================================

#[tokio::test]
#[serial]
async fn connection_attributes_populated() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _row = (&pool).fetch_one("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);

    assert_eq!(
        attr(&spans[0], "server.address"),
        Some(opentelemetry::Value::String("localhost".into()))
    );
    assert!(
        attr(&spans[0], "server.port").is_some(),
        "server.port missing"
    );
    assert_eq!(
        attr(&spans[0], "db.namespace"),
        Some(opentelemetry::Value::String("testdb".into()))
    );
}

// ===========================================================================
// Postgres-specific: SQLSTATE (db.response.status_code)
// ===========================================================================

#[tokio::test]
#[serial]
async fn sqlstate_recorded_on_constraint_violation() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    // Create a table with a unique constraint.
    sqlx::query("CREATE TABLE IF NOT EXISTS unique_test (id INT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM unique_test")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO unique_test (id) VALUES (1)")
        .execute(&pool)
        .await
        .unwrap();

    // Drop the setup spans so the assertion below sees only the violating query.
    tel.reset();

    // Insert a duplicate – should trigger SQLSTATE 23505 (unique_violation).
    let result = sqlx::query("INSERT INTO unique_test (id) VALUES (1)")
        .execute(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_error_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.status_code"),
        Some(opentelemetry::Value::String("23505".into()))
    );
}

// ===========================================================================
// Metrics
// ===========================================================================

#[tokio::test]
#[serial]
async fn operation_duration_metric_is_recorded() {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};

    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _row = (&pool).fetch_one("SELECT 1").await.unwrap();

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
}

// ===========================================================================
// Transaction rollback
// ===========================================================================

#[tokio::test]
#[serial]
async fn transaction_rollback() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Postgres> = pool.begin().await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS rollback_test (id SERIAL PRIMARY KEY)")
        .execute(&mut tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
}

// ===========================================================================
// PoolBuilder with_* methods
// ===========================================================================

#[tokio::test]
#[serial]
async fn builder_with_database_overrides_namespace() {
    test_builder_with_database_overrides_namespace!(raw_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_host_overrides_server_address() {
    test_builder_with_host_overrides_server_address!(raw_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_port_overrides_server_port() {
    test_builder_with_port_overrides_server_port!(raw_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_address() {
    test_builder_with_network_peer_address!(raw_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_port() {
    test_builder_with_network_peer_port!(raw_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// Pool close / is_closed
// ===========================================================================

#[tokio::test]
#[serial]
async fn pool_close_and_is_closed() {
    let _tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    assert!(!pool.is_closed());
    pool.close().await;
    assert!(pool.is_closed());
}

// ===========================================================================
// QueryTextMode
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_text_mode_off_suppresses_sql() {
    let shared = shared_container().await;
    let raw = sqlx::PgPool::connect(&shared.url).await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_query_text_mode(sqlx_otel::QueryTextMode::Off)
        .build();

    let tel = common::TestTelemetry::install();
    let _row = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].span_kind, SpanKind::Client);
    assert_eq!(
        attr(&spans[0], "db.system.name"),
        Some(opentelemetry::Value::String(SYSTEM.to_owned().into()))
    );
    assert!(attr(&spans[0], "db.namespace").is_some());
    assert!(
        attr(&spans[0], "db.query.text").is_none(),
        "db.query.text should not be present when QueryTextMode::Off"
    );
}

#[tokio::test]
#[serial]
async fn query_text_mode_obfuscated_replaces_literals() {
    let shared = shared_container().await;
    let raw = sqlx::PgPool::connect(&shared.url).await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_query_text_mode(sqlx_otel::QueryTextMode::Obfuscated)
        .build();

    let tel = common::TestTelemetry::install();
    let _row = (&pool)
        .fetch_optional("SELECT 1, 'alice', 3.14")
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.query.text"),
        Some(opentelemetry::Value::String("SELECT ?, ?, ?".into()))
    );
}

// ===========================================================================
// Annotations
// ===========================================================================

#[tokio::test]
#[serial]
async fn annotation_all_four_fields() {
    test_annotation_all_four_fields!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_summary_drives_span_name() {
    test_query_summary_drives_span_name!(test_pool().await, common::POSTGRES_DIALECT);
}

// ===========================================================================
// query-side annotations: sqlx::query(...).with_annotations(...).execute(&pool)
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE IF NOT EXISTS qe_pool (id SERIAL PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
    assert!(attr(&spans[0], "db.response.affected_rows").is_some());
}

#[tokio::test]
#[serial]
async fn query_execute_many_with_annotations_via_pool() {
    test_query_execute_many_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_with_annotations_via_pool() {
    test_query_fetch_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_many_with_annotations_via_pool() {
    test_query_fetch_many_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_all_with_annotations_via_pool() {
    test_query_fetch_all_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_one_with_annotations_via_pool() {
    test_query_fetch_one_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE IF NOT EXISTS qfo_pool (id INT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();

    let row = sqlx::query("SELECT id FROM qfo_pool WHERE id = 1")
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );
}

#[tokio::test]
#[serial]
async fn query_bind_first_then_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let row = sqlx::query("SELECT $1::int + $2::int AS sum")
        .bind(2_i32)
        .bind(3_i32)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    let sum: i32 = row.try_get("sum").unwrap();
    assert_eq!(sum, 5);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_annotations_first_then_bind_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let row = sqlx::query("SELECT $1::int + $2::int AS sum")
        .with_annotations(test_annotations())
        .bind(10_i32)
        .bind(20_i32)
        .fetch_one(&pool)
        .await
        .unwrap();
    let sum: i32 = row.try_get("sum").unwrap();
    assert_eq!(sum, 30);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_with_operation_shorthand_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE IF NOT EXISTS qop_pool (id SERIAL PRIMARY KEY)")
        .with_operation("SELECT", "users")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS qe_conn (id SERIAL PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&mut conn)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Postgres> = pool.begin().await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS qe_tx (id SERIAL PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_records_error() {
    test_query_execute_with_annotations_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

// --- query_as side ---------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_as_fetch_with_annotations_via_pool() {
    test_query_as_fetch_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_many_with_annotations_via_pool() {
    test_query_as_fetch_many_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_as_fetch_all_with_annotations_via_pool() {
    test_query_as_fetch_all_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_via_pool() {
    test_query_as_fetch_one_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_optional_with_annotations_via_pool() {
    test_query_as_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_records_error() {
    test_query_as_fetch_one_with_annotations_records_error!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

// --- query_scalar side -----------------------------------------------------

#[tokio::test]
#[serial]
async fn query_scalar_fetch_with_annotations_via_pool() {
    test_query_scalar_fetch_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_many_with_annotations_via_pool() {
    test_query_scalar_fetch_many_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_all_with_annotations_via_pool() {
    test_query_scalar_fetch_all_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_one_with_annotations_via_pool() {
    test_query_scalar_fetch_one_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_optional_with_annotations_via_pool() {
    test_query_scalar_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

// ===========================================================================
// query-side annotations: Map (Query::map / Query::try_map)
// ===========================================================================

// --- Per-position end-to-end ----------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_position_1_via_pool() {
    test_query_map_position_1_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_2_via_pool() {
    test_query_map_position_2_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_3_via_pool() {
    test_query_map_position_3_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_position_3_via_pool() {
    test_query_try_map_position_3_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

// --- Per-method on Map (so each forwarder body is hit) --------------------

#[tokio::test]
#[serial]
async fn map_fetch_with_annotations_via_pool() {
    test_map_fetch_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_many_with_annotations_via_pool() {
    test_map_fetch_many_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_all_with_annotations_via_pool() {
    test_map_fetch_all_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_one_with_annotations_via_pool() {
    test_map_fetch_one_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_optional_with_annotations_via_pool() {
    test_map_fetch_optional_with_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

// --- Composition (multi-map; both branches of step 4) --------------------

#[tokio::test]
#[serial]
async fn map_compose_after_annotations_via_pool() {
    test_map_compose_after_annotations_via_pool!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_try_map_compose_after_annotations_via_pool() {
    test_map_try_map_compose_after_annotations_via_pool!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

// --- Other executor receivers (smoke) -------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_connection() {
    test_query_map_with_annotations_via_connection!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_transaction() {
    test_query_map_with_annotations_via_transaction!(test_pool().await, common::POSTGRES_DIALECT);
}

// --- Error paths ----------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_records_error() {
    test_query_map_with_annotations_records_error!(test_pool().await, common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_with_annotations_propagates_mapper_error() {
    test_query_try_map_with_annotations_propagates_mapper_error!(
        test_pool().await,
        common::POSTGRES_DIALECT
    );
}

// ===========================================================================
// query-side annotations: compile-time-validated macros (`sqlx::query!()` etc.)
// ===========================================================================

const POSTGRES_MACRO_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS macro_users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)";

#[tokio::test]
#[serial]
async fn query_macro_execute_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM macro_users WHERE id = 101")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let result = sqlx::query!(
        "INSERT INTO macro_users (id, name) VALUES ($1, $2)",
        101_i32,
        "alice"
    )
    .with_annotations(test_annotations())
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(result.rows_affected(), 1);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO macro_users (id, name) VALUES (102, 'bob') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = $1", 102_i32)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.id, 102);
    assert_eq!(row.name, "bob");

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO macro_users (id, name) VALUES (103, 'carol'), (104, 'dave'), (105, 'eve') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .unwrap();

    tel.reset();
    let rows = sqlx::query!(
        "SELECT id, name FROM macro_users WHERE id BETWEEN $1 AND $2",
        103_i32,
        105_i32
    )
    .with_operation("SELECT", "users")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = $1", 99999_i32)
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

type MacroUser = common::MacroUser<i32>;

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO macro_users (id, name) VALUES (106, 'frank') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = $1",
        106_i32
    )
    .with_annotations(test_annotations())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(user.id, 106);
    assert_eq!(user.name, "frank");

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (107, 'grace'), (108, 'henry') ON CONFLICT (id) DO NOTHING")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let users = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id BETWEEN $1 AND $2",
        107_i32,
        108_i32
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(users.len(), 2);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = $1",
        99999_i32
    )
    .with_annotations(test_annotations())
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(user.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO macro_users (id, name) VALUES (109, 'irene') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&pool)
    .await
    .unwrap();

    tel.reset();
    let name: String = sqlx::query_scalar!("SELECT name FROM macro_users WHERE id = $1", 109_i32)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "irene");

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(POSTGRES_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (110, 'jack'), (111, 'kate') ON CONFLICT (id) DO NOTHING")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let ids: Vec<i32> = sqlx::query_scalar!(
        "SELECT id FROM macro_users WHERE id BETWEEN $1 AND $2 ORDER BY id",
        110_i32,
        111_i32
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ids, vec![110, 111]);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0], &common::POSTGRES_DIALECT);
}
