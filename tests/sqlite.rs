#![cfg(feature = "sqlite")]

mod common;

use common::{assert_common_span_attributes, assert_error_span, attr};
use futures::StreamExt;
use opentelemetry::trace::SpanKind;
use serial_test::serial;
use sqlx::Executor as _;
use sqlx::Sqlite;
use sqlx_otel::{Pool, PoolBuilder, Transaction};

const SYSTEM: &str = "sqlite";

/// Helper to create an in-memory Sqlite pool wrapped in our instrumented Pool.
async fn test_pool() -> Pool<Sqlite> {
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    PoolBuilder::from(raw).build()
}

// ===========================================================================
// execute
// ===========================================================================

#[tokio::test]
#[serial]
async fn execute_creates_span_via_pool() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    sqlx::query("CREATE TABLE exec_pool (id INTEGER PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
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
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
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
    tx.commit().await.unwrap();

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
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
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
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
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
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(2))
    );
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
}

#[tokio::test]
#[serial]
async fn fetch_one_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _row = (&mut tx).fetch_one("SELECT 1").await.unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
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
}

#[tokio::test]
#[serial]
async fn fetch_optional_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let result = (&mut tx).fetch_optional("SELECT 99").await.unwrap();
    assert!(result.is_some());
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert_eq!(
        attr(&spans[0], "db.response.returned_rows"),
        Some(opentelemetry::Value::I64(1))
    );
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
}

#[tokio::test]
#[serial]
async fn prepare_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _stmt = (&mut tx).prepare("SELECT 1").await.unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
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
}

#[tokio::test]
#[serial]
async fn prepare_with_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _stmt = (&mut tx).prepare_with("SELECT ?", &[]).await.unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
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
}

#[tokio::test]
#[serial]
async fn describe_via_transaction() {
    let tel = common::TestTelemetry::install();
    let pool = test_pool().await;

    let mut tx: Transaction<'_, Sqlite> = pool.begin().await.unwrap();
    let _desc = (&mut tx).describe("SELECT 1").await.unwrap();
    tx.commit().await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_common_span_attributes(&spans[0], SYSTEM);
    assert!(attr(&spans[0], "db.response.returned_rows").is_none());
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
