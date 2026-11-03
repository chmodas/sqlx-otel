#![cfg(feature = "sqlite")]

//! Connection-lifecycle contracts on the real wrappers: abandoned waits, workload attribution and
//! transaction outcomes. Cancellation of a terminal call at a known protocol point needs a real
//! server, so that half lives in `tests/postgres.rs`; the classification itself is covered
//! deterministically by the unit tests in `src/transaction_metrics.rs`.

mod common;

use std::collections::BTreeMap;
use std::task::Poll;
use std::time::Duration;

use common::TestTelemetry;
use futures::TryStreamExt;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Metric, MetricData, ResourceMetrics};
use serial_test::serial;
use sqlx::Executor as _;
use sqlx_otel::{Pool, PoolAttribution, PoolBuilder, QueryAnnotateExt};

const KEY: &str = "test.workload.kind";
const A: PoolAttribution = PoolAttribution::new(KEY, "a");
const B: PoolAttribution = PoolAttribution::new(KEY, "b");
const C: PoolAttribution = PoolAttribution::new(KEY, "c");
/// Series with no emitted attribution: outside a scope or using a reserved key.
const UNSCOPED: &str = "<unscoped>";

const WAIT: &str = "db.client.connection.wait_time";
const USE: &str = "db.client.connection.use_time";
const TIMEOUTS: &str = "db.client.connection.timeouts";
const PENDING: &str = "db.client.connection.pending_requests";
const ABANDONED: &str = "sqlx_otel.connection.abandoned_wait.duration";
const BEGIN: &str = "sqlx_otel.transaction.begin.duration";
const BEGIN_OUTCOME: &str = "sqlx_otel.transaction.begin.outcome";
const TRANSACTION: &str = "sqlx_otel.transaction.duration";
const OUTCOME: &str = "sqlx_otel.transaction.outcome";

fn snapshot(tel: &TestTelemetry) -> Vec<ResourceMetrics> {
    tel.reset();
    tel.metrics()
}

fn find<'a>(metrics: &'a [ResourceMetrics], name: &str) -> Option<&'a Metric> {
    metrics
        .iter()
        .flat_map(ResourceMetrics::scope_metrics)
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .find(|metric| metric.name() == name)
}

fn label<'a>(
    mut attributes: impl Iterator<Item = &'a opentelemetry::KeyValue>,
    key: &str,
) -> String {
    attributes
        .find(|kv| kv.key.as_str() == key)
        .map_or_else(|| UNSCOPED.to_owned(), |kv| kv.value.to_string())
}

/// Sample count of a histogram per value of `key`. Empty when no sample was ever recorded.
fn histogram_by(tel: &TestTelemetry, name: &str, key: &str) -> BTreeMap<String, u64> {
    let metrics = snapshot(tel);
    let Some(metric) = find(&metrics, name) else {
        return BTreeMap::new();
    };
    let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
        panic!("{name} must be an f64 histogram");
    };
    let mut counts = BTreeMap::new();
    for point in histogram.data_points() {
        *counts.entry(label(point.attributes(), key)).or_insert(0) += point.count();
    }
    counts
}

fn histogram_sum(tel: &TestTelemetry, name: &str) -> f64 {
    let metrics = snapshot(tel);
    let AggregatedMetrics::F64(MetricData::Histogram(histogram)) =
        find(&metrics, name).expect("histogram recorded").data()
    else {
        panic!("{name} must be an f64 histogram");
    };
    histogram
        .data_points()
        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::sum)
        .sum()
}

fn timeouts_by(tel: &TestTelemetry) -> BTreeMap<String, u64> {
    let metrics = snapshot(tel);
    let AggregatedMetrics::U64(MetricData::Sum(sum)) = find(&metrics, TIMEOUTS)
        .expect("timeouts start at zero")
        .data()
    else {
        panic!("timeouts must be a u64 counter");
    };
    sum.data_points()
        .map(|point| (label(point.attributes(), KEY), point.value()))
        .collect()
}

fn pending(tel: &TestTelemetry) -> i64 {
    let metrics = snapshot(tel);
    let AggregatedMetrics::I64(MetricData::Sum(sum)) = find(&metrics, PENDING)
        .expect("pending starts at zero")
        .data()
    else {
        panic!("pending must be an i64 sum");
    };
    assert_eq!(
        sum.data_points().count(),
        1,
        "pending stays one pool-wide series"
    );
    let point = sum.data_points().next().unwrap();
    assert_eq!(
        label(point.attributes(), KEY),
        UNSCOPED,
        "pending must never carry attribution"
    );
    point.value()
}

fn expected<const N: usize>(entries: [(&str, u64); N]) -> BTreeMap<String, u64> {
    entries
        .into_iter()
        .map(|(key, count)| (key.to_owned(), count))
        .collect()
}

async fn memory_pool() -> Pool<sqlx::Sqlite> {
    PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build()
}

/// A pool whose only connection the caller can hold, to make every later acquisition wait.
async fn single_connection_pool(acquire_timeout: Duration) -> Pool<sqlx::Sqlite> {
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(acquire_timeout)
        .connect(":memory:")
        .await
        .unwrap();
    PoolBuilder::from(raw).build()
}

const LONG: Duration = Duration::from_secs(30);

// ===========================================================================
// Abandoned waits
// ===========================================================================

#[tokio::test]
#[serial]
async fn a_never_polled_acquire_records_nothing() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(LONG).await;
    let held = pool.acquire().await.unwrap();

    drop(pool.acquire());
    drop(pool.begin());
    drop(pool.fetch_optional("SELECT 1"));
    drop(pool.fetch("SELECT 1"));

    assert_eq!(histogram_by(&tel, ABANDONED, KEY), expected([]));
    assert_eq!(pending(&tel), 0);
    drop(held);
}

#[tokio::test]
#[serial]
async fn a_dropped_wait_records_its_elapsed_time_once() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(LONG).await;
    let held = pool.acquire().await.unwrap();
    let waits_before = histogram_by(&tel, WAIT, KEY);

    let mut waiting = Box::pin(pool.acquire());
    assert!(futures::poll!(&mut waiting).is_pending());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(
        histogram_by(&tel, ABANDONED, KEY),
        expected([]),
        "still waiting, whatever the number of polls"
    );
    drop(waiting);

    assert_eq!(
        histogram_by(&tel, ABANDONED, KEY),
        expected([(UNSCOPED, 1)])
    );
    let waited = histogram_sum(&tel, ABANDONED);
    assert!(waited >= 0.03, "abandoned wait must be measured: {waited}");
    assert_eq!(histogram_by(&tel, WAIT, KEY), waits_before);
    assert_eq!(timeouts_by(&tel), expected([(UNSCOPED, 0)]));
    assert_eq!(pending(&tel), 0);
    drop(held);
}

#[tokio::test]
#[serial]
async fn a_wait_that_gets_an_answer_is_not_abandoned() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(Duration::from_millis(50)).await;
    drop(pool.acquire().await.unwrap());
    let held = pool.acquire().await.unwrap();
    assert!(matches!(
        pool.acquire().await,
        Err(sqlx::Error::PoolTimedOut)
    ));
    drop(held);
    pool.close().await;
    assert!(matches!(pool.acquire().await, Err(sqlx::Error::PoolClosed)));

    assert_eq!(histogram_by(&tel, ABANDONED, KEY), expected([]));
    assert_eq!(histogram_by(&tel, WAIT, KEY), expected([(UNSCOPED, 4)]));
    assert_eq!(timeouts_by(&tel), expected([(UNSCOPED, 1)]));
}

// ===========================================================================
// Attribution
// ===========================================================================

/// Every acquisition path, inside one scope.
#[tokio::test]
#[serial]
async fn attribution_covers_every_acquisition_path_without_changing_totals() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    A.scope(async {
        drop(pool.acquire().await.unwrap());
        pool.fetch_optional("SELECT 1").await.unwrap();
        pool.fetch("SELECT 1")
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        pool.with_operation("SELECT", "test")
            .fetch_optional("SELECT 1")
            .await
            .unwrap();
        sqlx::query("SELECT 1")
            .with_operation("SELECT", "test")
            .fetch_optional(&pool)
            .await
            .unwrap();
        pool.prepare_with(sqlx::SqlStr::from_static("SELECT 1"), &[])
            .await
            .unwrap();
        pool.describe(sqlx::SqlStr::from_static("SELECT 1"))
            .await
            .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("SELECT 1")
            .execute(&mut transaction)
            .await
            .unwrap();
        sqlx::query("SELECT 2")
            .execute(&mut transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    })
    .await;
    pool.fetch_optional("SELECT 1").await.unwrap();

    // Eight acquisitions in the scope, the two transaction queries among no extra ones.
    assert_eq!(
        histogram_by(&tel, WAIT, KEY),
        expected([("a", 8), (UNSCOPED, 1)])
    );
    assert_eq!(
        histogram_by(&tel, USE, KEY),
        expected([("a", 8), (UNSCOPED, 1)])
    );
    assert_eq!(histogram_by(&tel, BEGIN, KEY), expected([("a", 1)]));
    assert_eq!(histogram_by(&tel, TRANSACTION, KEY), expected([("a", 1)]));

    // Query instruments and pool-wide state stay exactly as they were.
    let metrics = snapshot(&tel);
    for name in [
        "db.client.operation.duration",
        "db.client.connection.max",
        "db.client.connection.idle.max",
        "db.client.connection.idle.min",
        PENDING,
    ] {
        let metric = find(&metrics, name).unwrap_or_else(|| panic!("{name} not recorded"));
        let mut keys = Vec::new();
        match metric.data() {
            AggregatedMetrics::F64(MetricData::Histogram(data)) => {
                for point in data.data_points() {
                    keys.extend(point.attributes().map(|kv| kv.key.to_string()));
                }
            }
            AggregatedMetrics::I64(MetricData::Gauge(data)) => {
                for point in data.data_points() {
                    keys.extend(point.attributes().map(|kv| kv.key.to_string()));
                }
            }
            AggregatedMetrics::I64(MetricData::Sum(data)) => {
                for point in data.data_points() {
                    keys.extend(point.attributes().map(|kv| kv.key.to_string()));
                }
            }
            _ => panic!("unexpected aggregation for {name}"),
        }
        assert!(
            !keys.iter().any(|key| key == KEY),
            "{name} must not carry attribution"
        );
    }
}

/// A lease keeps what was captured at acquisition, wherever it ends.
#[tokio::test]
#[serial]
async fn the_lease_ignores_the_scope_it_ends_in() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    // Connection: acquired under A, dropped under B.
    let connection = A.scope(pool.acquire()).await.unwrap();
    B.scope(async { drop(connection) }).await;
    assert_eq!(histogram_by(&tel, USE, KEY), expected([("a", 1)]));

    // Connection: acquired under A, dropped by an unscoped task.
    let connection = A.scope(pool.acquire()).await.unwrap();
    tokio::spawn(async move { drop(connection) }).await.unwrap();
    assert_eq!(histogram_by(&tel, USE, KEY), expected([("a", 2)]));

    // Transaction: begun under A, finished under B, and dropped under B.
    let transaction = A.scope(pool.begin()).await.unwrap();
    B.scope(transaction.commit()).await.unwrap();
    let transaction = A.scope(pool.begin()).await.unwrap();
    B.scope(async { drop(transaction) }).await;
    assert_eq!(histogram_by(&tel, TRANSACTION, KEY), expected([("a", 2)]));
    assert_eq!(histogram_by(&tel, USE, KEY), expected([("a", 4)]));
    assert_eq!(histogram_by(&tel, BEGIN, KEY), expected([("a", 2)]));
    assert_eq!(histogram_by(&tel, WAIT, KEY), expected([("a", 4)]));
}

/// A stream acquires on first poll, so that scope wins for the whole lease.
#[tokio::test]
#[serial]
async fn a_stream_keeps_the_scope_of_its_first_poll() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    // Constructed under B, which does not count: nothing was acquired yet.
    let mut rows = B
        .scope(async { pool.fetch("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3") })
        .await;
    A.scope(rows.try_next()).await.unwrap().unwrap();
    B.scope(rows.try_next()).await.unwrap().unwrap();
    rows.try_next().await.unwrap().unwrap();
    assert_eq!(
        histogram_by(&tel, USE, KEY),
        expected([]),
        "lease still open"
    );
    B.scope(async { drop(rows) }).await;

    assert_eq!(histogram_by(&tel, WAIT, KEY), expected([("a", 1)]));
    assert_eq!(histogram_by(&tel, USE, KEY), expected([("a", 1)]));
}

/// One acquisition of every kind per round, yielding across each await while holding the lease.
async fn workload(pool: Pool<sqlx::Sqlite>, rounds: u64) {
    for _ in 0..rounds {
        pool.fetch_optional("SELECT 1").await.unwrap();
        tokio::task::yield_now().await;

        let connection = pool.acquire().await.unwrap();
        tokio::task::yield_now().await;
        drop(connection);

        let mut transaction = pool.begin().await.unwrap();
        tokio::task::yield_now().await;
        sqlx::query("SELECT 1")
            .execute(&mut transaction)
            .await
            .unwrap();
        tokio::task::yield_now().await;
        transaction.commit().await.unwrap();
    }
}

/// Reserved labels must not replace pool identity or transaction outcomes in exported telemetry.
#[tokio::test]
#[serial]
async fn reserved_attribution_cannot_spoof_pool_identity_or_transaction_outcomes() {
    let tel = TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).with_pool_name("canonical").build();

    for key in [
        "db.client.connection.pool.name",
        "network.transport", // Optional and absent on this pool: still reserved.
        BEGIN_OUTCOME,
        OUTCOME,
    ] {
        PoolAttribution::new(key, "spoofed")
            .scope(async { pool.begin().await.unwrap().commit().await.unwrap() })
            .await;
    }

    for instrument in [WAIT, USE, BEGIN, TRANSACTION] {
        assert_eq!(
            histogram_by(&tel, instrument, "db.client.connection.pool.name"),
            expected([("canonical", 4)])
        );
        assert_eq!(
            histogram_by(&tel, instrument, "network.transport"),
            expected([(UNSCOPED, 4)])
        );
    }
    assert_eq!(
        histogram_by(&tel, BEGIN, BEGIN_OUTCOME),
        expected([("ok", 4)])
    );
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_ok", 4)])
    );
}

/// Scopes interleaved on one task and spread over worker threads never see each other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn concurrent_scopes_never_leak_across_awaits_or_worker_threads() {
    const ROUNDS: u64 = 40;
    let tel = TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(3)
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();

    // A and an unscoped sibling interleave on one spawned task.
    let same_task = {
        let (first, second) = (pool.clone(), pool.clone());
        tokio::spawn(async move {
            futures::join!(A.scope(workload(first, ROUNDS)), workload(second, ROUNDS));
        })
    };
    // B and C run as two tasks each, so workers keep migrating between the two scopes.
    let mut tasks = vec![same_task];
    for attribution in [B, B, C, C] {
        let pool = pool.clone();
        tasks.push(tokio::spawn(attribution.scope(workload(pool, ROUNDS))));
    }
    for task in tasks {
        task.await.unwrap();
    }

    let acquisitions = expected([
        ("a", 3 * ROUNDS),
        ("b", 6 * ROUNDS),
        ("c", 6 * ROUNDS),
        (UNSCOPED, 3 * ROUNDS),
    ]);
    assert_eq!(histogram_by(&tel, WAIT, KEY), acquisitions);
    assert_eq!(histogram_by(&tel, USE, KEY), acquisitions);
    let transactions = expected([
        ("a", ROUNDS),
        ("b", 2 * ROUNDS),
        ("c", 2 * ROUNDS),
        (UNSCOPED, ROUNDS),
    ]);
    assert_eq!(histogram_by(&tel, BEGIN, KEY), transactions);
    assert_eq!(histogram_by(&tel, TRANSACTION, KEY), transactions);
    assert_eq!(pending(&tel), 0);
}

/// Cancellation and timeouts belong to the attempt, not to whoever dropped it or ran alongside it.
#[tokio::test]
#[serial]
async fn waits_are_cancelled_and_timed_out_under_their_own_scope() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(Duration::from_millis(50)).await;
    let held = C.scope(pool.acquire()).await.unwrap();
    assert_eq!(
        timeouts_by(&tel),
        expected([(UNSCOPED, 0), ("c", 0)]),
        "a workload's series exists at zero before its first timeout"
    );

    let mut cancelled = Box::pin(A.scope(pool.acquire()));
    let mut timed_out = Box::pin(B.scope(pool.acquire()));
    assert!(futures::poll!(&mut cancelled).is_pending());
    assert!(futures::poll!(&mut timed_out).is_pending());
    assert_eq!(pending(&tel), 2);

    // Dropped from inside the other attempt's scope.
    B.scope(async { drop(cancelled) }).await;
    assert_eq!(histogram_by(&tel, ABANDONED, KEY), expected([("a", 1)]));
    assert_eq!(pending(&tel), 1);

    assert!(matches!(timed_out.await, Err(sqlx::Error::PoolTimedOut)));
    assert_eq!(
        timeouts_by(&tel),
        expected([(UNSCOPED, 0), ("a", 0), ("b", 1), ("c", 0)])
    );
    assert_eq!(
        histogram_by(&tel, WAIT, KEY),
        expected([("b", 1), ("c", 1)])
    );
    assert_eq!(histogram_by(&tel, ABANDONED, KEY), expected([("a", 1)]));
    assert_eq!(pending(&tel), 0);
    drop(held);
}

/// The documented limit: a spawned task starts outside the scope until told otherwise.
#[tokio::test]
#[serial]
async fn spawned_work_is_unscoped_unless_explicitly_inherited() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    let (plain, inherited) = A
        .scope(async {
            let plain = {
                let pool = pool.clone();
                tokio::spawn(async move { drop(pool.acquire().await.unwrap()) })
            };
            let inherited = {
                let pool = pool.clone();
                tokio::spawn(PoolAttribution::inherit(async move {
                    drop(pool.acquire().await.unwrap());
                }))
            };
            (plain, inherited)
        })
        .await;
    plain.await.unwrap();
    inherited.await.unwrap();

    assert_eq!(
        histogram_by(&tel, WAIT, KEY),
        expected([("a", 1), (UNSCOPED, 1)])
    );
    assert_eq!(
        histogram_by(&tel, USE, KEY),
        expected([("a", 1), (UNSCOPED, 1)])
    );
}

// ===========================================================================
// Transaction outcomes
// ===========================================================================

#[tokio::test]
#[serial]
async fn each_way_of_ending_a_transaction_records_one_outcome_on_one_acquisition() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    let mut transaction = pool.begin().await.unwrap();
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([]),
        "still open"
    );
    for query in ["SELECT 1", "SELECT 2", "SELECT 3"] {
        sqlx::query(query).execute(&mut transaction).await.unwrap();
    }
    assert_eq!(
        histogram_by(&tel, WAIT, KEY),
        expected([(UNSCOPED, 1)]),
        "queries inside a transaction must not acquire"
    );
    transaction.commit().await.unwrap();
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_ok", 1)])
    );

    pool.begin().await.unwrap().rollback().await.unwrap();
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_ok", 1), ("rollback_ok", 1)])
    );

    drop(pool.begin().await.unwrap());
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_ok", 1), ("dropped", 1), ("rollback_ok", 1)])
    );

    // Created but never polled: nothing was sent, so these are plain drops.
    drop(pool.begin().await.unwrap().commit());
    drop(pool.begin().await.unwrap().rollback());
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_ok", 1), ("dropped", 3), ("rollback_ok", 1)])
    );

    assert_eq!(
        histogram_by(&tel, BEGIN, BEGIN_OUTCOME),
        expected([("ok", 5)])
    );
    assert_eq!(histogram_by(&tel, WAIT, KEY), expected([(UNSCOPED, 5)]));
    assert_eq!(histogram_by(&tel, USE, KEY), expected([(UNSCOPED, 5)]));
}

/// A deferred constraint fails at `COMMIT` itself, so this is a real server rejection.
#[tokio::test]
#[serial]
async fn a_rejected_commit_is_an_error_outcome() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(LONG).await;
    pool.execute(
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (
             parent_id INTEGER REFERENCES parent (id) DEFERRABLE INITIALLY DEFERRED
         );",
    )
    .await
    .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO child VALUES (1)")
        .execute(&mut transaction)
        .await
        .unwrap();
    assert!(transaction.commit().await.is_err());

    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([("commit_error", 1)])
    );
    assert_eq!(
        histogram_by(&tel, USE, KEY)[UNSCOPED],
        2,
        "setup plus this lease"
    );
}

/// `BEGIN` fails on a connection the pool believes idle but which is already in a transaction.
#[tokio::test]
#[serial]
async fn a_failed_begin_keeps_its_lease_accounting_and_starts_no_transaction() {
    let tel = TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                connection.execute("BEGIN").await?;
                Ok(())
            })
        })
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();

    assert!(pool.begin().await.is_err());

    assert_eq!(
        histogram_by(&tel, BEGIN, BEGIN_OUTCOME),
        expected([("error", 1)])
    );
    assert_eq!(histogram_by(&tel, TRANSACTION, OUTCOME), expected([]));
    assert_eq!(histogram_by(&tel, WAIT, KEY), expected([(UNSCOPED, 1)]));
    assert_eq!(histogram_by(&tel, USE, KEY), expected([(UNSCOPED, 1)]));
}

/// A wait abandoned before `BEGIN` never reached the transaction instruments.
#[tokio::test]
#[serial]
async fn a_begin_dropped_while_waiting_for_a_connection_is_only_an_abandoned_wait() {
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(LONG).await;
    let held = pool.acquire().await.unwrap();

    let mut begin = Box::pin(pool.begin());
    assert!(futures::poll!(&mut begin).is_pending());
    drop(begin);

    assert_eq!(
        histogram_by(&tel, ABANDONED, KEY),
        expected([(UNSCOPED, 1)])
    );
    assert_eq!(histogram_by(&tel, BEGIN, BEGIN_OUTCOME), expected([]));
    assert_eq!(histogram_by(&tel, TRANSACTION, OUTCOME), expected([]));
    drop(held);
}

/// Through the real wrappers. The `SQLite` worker thread could answer before the first poll
/// returns, so either result is accepted as long as it matches what the caller saw; the
/// deterministic version of this lives in the `src/transaction_metrics.rs` unit tests.
#[tokio::test]
#[serial]
async fn a_dropped_terminal_call_reports_what_the_caller_could_see_exactly_once() {
    let tel = TestTelemetry::install();
    let pool = memory_pool().await;

    let mut commit = Box::pin(pool.begin().await.unwrap().commit());
    let commit_outcome = match futures::poll!(&mut commit) {
        Poll::Pending => "commit_abandoned",
        Poll::Ready(result) => {
            result.unwrap();
            "commit_ok"
        }
    };
    drop(commit);
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([(commit_outcome, 1)])
    );

    let mut rollback = Box::pin(pool.begin().await.unwrap().rollback());
    let rollback_outcome = match futures::poll!(&mut rollback) {
        Poll::Pending => "rollback_abandoned",
        Poll::Ready(result) => {
            result.unwrap();
            "rollback_ok"
        }
    };
    drop(rollback);
    assert_eq!(
        histogram_by(&tel, TRANSACTION, OUTCOME),
        expected([(commit_outcome, 1), (rollback_outcome, 1)])
    );
    assert_eq!(histogram_by(&tel, USE, KEY), expected([(UNSCOPED, 2)]));
}

// ===========================================================================
// Shape
// ===========================================================================

/// Hardcoded on purpose: a shared constant would make the assertion tautological.
#[tokio::test]
#[serial]
async fn new_instruments_export_their_unit_and_boundaries() {
    const LATENCY: &[f64] = &[
        0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        10.0, 30.0, 60.0,
    ];
    const LONG_RUNNING: &[f64] = &[
        0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
    ];
    let tel = TestTelemetry::install();
    let pool = single_connection_pool(LONG).await;
    pool.begin().await.unwrap().commit().await.unwrap();
    let held = pool.acquire().await.unwrap();
    let mut waiting = Box::pin(pool.acquire());
    assert!(futures::poll!(&mut waiting).is_pending());
    drop(waiting);
    drop(held);

    let metrics = snapshot(&tel);
    for (name, bounds) in [
        (ABANDONED, LATENCY),
        (BEGIN, LATENCY),
        (TRANSACTION, LONG_RUNNING),
    ] {
        let metric = find(&metrics, name).expect("instrument recorded");
        assert_eq!(metric.unit(), "s", "{name}");
        let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
            panic!("{name} must be an f64 histogram");
        };
        let point = histogram.data_points().next().expect("one data point");
        assert_eq!(point.bounds().collect::<Vec<_>>(), bounds, "{name}");
    }
}
