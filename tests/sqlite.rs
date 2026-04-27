#![cfg(feature = "sqlite")]

mod common;

use common::{assert_common_span_attributes, assert_error_span, attr};
use futures::StreamExt;
use opentelemetry::trace::SpanKind;
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::Row as _;
use sqlx::Sqlite;
use sqlx_otel::{Pool, PoolBuilder, QueryAnnotateExt, QueryAnnotations, Transaction};

const SYSTEM: &str = "sqlite";

/// Helper to create an in-memory Sqlite pool wrapped in our instrumented Pool.
async fn test_pool() -> Pool<Sqlite> {
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    PoolBuilder::from(raw).build()
}

/// Standard annotations used across most annotation tests.
fn test_annotations() -> QueryAnnotations {
    QueryAnnotations::new()
        .operation("SELECT")
        .collection("users")
}

/// Assert that the span carries the standard annotation attributes set by
/// [`test_annotations`].
fn assert_annotated_span(span: &opentelemetry_sdk::trace::SpanData) {
    assert_eq!(span.span_kind, SpanKind::Client);
    assert_eq!(span.name, "SELECT users");
    assert_eq!(
        attr(span, "db.system.name"),
        Some(opentelemetry::Value::String(SYSTEM.to_owned().into())),
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

// ===========================================================================
// execute
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_creates_span_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    (&pool)
        .execute("CREATE TABLE exec_pool (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert!(attr(&spans[0], "db.response.affected_rows").is_some());

    // With annotations
    pool.with_annotations(test_annotations())
        .execute("CREATE TABLE exec_pool2 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .execute("CREATE TABLE exec_pool3 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("CREATE TABLE exec_conn (id INTEGER PRIMARY KEY)")
        .execute(&mut conn)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert!(attr(&spans[0], "db.response.affected_rows").is_some());

    // With annotations
    conn.with_annotations(test_annotations())
        .execute("CREATE TABLE exec_conn2 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .execute("CREATE TABLE exec_conn3 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn execute_creates_span_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    sqlx::query("CREATE TABLE exec_tx (id INTEGER PRIMARY KEY)")
        .execute(&mut tx)
        .await
        .unwrap();

    // With annotations
    tx.with_annotations(test_annotations())
        .execute("CREATE TABLE exec_tx2 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .execute("CREATE TABLE exec_tx3 (id INTEGER PRIMARY KEY)")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert!(attr(&spans[0], "db.response.affected_rows").is_some());
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn execute_records_affected_rows() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE affected_test (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();

    // --- Bulk insert via VALUES list ---
    let tel = common::TestTelemetry::install();

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

    // --- Upsert (INSERT OR REPLACE) ---
    let tel = common::TestTelemetry::install();

    sqlx::query("INSERT OR REPLACE INTO affected_test (id, name) VALUES (1, 'alice_updated')")
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

    // --- Update multiple rows ---
    let tel = common::TestTelemetry::install();

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

    // --- Delete multiple rows ---
    let tel = common::TestTelemetry::install();

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

    // --- Delete with no matching rows ---
    let tel = common::TestTelemetry::install();

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

#[tokio::test]
#[serial]
async fn execute_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = sqlx::query("INVALID SQL GIBBERISH").execute(&pool).await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = pool
        .with_annotations(test_annotations())
        .execute("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = pool
        .with_operation("SELECT", "users")
        .execute("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// execute_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_many_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = (&pool).execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );

    // With annotations
    let mut stream = pool
        .with_annotations(test_annotations())
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = pool
        .with_operation("SELECT", "users")
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn execute_many_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let mut stream = (&mut conn).execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );

    // With annotations
    let mut stream = conn
        .with_annotations(test_annotations())
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = conn
        .with_operation("SELECT", "users")
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn execute_many_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let mut stream = (&mut tx).execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With annotations
    let mut stream = tx
        .with_annotations(test_annotations())
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With shorthand
    let mut stream = tx
        .with_operation("SELECT", "users")
        .execute_many("SELECT 1; SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn execute_many_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = (&pool).execute_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );

    // With annotations (error path)
    let mut stream = pool
        .with_annotations(test_annotations())
        .execute_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let mut stream = pool
        .with_operation("SELECT", "users")
        .execute_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// fetch
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = (&pool).fetch("SELECT 1 UNION ALL SELECT 2");
    let mut count = 0u64;
    while stream.next().await.is_some() {
        count += 1;
    }
    assert_eq!(count, 2);
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );

    // With annotations
    let mut stream = pool
        .with_annotations(test_annotations())
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = pool
        .with_operation("SELECT", "users")
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let mut stream = (&mut conn).fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );

    // With annotations
    let mut stream = conn
        .with_annotations(test_annotations())
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = conn
        .with_operation("SELECT", "users")
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let mut stream = (&mut tx).fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With annotations
    let mut stream = tx
        .with_annotations(test_annotations())
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With shorthand
    let mut stream = tx
        .with_operation("SELECT", "users")
        .fetch("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn fetch_stream_dropped_early_still_records_span() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

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
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
}

#[tokio::test]
#[serial]
async fn fetch_stream_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = (&pool).fetch("INVALID SQL");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );

    // With annotations (error path)
    let mut stream = pool
        .with_annotations(test_annotations())
        .fetch("INVALID SQL");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let mut stream = pool.with_operation("SELECT", "users").fetch("INVALID SQL");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// fetch_many
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_many_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

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
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );

    // With annotations
    let mut stream = pool
        .with_annotations(test_annotations())
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = pool
        .with_operation("SELECT", "users")
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_many_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let mut stream = (&mut conn).fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );

    // With annotations
    let mut stream = conn
        .with_annotations(test_annotations())
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    let mut stream = conn
        .with_operation("SELECT", "users")
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_many_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let mut stream = (&mut tx).fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With annotations
    let mut stream = tx
        .with_annotations(test_annotations())
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    // With shorthand
    let mut stream = tx
        .with_operation("SELECT", "users")
        .fetch_many("SELECT 1 UNION ALL SELECT 2");
    while stream.next().await.is_some() {}
    drop(stream);

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn fetch_many_dropped_early_still_records_span() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

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
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
}

#[tokio::test]
#[serial]
async fn fetch_many_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = (&pool).fetch_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );

    // With annotations (error path)
    let mut stream = pool
        .with_annotations(test_annotations())
        .fetch_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let mut stream = pool
        .with_operation("SELECT", "users")
        .fetch_many("INVALID SQL GIBBERISH");
    let result = stream.next().await;
    assert!(result.is_some_and(|r| r.is_err()));
    drop(stream);
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// fetch_all
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_all_records_row_count() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let rows = (&pool)
        .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(3))
    );

    // With annotations
    pool.with_annotations(test_annotations())
        .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .fetch_all("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_all_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let rows = (&mut conn)
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );

    // With annotations
    conn.with_annotations(test_annotations())
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_all_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let rows = (&mut tx)
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    // With annotations
    tx.with_annotations(test_annotations())
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .fetch_all("SELECT 1 UNION ALL SELECT 2")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn fetch_all_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = (&pool).fetch_all("INVALID SQL GIBBERISH").await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = pool
        .with_annotations(test_annotations())
        .fetch_all("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = pool
        .with_operation("SELECT", "users")
        .fetch_all("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// fetch_one
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_one_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _row = (&pool).fetch_one("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );

    // With annotations
    pool.with_annotations(test_annotations())
        .fetch_one("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .fetch_one("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_one_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let _row = (&mut conn).fetch_one("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );

    // With annotations
    conn.with_annotations(test_annotations())
        .fetch_one("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .fetch_one("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_one_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _row = (&mut tx).fetch_one("SELECT 1").await.unwrap();

    // With annotations
    tx.with_annotations(test_annotations())
        .fetch_one("SELECT 1")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .fetch_one("SELECT 1")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn fetch_one_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = (&pool).fetch_one("INVALID SQL GIBBERISH").await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = pool
        .with_annotations(test_annotations())
        .fetch_one("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = pool
        .with_operation("SELECT", "users")
        .fetch_one("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// fetch_optional
// ===========================================================================

#[tokio::test]
#[serial]
async fn fetch_optional_records_one_row() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = (&pool).fetch_optional("SELECT 1").await.unwrap();
    assert!(result.is_some());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );

    // With annotations
    pool.with_annotations(test_annotations())
        .fetch_optional("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .fetch_optional("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_zero_rows() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE empty_table (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();

    let result = (&pool)
        .fetch_optional("SELECT id FROM empty_table")
        .await
        .unwrap();
    assert!(result.is_none());

    let spans = tel.spans();
    let select_span = spans
        .iter()
        .find(|s| attr(s, "db.query.text").is_some_and(|v| v.to_string().contains("SELECT")));
    assert!(select_span.is_some());
    let select_span = select_span.unwrap();
    assert_common_span_attributes(select_span, SYSTEM);
    assert_eq!(
        attr(select_span, "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(0))
    );
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let result = (&mut conn).fetch_optional("SELECT 42").await.unwrap();
    assert!(result.is_some());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );

    // With annotations
    conn.with_annotations(test_annotations())
        .fetch_optional("SELECT 42")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .fetch_optional("SELECT 42")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let result = (&mut tx).fetch_optional("SELECT 99").await.unwrap();
    assert!(result.is_some());

    // With annotations
    tx.with_annotations(test_annotations())
        .fetch_optional("SELECT 99")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .fetch_optional("SELECT 99")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn fetch_optional_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = (&pool).fetch_optional("INVALID SQL GIBBERISH").await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert_eq!(attr(&spans[0], "db.response.returned_rows"), None);

    // With annotations (error path)
    let result = pool
        .with_annotations(test_annotations())
        .fetch_optional("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = pool
        .with_operation("SELECT", "users")
        .fetch_optional("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// prepare
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _stmt = (&pool).prepare("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    pool.with_annotations(test_annotations())
        .prepare("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .prepare("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn prepare_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let _stmt = (&mut conn).prepare("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    conn.with_annotations(test_annotations())
        .prepare("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .prepare("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn prepare_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _stmt = (&mut tx).prepare("SELECT 1").await.unwrap();

    // With annotations
    tx.with_annotations(test_annotations())
        .prepare("SELECT 1")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .prepare("SELECT 1")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn prepare_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let result = (&mut conn).prepare("INVALID SQL GIBBERISH").await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = conn
        .with_annotations(test_annotations())
        .prepare("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = conn
        .with_operation("SELECT", "users")
        .prepare("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// prepare_with
// ===========================================================================

#[tokio::test]
#[serial]
async fn prepare_with_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _stmt = (&pool).prepare_with("SELECT ?", &[]).await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    pool.with_annotations(test_annotations())
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn prepare_with_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let _stmt = (&mut conn).prepare_with("SELECT ?", &[]).await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    conn.with_annotations(test_annotations())
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn prepare_with_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _stmt = (&mut tx).prepare_with("SELECT ?", &[]).await.unwrap();

    // With annotations
    tx.with_annotations(test_annotations())
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .prepare_with("SELECT ?", &[])
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn prepare_with_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let result = (&mut conn).prepare_with("INVALID SQL GIBBERISH", &[]).await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = conn
        .with_annotations(test_annotations())
        .prepare_with("INVALID SQL GIBBERISH", &[])
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = conn
        .with_operation("SELECT", "users")
        .prepare_with("INVALID SQL GIBBERISH", &[])
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
}

// ===========================================================================
// describe
// ===========================================================================

#[tokio::test]
#[serial]
async fn describe_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _desc = (&pool).describe("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    pool.with_annotations(test_annotations())
        .describe("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    pool.with_operation("SELECT", "users")
        .describe("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn describe_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let _desc = (&mut conn).describe("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations
    conn.with_annotations(test_annotations())
        .describe("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());

    // With shorthand
    conn.with_operation("SELECT", "users")
        .describe("SELECT 1")
        .await
        .unwrap();
    assert_annotated_span(tel.spans().last().unwrap());
}

#[tokio::test]
#[serial]
async fn describe_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _desc = (&mut tx).describe("SELECT 1").await.unwrap();

    // With annotations
    tx.with_annotations(test_annotations())
        .describe("SELECT 1")
        .await
        .unwrap();

    // With shorthand
    tx.with_operation("SELECT", "users")
        .describe("SELECT 1")
        .await
        .unwrap();

    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 3);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
    assert_annotated_span(&spans[1]);
    assert_annotated_span(&spans[2]);
}

#[tokio::test]
#[serial]
async fn describe_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let result = (&mut conn).describe("INVALID SQL GIBBERISH").await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_error_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());

    // With annotations (error path)
    let result = conn
        .with_annotations(test_annotations())
        .describe("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);

    // With shorthand (error path)
    let result = conn
        .with_operation("SELECT", "users")
        .describe("INVALID SQL GIBBERISH")
        .await;
    assert!(result.is_err());
    let last = tel.spans().last().unwrap().clone();
    assert_annotated_span(&last);
    assert_error_span(&last);
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
}

// ===========================================================================
// QueryTextMode
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_text_mode_off_suppresses_sql() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_query_text_mode(sqlx_otel::QueryTextMode::Off)
        .build();

    let _: Option<(i32,)> = sqlx::query_as("SELECT 1")
        .fetch_optional(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].span_kind, SpanKind::Client);
    assert_eq!(
        attr(&spans[0], "db.system.name"),
        Some(opentelemetry::Value::String(SYSTEM.into()))
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
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_query_text_mode(sqlx_otel::QueryTextMode::Obfuscated)
        .build();

    sqlx::query("CREATE TABLE t (id INTEGER, name TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO t (id, name) VALUES (1, 'alice')")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(
        attr(&spans[1], "db.query.text"),
        Some(opentelemetry::Value::String(
            "INSERT INTO t (id, name) VALUES (?, ?)".into()
        ))
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

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    sqlx::query("CREATE TABLE rollback_test (id INTEGER PRIMARY KEY)")
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
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).with_database("custom_db").build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.namespace"),
        Some(opentelemetry::Value::String("custom_db".into()))
    );
}

#[tokio::test]
#[serial]
async fn builder_with_host_overrides_server_address() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).with_host("custom-host").build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "server.address"),
        Some(opentelemetry::Value::String("custom-host".into()))
    );
}

#[tokio::test]
#[serial]
async fn builder_with_port_overrides_server_port() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).with_port(9999).build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "server.port"),
        Some(opentelemetry::Value::I64(9999))
    );
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_address() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_network_peer_address("10.0.0.5")
        .build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "network.peer.address"),
        Some(opentelemetry::Value::String("10.0.0.5".into()))
    );
}

#[tokio::test]
#[serial]
async fn builder_with_network_peer_port() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).with_network_peer_port(5433).build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "network.peer.port"),
        Some(opentelemetry::Value::I64(5433))
    );
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
// Annotations
// ===========================================================================

#[tokio::test]
#[serial]
async fn annotation_all_four_fields() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    pool.with_annotations(
        QueryAnnotations::new()
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
    // Summary drives the span name (semconv level 1), distinct from "SELECT users" so
    // the assertion proves the summary path won rather than coinciding with level 2.
    assert_eq!(spans[0].name, "users by id");
    assert_eq!(
        attr(&spans[0], "db.operation.name"),
        Some(opentelemetry::Value::String("SELECT".into())),
    );
    assert_eq!(
        attr(&spans[0], "db.collection.name"),
        Some(opentelemetry::Value::String("users".into())),
    );
    assert_eq!(
        attr(&spans[0], "db.query.summary"),
        Some(opentelemetry::Value::String("users by id".into())),
    );
    assert_eq!(
        attr(&spans[0], "db.stored_procedure.name"),
        Some(opentelemetry::Value::String("sp_get_users".into())),
    );
}

#[tokio::test]
#[serial]
async fn query_summary_drives_span_name() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    pool.with_annotations(
        QueryAnnotations::new()
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
    // Summary drives the *name*, but does not suppress the other attributes.
    assert_eq!(
        attr(&spans[0], "db.query.summary"),
        Some(opentelemetry::Value::String("users by tenant".into())),
    );
    assert_eq!(
        attr(&spans[0], "db.operation.name"),
        Some(opentelemetry::Value::String("SELECT".into())),
    );
    assert_eq!(
        attr(&spans[0], "db.collection.name"),
        Some(opentelemetry::Value::String("users".into())),
    );
}

// ===========================================================================
// query-side annotations: sqlx::query(...).with_annotations(...).execute(&pool)
// ===========================================================================

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE qe_pool (id INTEGER PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert!(attr(&spans[0], "db.response.affected_rows").is_some());
}

#[tokio::test]
#[serial]
async fn query_execute_many_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    #[allow(deprecated)]
    let mut stream = sqlx::query("SELECT 1; SELECT 2")
        .with_annotations(test_annotations())
        .execute_many(&pool)
        .await;
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_fetch_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
}

#[tokio::test]
#[serial]
async fn query_fetch_many_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    #[allow(deprecated)]
    let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch_many(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let rows = sqlx::query("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .with_annotations(test_annotations())
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(3))
    );
}

#[tokio::test]
#[serial]
async fn query_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let _row = sqlx::query("SELECT 1")
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
}

#[tokio::test]
#[serial]
async fn query_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE qfo_pool (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();

    let _setup_tel = tel; // discard CREATE span
    let tel = common::TestTelemetry::install();

    let row = sqlx::query("SELECT id FROM qfo_pool WHERE id = 1")
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
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

    let row = sqlx::query("SELECT ?1 + ?2 AS sum")
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
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_annotations_first_then_bind_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let row = sqlx::query("SELECT ?1 + ?2 AS sum")
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
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_with_operation_shorthand_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE qop_pool (id INTEGER PRIMARY KEY)")
        .with_operation("SELECT", "users")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("CREATE TABLE qe_conn (id INTEGER PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&mut conn)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    sqlx::query("CREATE TABLE qe_tx (id INTEGER PRIMARY KEY)")
        .with_annotations(test_annotations())
        .execute(&mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_execute_with_annotations_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result = sqlx::query("INVALID SQL GIBBERISH")
        .with_annotations(test_annotations())
        .execute(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_error_span(&spans[0]);
}

// --- query_as side ---------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_as_fetch_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = sqlx::query_as::<_, (i32,)>("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_many_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    #[allow(deprecated)]
    let mut stream = sqlx::query_as::<_, (i32,)>("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch_many(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let rows: Vec<(i32,)> = sqlx::query_as("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let row: (i32,) = sqlx::query_as("SELECT 7")
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, 7);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let row: Option<(i32,)> = sqlx::query_as("SELECT 1 WHERE 1 = 0")
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_fetch_one_with_annotations_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result: Result<(i32,), _> = sqlx::query_as("INVALID SQL")
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_error_span(&spans[0]);
}

// --- query_scalar side -----------------------------------------------------

#[tokio::test]
#[serial]
async fn query_scalar_fetch_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = sqlx::query_scalar::<_, i32>("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_many_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    #[allow(deprecated)]
    let mut stream = sqlx::query_scalar::<_, i32>("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch_many(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let rows: Vec<i32> = sqlx::query_scalar("SELECT 1 UNION ALL SELECT 2")
        .with_annotations(test_annotations())
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![1, 2]);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i32 = sqlx::query_scalar("SELECT 42")
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 42);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: Option<i32> = sqlx::query_scalar("SELECT 1 WHERE 1 = 0")
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(value.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

// ===========================================================================
// query-side annotations: Map (Query::map / Query::try_map)
// ===========================================================================

// --- Per-position end-to-end ----------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_position_1_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT ?1")
        .with_annotations(test_annotations())
        .bind(7_i64)
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 7);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_map_position_2_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT ?1")
        .bind(11_i64)
        .with_annotations(test_annotations())
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 11);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_map_position_3_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT ?1")
        .bind(13_i64)
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 13);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_try_map_position_3_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT ?1")
        .bind(17_i64)
        .try_map(|row: sqlx::sqlite::SqliteRow| Ok(row.get::<i64, _>(0)))
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 17);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

// --- Per-method on Map (so each forwarder body is hit) --------------------

#[tokio::test]
#[serial]
async fn map_fetch_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
}

#[tokio::test]
#[serial]
async fn map_fetch_many_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    #[allow(deprecated)]
    let mut stream = sqlx::query("SELECT 1 UNION ALL SELECT 2")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_many(&pool);
    while stream.next().await.is_some() {}
    drop(stream);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn map_fetch_all_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let rows: Vec<i64> = sqlx::query("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec![1, 2, 3]);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn map_fetch_one_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT 19")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 19);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn map_fetch_optional_with_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: Option<i64> = sqlx::query("SELECT 1 WHERE 1 = 0")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(value.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

// --- Composition (multi-map; both branches of step 4) --------------------

#[tokio::test]
#[serial]
async fn map_compose_after_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT 5")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .map(|n| n * 2)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 10);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn map_try_map_compose_after_annotations_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let value: i64 = sqlx::query("SELECT 6")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .try_map(|n: i64| Ok::<_, sqlx::Error>(n + 100))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, 106);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

// --- Other executor receivers (smoke) -------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_connection() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut conn = pool.acquire().await.unwrap();
    let value: i64 = sqlx::query("SELECT 23")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(value, 23);

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_map_with_annotations_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let value: i64 = sqlx::query("SELECT 29")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_one(&mut tx)
        .await
        .unwrap();
    assert_eq!(value, 29);
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

// --- Error paths ----------------------------------------------------------

#[tokio::test]
#[serial]
async fn query_map_with_annotations_records_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let result: Result<i64, _> = sqlx::query("INVALID SQL")
        .map(|row: sqlx::sqlite::SqliteRow| row.get::<i64, _>(0))
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
    assert_error_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_try_map_with_annotations_propagates_mapper_error() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    // A `try_map` closure that fails on an otherwise-valid row produces a user-visible
    // error, but it happens *after* the database round-trip has already succeeded – the
    // executor sees the row arrive, completes the fetch, and only then does the mapper
    // surface the error. The span therefore reports success at the database layer; the
    // important contract here is that the user-visible Err carries through and that the
    // annotations were attached to the (successful) span.
    let result: Result<i64, _> = sqlx::query("SELECT 1")
        .try_map(|_row: sqlx::sqlite::SqliteRow| {
            Err::<i64, _>(sqlx::Error::Decode(
                "intentional decode failure".to_string().into(),
            ))
        })
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await;
    assert!(result.is_err());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
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
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_one_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (2, 'bob')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?1", 2_i64)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.id, 2);
    assert_eq!(row.name, "bob");

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_all_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (3, 'carol'), (4, 'dave'), (5, 'eve')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_macro_fetch_optional_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
    let row = sqlx::query!("SELECT id, name FROM macro_users WHERE id = ?1", 9999_i64)
        .with_annotations(test_annotations())
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none());

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

type MacroUser = common::MacroUser<i64>;

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_one_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (6, 'frank')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_all_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (7, 'grace'), (8, 'henry')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_as_macro_fetch_optional_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_one_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (9, 'irene')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
    let name: String = sqlx::query_scalar!("SELECT name FROM macro_users WHERE id = ?1", 9_i64)
        .with_annotations(test_annotations())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "irene");

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}

#[tokio::test]
#[serial]
async fn query_scalar_macro_fetch_all_with_annotations_via_pool() {
    let _setup_tel = common::TestTelemetry::install();
    let pool = test_pool().await;
    sqlx::query(SQLITE_MACRO_SCHEMA)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO macro_users (id, name) VALUES (10, 'jack'), (11, 'kate')")
        .execute(&pool)
        .await
        .unwrap();

    let tel = common::TestTelemetry::install();
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

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_annotated_span(&spans[0]);
}
