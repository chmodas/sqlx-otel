#![cfg(feature = "mysql")]

mod common;

use std::sync::OnceLock;
use std::time::Duration;

use common::{assert_error_span, attr, test_annotations};
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::MySql;
use sqlx_otel::{Pool, PoolBuilder, QueryAnnotateExt};
use testcontainers::core::IntoContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

/// Backend row type used by parameterised map test macros (see `tests/sqlite.rs`).
type Row = sqlx::mysql::MySqlRow;

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
            let container = GenericImage::new("mysql", "8")
                .with_wait_for(testcontainers::core::WaitFor::message_on_stderr(
                    "port: 3306",
                ))
                .with_exposed_port(3306.tcp())
                .with_env_var("MYSQL_ROOT_PASSWORD", "test")
                .with_env_var("MYSQL_DATABASE", "testdb")
                .with_env_var("MYSQL_ALLOW_EMPTY_PASSWORD", "no")
                .with_startup_timeout(Duration::from_secs(120))
                .start()
                .await
                .expect("starting mysql container");

            let port = container.get_host_port_ipv4(3306).await.unwrap();
            let url = format!("mysql://root:test@localhost:{port}/testdb");
            SharedContainer {
                _container: container,
                url,
            }
        })
        .await
}

/// Return an instrumented pool connected to the shared container.
async fn test_pool() -> Pool<MySql> {
    PoolBuilder::from(raw_pool().await).build()
}

/// Raw (un-instrumented) sqlx pool, used by parameterised builder / query-text-mode
/// tests that need to apply specific `PoolBuilder` configurations themselves.
async fn raw_pool() -> sqlx::MySqlPool {
    let shared = shared_container().await;
    sqlx::MySqlPool::connect(&shared.url).await.unwrap()
}

// ===========================================================================
// execute
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_creates_span_via_pool() {
    test_execute_creates_span_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_connection() {
    test_execute_creates_span_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_transaction() {
    test_execute_creates_span_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_error() {
    test_execute_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_affected_rows() {
    test_execute_records_affected_rows!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// execute_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_many_via_pool() {
    test_execute_many_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_connection() {
    test_execute_many_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_transaction() {
    test_execute_many_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_records_error() {
    test_execute_many_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// fetch
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_via_pool() {
    test_fetch_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_connection() {
    test_fetch_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_transaction() {
    test_fetch_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_stream_dropped_early_still_records_span() {
    test_fetch_stream_dropped_early_still_records_span!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_stream_records_error() {
    test_fetch_stream_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// fetch_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_many_via_pool() {
    test_fetch_many_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_connection() {
    test_fetch_many_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_transaction() {
    test_fetch_many_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_dropped_early_still_records_span() {
    test_fetch_many_dropped_early_still_records_span!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_records_error() {
    test_fetch_many_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// fetch_all
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_all_via_pool() {
    test_fetch_all_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_connection() {
    test_fetch_all_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_transaction() {
    test_fetch_all_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_records_error() {
    test_fetch_all_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// fetch_one
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_one_via_pool() {
    test_fetch_one_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_connection() {
    test_fetch_one_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_transaction() {
    test_fetch_one_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_records_error() {
    test_fetch_one_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// fetch_optional
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_optional_records_one_row() {
    test_fetch_optional_records_one_row!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_zero_rows() {
    test_fetch_optional_records_zero_rows!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_connection() {
    test_fetch_optional_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_transaction() {
    test_fetch_optional_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_error() {
    test_fetch_optional_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// prepare
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_via_pool() {
    test_prepare_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_connection() {
    test_prepare_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_transaction() {
    test_prepare_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_records_error() {
    test_prepare_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// prepare_with
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_with_via_pool() {
    test_prepare_with_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_connection() {
    test_prepare_with_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_transaction() {
    test_prepare_with_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_records_error() {
    test_prepare_with_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// describe
// ===========================================================================

#[tokio::test]
#[serial]
async fn describe_via_pool() {
    test_describe_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_connection() {
    test_describe_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_transaction() {
    test_describe_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_records_error() {
    test_describe_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// MySQL-specific: connection attributes
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
// MySQL-specific: SQLSTATE (db.response.status_code)
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

    // Insert a duplicate – should trigger SQLSTATE 23000 (integrity constraint violation).
    let result = sqlx::query("INSERT INTO unique_test (id) VALUES (1)")
        .execute(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_error_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.status_code"),
        Some(opentelemetry::Value::String("23000".into()))
    );
}

// ===========================================================================
// Metrics
// ===========================================================================

#[tokio::test]
#[serial]
async fn operation_duration_metric_is_recorded() {
    test_operation_duration_metric_is_recorded!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// Transaction rollback
// ===========================================================================

#[tokio::test]
#[serial]
async fn transaction_rollback() {
    test_transaction_rollback!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// PoolBuilder with_* methods
// ===========================================================================

#[tokio::test]
#[serial]
async fn builder_with_database_overrides_namespace() {
    test_builder_with_database_overrides_namespace!(raw_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_host_overrides_server_address() {
    test_builder_with_host_overrides_server_address!(raw_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_port_overrides_server_port() {
    test_builder_with_port_overrides_server_port!(raw_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_address() {
    test_builder_with_network_peer_address!(raw_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_port() {
    test_builder_with_network_peer_port!(raw_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// Pool close / is_closed
// ===========================================================================

#[tokio::test]
#[serial]
async fn pool_close_and_is_closed() {
    test_pool_close_and_is_closed!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// QueryTextMode
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_text_mode_off_suppresses_sql() {
    test_query_text_mode_off_suppresses_sql!(raw_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_text_mode_obfuscated_replaces_literals() {
    test_query_text_mode_obfuscated_replaces_literals!(raw_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// Annotations
// ===========================================================================

#[tokio::test]
#[serial]
async fn annotation_all_four_fields() {
    test_annotation_all_four_fields!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_summary_drives_span_name() {
    test_query_summary_drives_span_name!(test_pool().await, common::MYSQL_DIALECT);
}

// ===========================================================================
// query-side annotations: sqlx::query(...).with_annotations(...).execute(&pool)
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_pool() {
    test_query_execute_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_many_with_annotations_via_pool() {
    test_query_execute_many_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_with_annotations_via_pool() {
    test_query_fetch_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_many_with_annotations_via_pool() {
    test_query_fetch_many_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_all_with_annotations_via_pool() {
    test_query_fetch_all_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_one_with_annotations_via_pool() {
    test_query_fetch_one_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_optional_with_annotations_via_pool() {
    test_query_fetch_optional_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_bind_first_then_annotations_via_pool() {
    test_query_bind_first_then_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_annotations_first_then_bind_via_pool() {
    test_query_annotations_first_then_bind_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_with_operation_shorthand_via_pool() {
    test_query_with_operation_shorthand_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_connection() {
    test_query_execute_with_annotations_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_transaction() {
    test_query_execute_with_annotations_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_records_error() {
    test_query_execute_with_annotations_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

// --- query_as side ---------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_as_fetch_with_annotations_via_pool() {
    test_query_as_fetch_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_many_with_annotations_via_pool() {
    test_query_as_fetch_many_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_all_with_annotations_via_pool() {
    test_query_as_fetch_all_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_via_pool() {
    test_query_as_fetch_one_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_optional_with_annotations_via_pool() {
    test_query_as_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_records_error() {
    test_query_as_fetch_one_with_annotations_records_error!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

// --- query_scalar side -----------------------------------------------------

#[tokio::test]
#[serial]
async fn query_scalar_fetch_with_annotations_via_pool() {
    test_query_scalar_fetch_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_many_with_annotations_via_pool() {
    test_query_scalar_fetch_many_with_annotations_via_pool!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_all_with_annotations_via_pool() {
    test_query_scalar_fetch_all_with_annotations_via_pool!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_one_with_annotations_via_pool() {
    test_query_scalar_fetch_one_with_annotations_via_pool!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_optional_with_annotations_via_pool() {
    test_query_scalar_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

// ===========================================================================
// query-side annotations: Map (Query::map / Query::try_map)
// ===========================================================================
//
// MySQL coerces arithmetic on bind parameters to DOUBLE, so any test that passes a value
// through `?` and decodes it as `i64` wraps the expression in `CAST(? AS SIGNED)`. Plain
// integer literals (`SELECT 19`) are returned as `BIGINT` and decode straight to `i64`.

// --- Per-position end-to-end ----------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_position_1_via_pool() {
    test_query_map_position_1_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_2_via_pool() {
    test_query_map_position_2_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_3_via_pool() {
    test_query_map_position_3_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_position_3_via_pool() {
    test_query_try_map_position_3_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

// --- Per-method on Map (so each forwarder body is hit) --------------------

#[tokio::test]
#[serial]
async fn map_fetch_with_annotations_via_pool() {
    test_map_fetch_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_many_with_annotations_via_pool() {
    test_map_fetch_many_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_all_with_annotations_via_pool() {
    test_map_fetch_all_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_one_with_annotations_via_pool() {
    test_map_fetch_one_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_optional_with_annotations_via_pool() {
    test_map_fetch_optional_with_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

// --- Composition (multi-map; both branches of step 4) --------------------

#[tokio::test]
#[serial]
async fn map_compose_after_annotations_via_pool() {
    test_map_compose_after_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_try_map_compose_after_annotations_via_pool() {
    test_map_try_map_compose_after_annotations_via_pool!(test_pool().await, common::MYSQL_DIALECT);
}

// --- Other executor receivers (smoke) -------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_connection() {
    test_query_map_with_annotations_via_connection!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_transaction() {
    test_query_map_with_annotations_via_transaction!(test_pool().await, common::MYSQL_DIALECT);
}

// --- Error paths ----------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_records_error() {
    test_query_map_with_annotations_records_error!(test_pool().await, common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_with_annotations_propagates_mapper_error() {
    test_query_try_map_with_annotations_propagates_mapper_error!(
        test_pool().await,
        common::MYSQL_DIALECT
    );
}

// ===========================================================================
// query-side annotations: compile-time-validated macros (`sqlx::query!()` etc.)
// ===========================================================================

const MYSQL_MACRO_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS macro_users (id INT PRIMARY KEY, name VARCHAR(255) NOT NULL)";

#[tokio::test]
#[serial]
async fn query_macro_execute_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM macro_users WHERE id = 201")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let result = sqlx::query!(
        "INSERT INTO macro_users (id, name) VALUES (?, ?)",
        201_i32,
        "alice"
    )
    .with_annotations(test_annotations())
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(result.rows_affected(), 1);

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO macro_users (id, name) VALUES (202, 'bob')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?", 202_i32)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.id, 202);
    assert_eq!(row.name, "bob");

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT IGNORE INTO macro_users (id, name) VALUES (203, 'carol'), (204, 'dave'), (205, 'eve')",
    )
    .execute(&pool)
    .await
    .unwrap();

    tel.reset();
    let rows = sqlx::query!(
        "SELECT id, name FROM macro_users WHERE id BETWEEN ? AND ?",
        203_i32,
        205_i32
    )
    .with_operation("SELECT", "users")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?", 99999_i32)
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

type MacroUser = common::MacroUser<i32>;

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO macro_users (id, name) VALUES (206, 'frank')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = ?",
        206_i32
    )
    .with_annotations(test_annotations())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(user.id, 206);
    assert_eq!(user.name, "frank");

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO macro_users (id, name) VALUES (207, 'grace'), (208, 'henry')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let users = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id BETWEEN ? AND ?",
        207_i32,
        208_i32
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(users.len(), 2);

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = ?",
        99999_i32
    )
    .with_annotations(test_annotations())
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(user.is_none());

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO macro_users (id, name) VALUES (209, 'irene')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let name: String = sqlx::query_scalar!("SELECT name FROM macro_users WHERE id = ?", 209_i32)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "irene");

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(MYSQL_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO macro_users (id, name) VALUES (210, 'jack'), (211, 'kate')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let ids: Vec<i32> = sqlx::query_scalar!(
        "SELECT id FROM macro_users WHERE id BETWEEN ? AND ? ORDER BY id",
        210_i32,
        211_i32
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ids, vec![210, 211]);

    common::assert_one_annotated_span(&tel, &common::MYSQL_DIALECT);
}
