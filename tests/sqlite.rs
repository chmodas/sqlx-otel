#![cfg(feature = "sqlite")]

mod common;

use common::{assert_common_span_attributes, assert_error_span, attr};
use futures::StreamExt;
use opentelemetry::trace::SpanKind;
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::Sqlite;
use sqlx_otel::{Pool, PoolBuilder, QueryAnnotations, Transaction};

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
