#![cfg(feature = "sqlite")]

mod common;

use common::attr;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use serial_test::serial;
use sqlx::Executor as _;
use sqlx_otel::PoolBuilder;
use std::time::Duration;

const POOL_NAME: &str = "test-pool";

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
        tokio::time::sleep(Duration::from_millis(100)).await;

        let metrics = tel.metrics();

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
    async fn no_pool_metrics_without_pool_name() {
        let tel = common::TestTelemetry::install();
        let raw = sqlx::SqlitePool::connect(":memory:").await.unwrap();
        let pool = PoolBuilder::from(raw)
            .with_pool_metrics_interval(Duration::from_millis(50))
            .build();

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
        tokio::time::sleep(Duration::from_millis(100)).await;

        let metrics = tel.metrics();

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
