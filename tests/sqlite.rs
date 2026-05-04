#![cfg(feature = "sqlite")]

mod common;

use common::test_annotations;
use futures::StreamExt as _;
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::Sqlite;
use sqlx_otel::{Pool, PoolBuilder, QueryAnnotateExt};

const SYSTEM: &str = "sqlite";

/// Backend row type used by parameterised map test macros: `|row| row.get::<T, _>(idx)`.
/// Each backend defines its own alias so the shared macros can reference `Row` without
/// hardcoding a per-backend SQL row type.
type Row = sqlx::sqlite::SqliteRow;

/// Helper to create an in-memory Sqlite pool wrapped in our instrumented Pool.
async fn test_pool() -> Pool<Sqlite> {
    PoolBuilder::from(raw_pool().await).build()
}

/// Raw (un-instrumented) sqlx pool, used by parameterised builder / query-text-mode
/// tests that need to apply specific `PoolBuilder` configurations themselves.
async fn raw_pool() -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(":memory:").await.unwrap()
}

// ===========================================================================
// execute
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_creates_span_via_pool() {
    test_execute_creates_span_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_connection() {
    test_execute_creates_span_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_transaction() {
    test_execute_creates_span_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_affected_rows() {
    test_execute_records_affected_rows!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_records_error() {
    test_execute_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// execute_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_many_via_pool() {
    test_execute_many_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_connection() {
    test_execute_many_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_via_transaction() {
    test_execute_many_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn execute_many_records_error() {
    test_execute_many_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// fetch
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_via_pool() {
    test_fetch_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_connection() {
    test_fetch_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_via_transaction() {
    test_fetch_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_stream_dropped_early_still_records_span() {
    test_fetch_stream_dropped_early_still_records_span!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_stream_records_error() {
    test_fetch_stream_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// fetch_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_many_via_pool() {
    test_fetch_many_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_connection() {
    test_fetch_many_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_via_transaction() {
    test_fetch_many_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_dropped_early_still_records_span() {
    test_fetch_many_dropped_early_still_records_span!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_many_records_error() {
    test_fetch_many_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// fetch_all
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_all_via_pool() {
    test_fetch_all_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_connection() {
    test_fetch_all_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_via_transaction() {
    test_fetch_all_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_all_records_error() {
    test_fetch_all_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// fetch_one
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_one_via_pool() {
    test_fetch_one_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_connection() {
    test_fetch_one_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_via_transaction() {
    test_fetch_one_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_one_records_error() {
    test_fetch_one_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// fetch_optional
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_optional_records_one_row() {
    test_fetch_optional_records_one_row!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_zero_rows() {
    test_fetch_optional_records_zero_rows!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_connection() {
    test_fetch_optional_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_transaction() {
    test_fetch_optional_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_error() {
    test_fetch_optional_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// prepare
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_via_pool() {
    test_prepare_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_connection() {
    test_prepare_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_via_transaction() {
    test_prepare_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_records_error() {
    test_prepare_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// prepare_with
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_with_via_pool() {
    test_prepare_with_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_connection() {
    test_prepare_with_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_via_transaction() {
    test_prepare_with_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn prepare_with_records_error() {
    test_prepare_with_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// describe
// ===========================================================================

#[tokio::test]
#[serial]
async fn describe_via_pool() {
    test_describe_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_connection() {
    test_describe_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_via_transaction() {
    test_describe_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn describe_records_error() {
    test_describe_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// Metrics
// ===========================================================================
// Per-method coverage of `db.client.operation.duration` lives inside each
// `test_<method>_*` macro via `assert_metric_for_system` / `assert_annotated_metric` /
// `assert_error_metric`. The targeted SQLSTATE assertion below pins the backend-specific
// `db.response.status_code` value because that dimension varies per driver and would
// otherwise need a per-backend value plumbed into every error-path macro.

#[tokio::test]
#[serial]
async fn operation_duration_metric_carries_full_annotations() {
    test_operation_duration_metric_carries_full_annotations!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn operation_duration_metric_carries_sqlstate() {
    // SQLite extended result code `1` = SQLITE_ERROR (returned for `no such table`).
    test_operation_duration_metric_carries_sqlstate!(test_pool().await, "1");
}

// ===========================================================================
// QueryTextMode
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_text_mode_off_suppresses_sql() {
    test_query_text_mode_off_suppresses_sql!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_text_mode_obfuscated_replaces_literals() {
    test_query_text_mode_obfuscated_replaces_literals!(raw_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// Transaction rollback
// ===========================================================================

#[tokio::test]
#[serial]
async fn transaction_rollback() {
    test_transaction_rollback!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// PoolBuilder with_* methods
// ===========================================================================

#[tokio::test]
#[serial]
async fn builder_with_database_overrides_namespace() {
    test_builder_with_database_overrides_namespace!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_host_overrides_server_address() {
    test_builder_with_host_overrides_server_address!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_port_overrides_server_port() {
    test_builder_with_port_overrides_server_port!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_address() {
    test_builder_with_network_peer_address!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_port() {
    test_builder_with_network_peer_port!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_pool_name_propagates_to_span_and_metric() {
    test_builder_with_pool_name_propagates_to_span_and_metric!(
        raw_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn builder_default_network_protocol_name_absent_for_sqlite() {
    // SQLite is an embedded backend with no wire protocol; default omits
    // `network.protocol.name`.
    let expected: Option<&str> = None;
    test_builder_default_network_protocol_name!(raw_pool().await, expected);
}

#[tokio::test]
#[serial]
async fn builder_with_network_protocol_name_overrides() {
    test_builder_with_network_protocol_name_overrides!(raw_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn builder_with_network_transport() {
    test_builder_with_network_transport!(raw_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// Pool close / is_closed
// ===========================================================================

#[tokio::test]
#[serial]
async fn pool_close_and_is_closed() {
    test_pool_close_and_is_closed!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// Annotations
// ===========================================================================

#[tokio::test]
#[serial]
async fn annotation_all_four_fields() {
    test_annotation_all_four_fields!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_summary_drives_span_name() {
    test_query_summary_drives_span_name!(test_pool().await, common::SQLITE_DIALECT);
}

// ===========================================================================
// query-side annotations: sqlx::query(...).with_annotations(...).execute(&pool)
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_pool() {
    test_query_execute_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_many_with_annotations_via_pool() {
    test_query_execute_many_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_with_annotations_via_pool() {
    test_query_fetch_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_many_with_annotations_via_pool() {
    test_query_fetch_many_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_all_with_annotations_via_pool() {
    test_query_fetch_all_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_one_with_annotations_via_pool() {
    test_query_fetch_one_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_fetch_optional_with_annotations_via_pool() {
    test_query_fetch_optional_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_bind_first_then_annotations_via_pool() {
    test_query_bind_first_then_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_annotations_first_then_bind_via_pool() {
    test_query_annotations_first_then_bind_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_with_operation_shorthand_via_pool() {
    test_query_with_operation_shorthand_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_connection() {
    test_query_execute_with_annotations_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_transaction() {
    test_query_execute_with_annotations_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_records_error() {
    test_query_execute_with_annotations_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

// --- query_as side ---------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_as_fetch_with_annotations_via_pool() {
    test_query_as_fetch_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_many_with_annotations_via_pool() {
    test_query_as_fetch_many_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_all_with_annotations_via_pool() {
    test_query_as_fetch_all_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_via_pool() {
    test_query_as_fetch_one_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_optional_with_annotations_via_pool() {
    test_query_as_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_records_error() {
    test_query_as_fetch_one_with_annotations_records_error!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

// --- query_scalar side -----------------------------------------------------

#[tokio::test]
#[serial]
async fn query_scalar_fetch_with_annotations_via_pool() {
    test_query_scalar_fetch_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_many_with_annotations_via_pool() {
    test_query_scalar_fetch_many_with_annotations_via_pool!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_all_with_annotations_via_pool() {
    test_query_scalar_fetch_all_with_annotations_via_pool!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_one_with_annotations_via_pool() {
    test_query_scalar_fetch_one_with_annotations_via_pool!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_optional_with_annotations_via_pool() {
    test_query_scalar_fetch_optional_with_annotations_via_pool!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

// ===========================================================================
// query-side annotations: Map (Query::map / Query::try_map)
// ===========================================================================

// --- Per-position end-to-end ----------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_position_1_via_pool() {
    test_query_map_position_1_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_2_via_pool() {
    test_query_map_position_2_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_position_3_via_pool() {
    test_query_map_position_3_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_position_3_via_pool() {
    test_query_try_map_position_3_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

// --- Per-method on Map (so each forwarder body is hit) --------------------

#[tokio::test]
#[serial]
async fn map_fetch_with_annotations_via_pool() {
    test_map_fetch_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_many_with_annotations_via_pool() {
    test_map_fetch_many_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_all_with_annotations_via_pool() {
    test_map_fetch_all_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_one_with_annotations_via_pool() {
    test_map_fetch_one_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_fetch_optional_with_annotations_via_pool() {
    test_map_fetch_optional_with_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

// --- Composition (multi-map; both branches of step 4) --------------------

#[tokio::test]
#[serial]
async fn map_compose_after_annotations_via_pool() {
    test_map_compose_after_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn map_try_map_compose_after_annotations_via_pool() {
    test_map_try_map_compose_after_annotations_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}

// --- Other executor receivers (smoke) -------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_connection() {
    test_query_map_with_annotations_via_connection!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_transaction() {
    test_query_map_with_annotations_via_transaction!(test_pool().await, common::SQLITE_DIALECT);
}

// --- Error paths ----------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_records_error() {
    test_query_map_with_annotations_records_error!(test_pool().await, common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_try_map_with_annotations_propagates_mapper_error() {
    test_query_try_map_with_annotations_propagates_mapper_error!(
        test_pool().await,
        common::SQLITE_DIALECT
    );
}

// ===========================================================================
// query-side annotations: compile-time-validated macros (`sqlx::query!()` etc.)
// ===========================================================================
//
// These tests exercise the macro path end-to-end: the macro expands to either
// `Query<'_, DB, _>` (no result columns) or `Map<'_, DB, _, _>` (with columns),
// and the wrapper supports both via the same `QueryAnnotateExt` impls used by
// hand-written queries. Each test inserts a row with a unique primary key so
// the shared `macro_users` table can be reused across the suite.

const SQLITE_MACRO_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS macro_users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)";

#[tokio::test]
#[serial]
async fn query_macro_execute_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let result = sqlx::query!(
        "INSERT INTO macro_users (id, name) VALUES (?1, ?2)",
        1_i64,
        "alice"
    )
    .with_annotations(test_annotations())
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(result.rows_affected(), 1);

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (2, 'bob')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?1", 2_i64)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.id, 2);
    assert_eq!(row.name, "bob");

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (3, 'carol'), (4, 'dave'), (5, 'eve')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let rows = sqlx::query!(
        "SELECT id, name FROM macro_users WHERE id BETWEEN ?1 AND ?2",
        3_i64,
        5_i64
    )
    .with_operation("SELECT", "users")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?1", 9999_i64)
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

type MacroUser = common::MacroUser<i64>;

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (6, 'frank')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = ?1",
        6_i64
    )
    .with_annotations(test_annotations())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(user.id, 6);
    assert_eq!(user.name, "frank");

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (7, 'grace'), (8, 'henry')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let users = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id BETWEEN ?1 AND ?2",
        7_i64,
        8_i64
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(users.len(), 2);

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let user = sqlx::query_as!(
        MacroUser,
        "SELECT id, name FROM macro_users WHERE id = ?1",
        9999_i64
    )
    .with_annotations(test_annotations())
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(user.is_none());

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (9, 'irene')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let name: String = sqlx::query_scalar!("SELECT name FROM macro_users WHERE id = ?1", 9_i64)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "irene");

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (10, 'jack'), (11, 'kate')")
        .execute(&pool)
        .await
        .unwrap();

    tel.reset();
    let ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT id FROM macro_users WHERE id BETWEEN ?1 AND ?2 ORDER BY id",
        10_i64,
        11_i64
    )
    .with_annotations(test_annotations())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ids, vec![10, 11]);

    common::assert_one_annotated_span(&tel, &common::SQLITE_DIALECT);
}

// ===========================================================================
// Trace context propagation
//
// These tests pin the executor's behaviour around the ambient OpenTelemetry context:
// when a caller has an active span in scope, the query span the executor creates
// becomes its child (same `trace_id`, `parent_span_id` equal to the caller's span id).
// The propagation logic lives in `start_span` (`src/executor.rs`), which builds spans
// via `tracer.span_builder(...).start(&tracer)`. `start` adopts whatever
// `Context::current()` carries at call time, so this is what we verify here.
//
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_span_inherits_parent_context_unary() {
    use opentelemetry::trace::{Span as _, TraceContextExt, Tracer, TracerProvider};

    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("test-outer");
    let outer_span = tracer.start("outer");
    let outer_ctx = outer_span.span_context().clone();
    let cx = opentelemetry::Context::current_with_span(outer_span);

    {
        let _guard = cx.clone().attach();
        (&pool).execute("SELECT 1").await.unwrap();
    }
    drop(cx);

    let mut spans = tel.spans();
    spans.sort_by_key(|s| s.start_time);
    assert_eq!(spans.len(), 2, "outer + query span");

    let query_span = spans
        .iter()
        .find(|s| s.name == SYSTEM)
        .expect("query span (named after system) should be present");
    assert_eq!(
        query_span.span_context.trace_id(),
        outer_ctx.trace_id(),
        "query span trace_id should match the outer context's trace_id",
    );
    assert_eq!(
        query_span.parent_span_id,
        outer_ctx.span_id(),
        "query span parent_span_id should equal the outer span's span_id",
    );
}

#[tokio::test]
#[serial]
async fn query_span_inherits_parent_context_streaming() {
    use opentelemetry::trace::{Span as _, TraceContextExt, Tracer, TracerProvider};

    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("test-outer");
    let outer_span = tracer.start("outer");
    let outer_ctx = outer_span.span_context().clone();
    let cx = opentelemetry::Context::current_with_span(outer_span);

    {
        let _guard = cx.clone().attach();
        let mut stream = (&pool).fetch("SELECT 1");
        while stream.next().await.is_some() {}
    }
    drop(cx);

    let mut spans = tel.spans();
    spans.sort_by_key(|s| s.start_time);
    assert_eq!(spans.len(), 2, "outer + query span");

    let query_span = spans
        .iter()
        .find(|s| s.name == SYSTEM)
        .expect("query span (named after system) should be present");
    assert_eq!(
        query_span.span_context.trace_id(),
        outer_ctx.trace_id(),
        "stream query span trace_id should match the outer context's trace_id",
    );
    assert_eq!(
        query_span.parent_span_id,
        outer_ctx.span_id(),
        "stream query span parent_span_id should equal the outer span's span_id",
    );
}

#[tokio::test]
#[serial]
async fn executor_side_query_side_parity_via_pool() {
    test_executor_side_query_side_parity_via_pool!(test_pool().await, common::SQLITE_DIALECT);
}
