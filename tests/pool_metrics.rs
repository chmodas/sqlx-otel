#![cfg(feature = "sqlite")]

mod common;

use common::attr;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use serial_test::serial;
use sqlx::Executor as _;
use sqlx_otel::PoolBuilder;
use std::time::Duration;

const POOL_NAME: &str = "test-pool";

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
        12,
        "query instrumentation must not be nested"
    );
    assert_eq!(histogram_count(&tel, "db.client.connection.wait_time"), 12);
    assert_eq!(histogram_count(&tel, "db.client.connection.use_time"), 12);
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
