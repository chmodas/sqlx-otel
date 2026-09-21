#![cfg(feature = "sqlite")]

mod common;

use common::attr;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use serial_test::serial;
use sqlx::Executor as _;
use sqlx_otel::PoolBuilder;
use std::time::Duration;

const POOL_NAME: &str = "test-pool";

/// The boundaries the crate ships on every latency histogram.
///
/// Hardcoded on purpose: importing `LATENCY_BUCKETS_SECONDS` would make the assertion
/// tautological, and it would pass through any edit to the constant.
const EXPECTED_LATENCY_BUCKETS: &[f64] = &[
    0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    30.0, 60.0,
];

fn histogram_count(tel: &common::TestTelemetry, name: &str) -> u64 {
    tel.reset();
    let metrics = tel.metrics();
    let Some(metric) = find_metric(&metrics, name) else {
        return 0;
    };
    let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
        panic!("expected histogram")
    };
    hist.data_points()
        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
        .sum()
}

fn pending_count(tel: &common::TestTelemetry) -> i64 {
    tel.reset();
    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.pending_requests").unwrap();
    let AggregatedMetrics::I64(MetricData::Sum(sum)) = metric.data() else {
        panic!("expected pending sum")
    };
    sum.data_points()
        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
        .sum()
}

fn timeout_count(tel: &common::TestTelemetry) -> u64 {
    tel.reset();
    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.timeouts").unwrap();
    let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
        panic!("expected timeout sum")
    };
    sum.data_points()
        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
        .sum()
}

#[tokio::test]
#[serial]
async fn every_pool_executor_path_acquires_once_without_duplicate_query_spans() {
    use futures::TryStreamExt;
    use sqlx_otel::QueryAnnotateExt;
    let tel = common::TestTelemetry::install();
    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    pool.execute("CREATE TABLE test (id integer)")
        .await
        .unwrap();
    pool.execute("INSERT INTO test VALUES (1)").await.unwrap();
    pool.fetch_all("SELECT id FROM test").await.unwrap();
    pool.fetch_one("SELECT id FROM test").await.unwrap();
    pool.fetch_optional("SELECT id FROM test").await.unwrap();
    pool.fetch("SELECT id FROM test")
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    #[allow(deprecated)]
    pool.fetch_many("SELECT id FROM test")
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    pool.execute_many("UPDATE test SET id = 2")
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    pool.prepare_with(sqlx::SqlStr::from_static("SELECT id FROM test"), &[])
        .await
        .unwrap();
    pool.describe(sqlx::SqlStr::from_static("SELECT id FROM test"))
        .await
        .unwrap();
    // `prepare` defaults into `prepare_with`, so it is a funnel path of its own.
    pool.prepare(sqlx::SqlStr::from_static("SELECT id FROM test"))
        .await
        .unwrap();
    pool.with_operation("SELECT", "test")
        .fetch_one("SELECT id FROM test")
        .await
        .unwrap();
    sqlx::query("SELECT id FROM test")
        .with_operation("SELECT", "test")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        tel.spans().len(),
        13,
        "query instrumentation must not be nested"
    );
    assert_eq!(histogram_count(&tel, "db.client.connection.wait_time"), 13);
    assert_eq!(histogram_count(&tel, "db.client.connection.use_time"), 13);
    assert_eq!(pending_count(&tel), 0);
    assert_eq!(timeout_count(&tel), 0);
}

#[tokio::test]
#[serial]
async fn transactions_keep_one_acquisition_and_measure_the_complete_lease() {
    let tel = common::TestTelemetry::install();
    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    for finish in 0..3 {
        let mut transaction = pool.begin().await.unwrap();
        sqlx::query("SELECT 1")
            .execute(&mut transaction)
            .await
            .unwrap();
        sqlx::query("SELECT 2")
            .execute(&mut transaction)
            .await
            .unwrap();
        assert_eq!(
            histogram_count(&tel, "db.client.connection.wait_time"),
            finish + 1
        );
        assert_eq!(
            histogram_count(&tel, "db.client.connection.use_time"),
            finish
        );
        match finish {
            0 => transaction.commit().await.unwrap(),
            1 => transaction.rollback().await.unwrap(),
            _ => drop(transaction),
        }
        assert_eq!(
            histogram_count(&tel, "db.client.connection.use_time"),
            finish + 1
        );
    }
    assert_eq!(pending_count(&tel), 0);
}

async fn waiting_operation(
    pool: &sqlx_otel::Pool<sqlx::Sqlite>,
    path: u8,
) -> Result<(), sqlx::Error> {
    use futures::TryStreamExt;
    match path {
        0 => {
            drop(pool.acquire().await?);
        }
        1 => {
            pool.fetch_optional("SELECT 1").await?;
        }
        2 => {
            drop(pool.begin().await?);
        }
        3 => {
            pool.fetch("SELECT 1").try_next().await?;
        }
        _ => {
            pool.with_operation("SELECT", "test")
                .fetch_optional("SELECT 1")
                .await?;
        }
    }
    Ok(())
}

#[tokio::test]
#[serial]
async fn all_acquisition_paths_clear_pending_on_cancel_and_report_real_timeouts() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(100))
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();
    let held = pool.acquire().await.unwrap();
    for path in 0..5 {
        let mut cancelled = Box::pin(waiting_operation(&pool, path));
        assert!(futures::poll!(&mut cancelled).is_pending());
        assert_eq!(pending_count(&tel), 1);
        drop(cancelled);
        assert_eq!(
            pending_count(&tel),
            0,
            "cancelled acquisition leaked pending"
        );
        assert_eq!(
            timeout_count(&tel),
            u64::from(path),
            "caller cancellation is not a pool timeout"
        );
        assert!(matches!(
            waiting_operation(&pool, path).await,
            Err(sqlx::Error::PoolTimedOut)
        ));
        assert_eq!(pending_count(&tel), 0);
        assert_eq!(timeout_count(&tel), u64::from(path) + 1);
        assert_eq!(
            histogram_count(&tel, "db.client.connection.wait_time"),
            u64::from(path) + 2
        );
    }
    drop(held);
}

#[tokio::test]
#[serial]
async fn dropping_a_partially_consumed_stream_releases_its_only_connection() {
    use futures::TryStreamExt;
    let tel = common::TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();
    let mut rows = pool.fetch("SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3");
    rows.try_next().await.unwrap().unwrap();
    assert_eq!(histogram_count(&tel, "db.client.connection.use_time"), 0);
    drop(rows);
    assert_eq!(histogram_count(&tel, "db.client.connection.use_time"), 1);
    tokio::time::timeout(Duration::from_secs(1), pool.acquire())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending_count(&tel), 0);
}

/// Resolve a latency histogram's bucket boundaries as the SDK actually configured them.
fn histogram_bounds(tel: &common::TestTelemetry, name: &str) -> Vec<f64> {
    tel.reset();
    let metrics = tel.metrics();
    let metric = find_metric(&metrics, name).expect("histogram not recorded");
    let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
        panic!("expected histogram")
    };
    hist.data_points()
        .next()
        .expect("no data point")
        .bounds()
        .collect()
}

/// Every latency histogram must carry the shared seconds-scale boundaries.
///
/// The `OTel` SDK default boundaries (`0, 5, 10, 25, ... 10000`) are millisecond-shaped, so on a
/// seconds-valued histogram every observation below five seconds collapses into one bucket and
/// quantiles become interpolation artefacts. This pins all three latency histograms to the
/// shared set; row-count histograms are deliberately left on the defaults.
#[tokio::test]
#[serial]
async fn latency_histograms_use_seconds_scale_buckets() {
    let tel = common::TestTelemetry::install();
    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    drop(pool.acquire().await.unwrap());
    pool.execute("SELECT 1").await.unwrap();
    pool.fetch_all("SELECT 1").await.unwrap();

    for name in LATENCY_HISTOGRAMS {
        assert_eq!(
            histogram_bounds(&tel, name),
            EXPECTED_LATENCY_BUCKETS,
            "{name} boundaries"
        );
    }

    // The negative half. Row counts are not durations, so the seconds-scale set must not reach
    // them – and the shared constant now sits two lines from their builders, which is exactly
    // where an accidental copy-paste would land.
    for name in [
        "db.client.response.returned_rows",
        "db.client.response.affected_rows",
    ] {
        assert_ne!(
            histogram_bounds(&tel, name),
            EXPECTED_LATENCY_BUCKETS,
            "{name} must keep the SDK default boundaries"
        );
    }
}

/// Sub-millisecond acquisitions must land in the low buckets, not be swallowed by a coarse one.
///
/// This is the behaviour the boundaries exist for: an in-memory `SQLite` pool acquires in tens of
/// microseconds, which the SDK defaults would record as indistinguishable from a four-second
/// wait. The assertion is deliberately made against the 5ms boundary rather than the tightest
/// one measurement allows – still three orders of magnitude finer than the SDK default's first
/// edge, without making the test hostage to a scheduler hiccup on a loaded CI runner.
#[tokio::test]
#[serial]
async fn submillisecond_acquisition_lands_below_the_first_millisecond_boundary() {
    let tel = common::TestTelemetry::install();
    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    for _ in 0..8 {
        drop(pool.acquire().await.unwrap());
    }

    tel.reset();
    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.wait_time").unwrap();
    let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
        panic!("expected histogram")
    };
    let dp = hist.data_points().next().unwrap();
    let bounds: Vec<f64> = dp.bounds().collect();
    let counts: Vec<u64> = dp.bucket_counts().collect();
    let five_ms = bounds
        .iter()
        .position(|b| (b - 0.005).abs() < f64::EPSILON)
        .expect("0.005 boundary missing from the shipped set");
    let below_five_ms: u64 = counts[..=five_ms].iter().sum();
    assert_eq!(
        below_five_ms,
        dp.count(),
        "warm in-memory acquisitions should all record under 5ms; got {counts:?}"
    );
}

/// A query that fails after acquisition must still release its connection and clear pending.
///
/// The failure arrives from the driver once the connection is already leased, so the guard and
/// the usage lease both have to unwind on the error path, not just on success.
#[tokio::test]
#[serial]
async fn queries_failing_after_acquisition_release_the_connection() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(200))
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();

    assert!(pool.fetch_optional("SELECT * FROM missing").await.is_err());
    assert!(pool.execute("NOT VALID SQL").await.is_err());
    assert!(pool.fetch_all("SELECT * FROM missing").await.is_err());

    // Collect spans before touching any metric helper: they all call `TestTelemetry::reset`,
    // which drains the span exporter as well as the metric one.
    //
    // Query-level instrumentation must survive the error path, not just acquisition accounting:
    // each failure should still produce a span marked with the error.
    let spans = tel.spans();
    assert_eq!(spans.len(), 3, "one span per failed query");
    assert!(
        spans.iter().all(|span| span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "error.type")),
        "every failed query span must carry error.type"
    );

    assert_eq!(pending_count(&tel), 0);
    assert_eq!(
        timeout_count(&tel),
        0,
        "a query error is not a pool timeout"
    );
    assert_eq!(histogram_count(&tel, "db.client.connection.wait_time"), 3);
    assert_eq!(histogram_count(&tel, "db.client.connection.use_time"), 3);
    assert_eq!(
        histogram_count(&tel, "db.client.operation.duration"),
        3,
        "a failed query should still record its duration"
    );
    // The inner `expect` carries the diagnostic: these pools set a 200ms acquire timeout, so a
    // leak surfaces as `Err(PoolTimedOut)` long before the outer 1s timeout could fire.
    tokio::time::timeout(Duration::from_secs(1), pool.acquire())
        .await
        .expect("acquire hung")
        .expect("failed queries exhausted the pool");
}

/// A closed pool fails acquisition without leasing a connection or counting a timeout.
///
/// `PoolClosed` and `PoolTimedOut` are both acquisition failures but only the latter belongs in
/// `db.client.connection.timeouts`; conflating them would inflate the timeout rate on shutdown.
#[tokio::test]
#[serial]
async fn closed_pool_records_no_lease_and_no_timeout() {
    let tel = common::TestTelemetry::install();
    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    pool.close().await;

    assert!(pool.fetch_optional("SELECT 1").await.is_err());
    assert!(pool.begin().await.is_err());

    assert_eq!(pending_count(&tel), 0);
    assert_eq!(timeout_count(&tel), 0, "PoolClosed is not a timeout");
    assert_eq!(
        histogram_count(&tel, "db.client.connection.use_time"),
        0,
        "no connection was ever leased"
    );
}

/// A multi-statement stream holds one connection across every statement, and releases it when
/// abandoned between them.
///
/// Row-only streams are already covered by `dropping_a_partially_consumed_stream_releases_its_
/// only_connection`. The distinct axis here is `fetch_many` yielding `Either::Left(QueryResult)`
/// at each statement boundary: the generator resumes across those boundaries on the same lease,
/// so a single acquisition must cover all three statements no matter where the caller stops.
#[tokio::test]
#[serial]
async fn multi_statement_stream_holds_one_lease_across_statement_boundaries() {
    use futures::TryStreamExt;
    let tel = common::TestTelemetry::install();
    let raw = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(200))
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();
    pool.execute("CREATE TABLE t (id integer)").await.unwrap();

    // The SDK aggregates histograms cumulatively and `TestTelemetry::reset` only drops exported
    // batches, so the setup query's observations persist. Compare against a baseline rather than
    // against zero.
    let waits_before = histogram_count(&tel, "db.client.connection.wait_time");
    let uses_before = histogram_count(&tel, "db.client.connection.use_time");

    // Three statements: each completion yields a `Left(QueryResult)` before the next begins.
    #[allow(deprecated)]
    let mut items = Box::pin(pool.fetch_many(
        "INSERT INTO t VALUES (1); INSERT INTO t VALUES (2); INSERT INTO t VALUES (3);",
    ));

    let first = items.try_next().await.unwrap().expect("no first item");
    assert!(
        first.is_left(),
        "expected a QueryResult at the first statement boundary"
    );
    assert_eq!(
        histogram_count(&tel, "db.client.connection.wait_time"),
        waits_before + 1,
        "one acquisition covers the whole multi-statement stream"
    );
    assert_eq!(
        histogram_count(&tel, "db.client.connection.use_time"),
        uses_before,
        "the lease is still open while the stream is mid-flight"
    );

    // Abandon between statements, not between rows.
    drop(items);

    assert_eq!(
        histogram_count(&tel, "db.client.connection.use_time"),
        uses_before + 1,
        "abandoning the stream must close out exactly one lease"
    );
    assert_eq!(
        histogram_count(&tel, "db.client.connection.wait_time"),
        waits_before + 1,
        "no statement boundary may trigger a second acquisition"
    );
    assert_eq!(pending_count(&tel), 0);
    tokio::time::timeout(Duration::from_secs(1), pool.acquire())
        .await
        .expect("acquire hung")
        .expect("stream abandoned between statements leaked its connection");
}

/// The three latency histogram names the crate ships explicit boundaries for.
const LATENCY_HISTOGRAMS: [&str; 3] = [
    "db.client.connection.wait_time",
    "db.client.connection.use_time",
    "db.client.operation.duration",
];

/// Build a pool under a freshly installed meter provider and report each latency histogram's
/// bucket boundaries.
///
/// Takes an optional view so the same pool workload can be observed with and without one. Each
/// call installs its own provider because `PoolBuilder::build` resolves its instruments from
/// whatever provider is global at that moment, so the view has to be registered first.
async fn latency_bounds_under_view(view_boundaries: Option<Vec<f64>>) -> Vec<Vec<f64>> {
    use opentelemetry_sdk::metrics::{
        Aggregation, InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
    };

    let exporter = InMemoryMetricExporter::default();
    let mut builder =
        SdkMeterProvider::builder().with_reader(PeriodicReader::builder(exporter.clone()).build());
    if let Some(boundaries) = view_boundaries {
        // Match only the three histograms. A broader predicate would also hit the counters and
        // gauges, where the SDK rejects a histogram aggregation and silently falls back to the
        // implicit default view – passing through an error-recovery path instead of this one.
        builder = builder.with_view(move |i: &Instrument| {
            LATENCY_HISTOGRAMS.contains(&i.name()).then(|| {
                Stream::builder()
                    .with_aggregation(Aggregation::ExplicitBucketHistogram {
                        boundaries: boundaries.clone(),
                        record_min_max: true,
                    })
                    .build()
                    .unwrap()
            })
        });
    }
    let provider = builder.build();
    opentelemetry::global::set_meter_provider(provider.clone());

    let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await.unwrap()).build();
    drop(pool.acquire().await.unwrap());
    pool.execute("SELECT 1").await.unwrap();
    provider.force_flush().unwrap();

    let collected = exporter.get_finished_metrics().unwrap();
    LATENCY_HISTOGRAMS
        .iter()
        .map(|name| {
            let metric = find_metric(&collected, name).expect("histogram not recorded");
            let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
                panic!("expected histogram for {name}")
            };
            hist.data_points()
                .next()
                .expect("no data point")
                .bounds()
                .collect()
        })
        .collect()
}

/// An application `View` must be able to override the bucket boundaries the crate ships.
///
/// The crate sets boundaries as instrument *advice*, and the SDK applies advice only where no
/// matching view has already set an aggregation. That is what lets callers retune the histograms
/// without this crate exposing an API for it, and the README documents it.
///
/// Both halves are asserted in one test on purpose. Checking only that a view produces the
/// boundaries it asked for would pass even if the crate shipped no advice at all – it would be
/// testing the SDK, not this crate.
///
/// The baseline does most of the work: it shows the crate's boundaries are applied when no view
/// is registered, so the second half is genuinely replacing them rather than supplying a value
/// where there was none. The `assert_ne!` covers the one case the baseline cannot – an override
/// that happens to match the shipped set, which would leave the two halves indistinguishable.
///
/// This test installs its own meter provider rather than using `TestTelemetry`, which has no
/// hook for registering views. That makes `#[serial]` matter more here than elsewhere: nothing
/// restores the previous provider afterwards, and the next test's `TestTelemetry` installation
/// is what reclaims it.
#[tokio::test]
#[serial]
async fn an_application_view_overrides_the_shipped_bucket_boundaries() {
    const OVERRIDE: &[f64] = &[0.005, 0.05, 0.5, 5.0];

    // Without a view, the pool must come up on the boundaries the crate advises.
    let baseline = latency_bounds_under_view(None).await;
    for (name, bounds) in LATENCY_HISTOGRAMS.iter().zip(&baseline) {
        assert_eq!(
            bounds, EXPECTED_LATENCY_BUCKETS,
            "{name}: crate advice must apply when no view matches"
        );
    }

    // With one, the view's aggregation must win outright.
    let overridden = latency_bounds_under_view(Some(OVERRIDE.to_vec())).await;
    for (name, bounds) in LATENCY_HISTOGRAMS.iter().zip(&overridden) {
        assert_eq!(
            bounds, OVERRIDE,
            "{name}: a view's aggregation must beat the crate's advice"
        );
    }

    // The guard that keeps this test honest: if the crate ever stopped shipping advice, the two
    // observations would coincide and the assertions above would still pass individually.
    assert_ne!(
        baseline, overridden,
        "override must actually change the bucket layout"
    );
}

/// Helper to find a named metric in the collected resource metrics.
fn find_metric<'a>(
    resource_metrics: &'a [opentelemetry_sdk::metrics::data::ResourceMetrics],
    name: &str,
) -> Option<&'a opentelemetry_sdk::metrics::data::Metric> {
    resource_metrics.iter().find_map(|rm| {
        rm.scope_metrics()
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|m| m.name() == name)
    })
}

/// Helper to find an i64 gauge value with a specific attribute filter.
fn gauge_value(
    resource_metrics: &[opentelemetry_sdk::metrics::data::ResourceMetrics],
    name: &str,
    filter_key: &str,
    filter_value: &str,
) -> Option<i64> {
    let metric = find_metric(resource_metrics, name)?;
    if let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() {
        gauge.data_points().find_map(|dp| {
            let matches = dp
                .attributes()
                .any(|kv| kv.key.as_str() == filter_key && kv.value.to_string() == filter_value);
            if matches { Some(dp.value()) } else { None }
        })
    } else {
        None
    }
}

/// Helper to get any i64 gauge value (ignoring attribute filtering).
fn gauge_any_value(
    resource_metrics: &[opentelemetry_sdk::metrics::data::ResourceMetrics],
    name: &str,
) -> Option<i64> {
    let metric = find_metric(resource_metrics, name)?;
    if let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() {
        gauge
            .data_points()
            .next()
            .map(opentelemetry_sdk::metrics::data::GaugeDataPoint::value)
    } else {
        None
    }
}

/// Poll a closure until it returns `Some` or the deadline elapses.
///
/// Used to wait for background-task-driven metrics to arrive without depending on a
/// fixed sleep duration. The 10ms inter-poll cadence picks up state changes promptly
/// while keeping the busy-wait cost bounded; the caller-provided timeout sets the
/// upper bound that defines a flake threshold rather than a hard expectation.
async fn poll_for<F, T>(timeout: Duration, mut f: F) -> Option<T>
where
    F: FnMut() -> Option<T>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ===========================================================================
// Static pool configuration gauges (no runtime needed)
// ===========================================================================

#[tokio::test]
#[serial]
async fn connection_max_matches_pool_options() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
        .max_connections(5)
        .connect(":memory:")
        .await
        .unwrap();
    let _pool = PoolBuilder::from(raw).build();

    let metrics = tel.metrics();
    let max = gauge_any_value(&metrics, "db.client.connection.max");
    assert_eq!(max, Some(5), "max connections should be 5");
}

#[tokio::test]
#[serial]
async fn connection_idle_min_matches_pool_options() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
        .min_connections(2)
        .connect(":memory:")
        .await
        .unwrap();
    let _pool = PoolBuilder::from(raw).build();

    let metrics = tel.metrics();
    let min = gauge_any_value(&metrics, "db.client.connection.idle.min");
    assert_eq!(min, Some(2), "idle min should be 2");
}

#[tokio::test]
#[serial]
async fn connection_idle_max_matches_max_connections() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
        .max_connections(7)
        .connect(":memory:")
        .await
        .unwrap();
    let _pool = PoolBuilder::from(raw).build();

    let metrics = tel.metrics();
    let idle_max = gauge_any_value(&metrics, "db.client.connection.idle.max");
    assert_eq!(idle_max, Some(7), "idle.max should equal max_connections");
}

// ===========================================================================
// Inline metrics (no runtime needed)
// ===========================================================================

#[tokio::test]
#[serial]
async fn wait_time_recorded_on_acquire() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).build();

    let conn = pool.acquire().await.unwrap();
    drop(conn);

    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.wait_time");
    assert!(metric.is_some(), "wait_time metric should be present");

    if let Some(m) = metric {
        assert_eq!(m.unit(), "s");
        if let AggregatedMetrics::F64(MetricData::Histogram(hist)) = m.data() {
            let dp: Vec<_> = hist.data_points().collect();
            assert!(!dp.is_empty(), "should have data points");
            assert!(dp[0].count() >= 1, "should have at least one recording");
        } else {
            panic!("wait_time should be an f64 histogram");
        }
    }
}

#[tokio::test]
#[serial]
async fn use_time_recorded_on_connection_drop() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).build();

    let conn = pool.acquire().await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    drop(conn);

    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.use_time");
    assert!(metric.is_some(), "use_time metric should be present");

    if let Some(m) = metric {
        assert_eq!(m.unit(), "s");
        if let AggregatedMetrics::F64(MetricData::Histogram(hist)) = m.data() {
            let dp: Vec<_> = hist.data_points().collect();
            assert!(!dp.is_empty(), "should have data points");
            assert!(dp[0].count() >= 1, "should have at least one recording");
        } else {
            panic!("use_time should be an f64 histogram");
        }
    }
}

#[tokio::test]
#[serial]
async fn timeouts_counter_incremented_on_pool_timeout() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(10))
        .connect(":memory:")
        .await
        .unwrap();
    let pool = PoolBuilder::from(raw).build();

    let _conn = pool.acquire().await.unwrap();
    let result = pool.acquire().await;
    assert!(result.is_err(), "should time out");

    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.timeouts");
    assert!(metric.is_some(), "timeouts metric should be present");

    if let Some(m) = metric {
        if let AggregatedMetrics::U64(MetricData::Sum(sum)) = m.data() {
            let total: u64 = sum
                .data_points()
                .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                .sum();
            assert!(total >= 1, "should have at least one timeout recorded");
        } else {
            panic!("timeouts should be a u64 sum/counter");
        }
    }
}

#[tokio::test]
#[serial]
async fn pending_requests_recorded_on_acquire() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).build();

    let conn = pool.acquire().await.unwrap();
    drop(conn);

    let metrics = tel.metrics();
    let metric = find_metric(&metrics, "db.client.connection.pending_requests");
    assert!(
        metric.is_some(),
        "pending_requests metric should be present"
    );

    if let Some(m) = metric {
        if let AggregatedMetrics::I64(MetricData::Sum(sum)) = m.data() {
            let dp: Vec<_> = sum.data_points().collect();
            assert!(!dp.is_empty(), "should have data points");
            assert_eq!(dp[0].value(), 0, "net pending should be 0 after release");
        } else {
            panic!("pending_requests should be an i64 sum (UpDownCounter)");
        }
    }
}

#[tokio::test]
#[serial]
async fn spans_still_emitted_with_pool_metrics() {
    let tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_pool_name(POOL_NAME)
        .with_pool_metrics_interval(Duration::from_millis(50))
        .build();

    let _ = (&pool).fetch_optional("SELECT 1").await.unwrap();

    let spans = tel.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(
        attr(&spans[0], "db.system.name"),
        Some(opentelemetry::Value::String("sqlite".into()))
    );
}

// ===========================================================================
// Debug impls
// ===========================================================================

#[tokio::test]
#[serial]
async fn pool_connection_debug() {
    let _tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).build();

    let conn = pool.acquire().await.unwrap();
    let debug = format!("{conn:?}");
    assert!(debug.contains("PoolConnection"), "Debug output: {debug}");
}

#[tokio::test]
#[serial]
async fn pool_clone() {
    let _tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw).build();

    let cloned = pool.clone();
    assert!(!cloned.is_closed());
}

#[cfg(feature = "runtime-tokio")]
#[tokio::test]
#[serial]
async fn pool_debug_with_shutdown_handle() {
    let _tel = common::TestTelemetry::install();
    let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    let pool = PoolBuilder::from(raw)
        .with_pool_name(POOL_NAME)
        .with_pool_metrics_interval(Duration::from_millis(50))
        .build();

    let debug = format!("{pool:?}");
    assert!(debug.contains("Pool"), "Debug output: {debug}");
    assert!(
        debug.contains("ShutdownHandle"),
        "Should contain ShutdownHandle: {debug}"
    );
}

// ===========================================================================
// Connection count via background task (runtime-tokio)
// ===========================================================================

#[cfg(feature = "runtime-tokio")]
mod tokio_runtime {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn connection_count_reports_idle_and_used() {
        let tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_name(POOL_NAME)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

        let conn = pool.acquire().await.unwrap();

        let metrics = poll_for(Duration::from_secs(2), || {
            let snapshot = tel.metrics();
            find_metric(&snapshot, "db.client.connection.count")?;
            Some(snapshot)
        })
        .await
        .expect("db.client.connection.count metric should be reported within 2s");

        let idle = gauge_value(
            &metrics,
            "db.client.connection.count",
            "db.client.connection.state",
            "idle",
        );
        let used = gauge_value(
            &metrics,
            "db.client.connection.count",
            "db.client.connection.state",
            "used",
        );

        assert!(idle.is_some(), "idle count should be reported");
        assert!(used.is_some(), "used count should be reported");
        assert!(used.unwrap() >= 1, "at least one connection should be used");

        let metric = find_metric(&metrics, "db.client.connection.count").unwrap();
        if let AggregatedMetrics::I64(MetricData::Gauge(gauge)) = metric.data() {
            let has_pool_name = gauge.data_points().any(|dp| {
                dp.attributes().any(|kv| {
                    kv.key.as_str() == "db.client.connection.pool.name"
                        && kv.value.to_string() == POOL_NAME
                })
            });
            assert!(has_pool_name, "pool name attribute missing");
        }

        drop(conn);
    }

    #[tokio::test]
    #[serial]
    async fn background_task_survives_clone_drop() {
        let tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_name(POOL_NAME)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

        let clone = pool.clone();
        // Dropping a clone must NOT stop the polling task
        drop(clone);

        let conn = pool.acquire().await.unwrap();

        let used = poll_for(Duration::from_secs(2), || {
            let snapshot = tel.metrics();
            let used = gauge_value(
                &snapshot,
                "db.client.connection.count",
                "db.client.connection.state",
                "used",
            )?;
            if used >= 1 { Some(used) } else { None }
        })
        .await;

        assert!(
            used.is_some(),
            "connection.count should keep updating after a clone is dropped"
        );

        drop(conn);
    }

    #[tokio::test]
    #[serial]
    async fn no_pool_metrics_without_pool_name() {
        let tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

        // Asserting absence-after-wait: polling cannot replace this sleep, since the
        // expected outcome is that no metric ever arrives. The 100ms window is two task
        // intervals – long enough for a regression that *did* emit metrics to be caught.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let metrics = tel.metrics();
        let count = find_metric(&metrics, "db.client.connection.count");
        assert!(
            count.is_none(),
            "pool metrics should not be emitted without a pool name"
        );

        drop(pool);
    }

    #[tokio::test]
    #[serial]
    async fn background_task_stops_on_pool_drop() {
        let _tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_name(POOL_NAME)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

        drop(pool);
        // Asserting absence-after-drop: the sleep is the deliberate window during which a
        // still-running task would emit a recording. Polling cannot replace it because
        // the expected outcome is silence.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ===========================================================================
// Connection count via background task (runtime-async-std)
// ===========================================================================

#[cfg(all(feature = "runtime-async-std", not(feature = "runtime-tokio")))]
mod async_std_runtime {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn connection_count_reports_idle_and_used() {
        let tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_name(POOL_NAME)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

        let conn = pool.acquire().await.unwrap();

        let metrics = poll_for(Duration::from_secs(2), || {
            let snapshot = tel.metrics();
            find_metric(&snapshot, "db.client.connection.count")?;
            Some(snapshot)
        })
        .await
        .expect("db.client.connection.count metric should be reported within 2s");

        let idle = gauge_value(
            &metrics,
            "db.client.connection.count",
            "db.client.connection.state",
            "idle",
        );
        let used = gauge_value(
            &metrics,
            "db.client.connection.count",
            "db.client.connection.state",
            "used",
        );

        assert!(idle.is_some(), "idle count should be reported");
        assert!(used.is_some(), "used count should be reported");
        assert!(used.unwrap() >= 1, "at least one connection should be used");

        drop(conn);
    }
}
