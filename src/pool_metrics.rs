//! Background task that periodically records `db.client.connection.count` by polling
//! `sqlx::Pool` for the current number of idle and used connections.
//!
//! Spawned by [`PoolBuilder::build()`](crate::PoolBuilder::build) when a pool name is
//! configured and a runtime feature (e.g. `runtime-tokio`) is enabled. The task stops
//! only once the [`Pool`](crate::Pool) and every clone of it have been dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Handle that signals the background polling task to stop when the last clone is
/// dropped.
#[derive(Clone)]
pub(crate) struct ShutdownHandle {
    flag: Arc<AtomicBool>,
}

impl Drop for ShutdownHandle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.flag) == 1 {
            self.flag.store(true, Ordering::Relaxed);
        }
    }
}

impl std::fmt::Debug for ShutdownHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownHandle").finish_non_exhaustive()
    }
}

/// Spawn a background task that polls `db.client.connection.count` at the given interval.
///
/// Returns a [`ShutdownHandle`] that signals the task to stop when dropped.
#[cfg(any(feature = "runtime-tokio", feature = "runtime-async-std"))]
pub(crate) fn spawn<R: crate::runtime::Runtime, DB: sqlx::Database>(
    pool: sqlx::Pool<DB>,
    pool_name: String,
    interval: std::time::Duration,
) -> ShutdownHandle {
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::{Gauge, Meter};
    use opentelemetry_semantic_conventions::{attribute, metric};

    fn record<D: sqlx::Database>(
        count: &Gauge<i64>,
        pool: &sqlx::Pool<D>,
        base_attrs: &[KeyValue],
    ) {
        #[allow(clippy::cast_possible_wrap)] // connection count will never exceed i64::MAX
        let idle = pool.num_idle() as i64;
        let total = i64::from(pool.size());
        let used = total - idle;

        let mut idle_attrs = Vec::with_capacity(base_attrs.len() + 1);
        idle_attrs.extend_from_slice(base_attrs);
        idle_attrs.push(KeyValue::new(attribute::DB_CLIENT_CONNECTION_STATE, "idle"));

        let mut used_attrs = Vec::with_capacity(base_attrs.len() + 1);
        used_attrs.extend_from_slice(base_attrs);
        used_attrs.push(KeyValue::new(attribute::DB_CLIENT_CONNECTION_STATE, "used"));

        count.record(idle, &idle_attrs);
        count.record(used, &used_attrs);
    }

    let flag = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::downgrade(&flag);

    R::spawn(async move {
        let meter: Meter = opentelemetry::global::meter("sqlx-otel");
        let count = meter
            .i64_gauge(metric::DB_CLIENT_CONNECTION_COUNT)
            .with_description("The number of connections currently in the state described by the `db.client.connection.state` attribute.")
            .build();
        let base_attrs = vec![KeyValue::new(
            attribute::DB_CLIENT_CONNECTION_POOL_NAME,
            pool_name,
        )];

        loop {
            R::sleep(interval).await;
            match shutdown.upgrade() {
                Some(flag) if !flag.load(Ordering::Relaxed) => {
                    record(&count, &pool, &base_attrs);
                }
                _ => break,
            }
        }
    });

    ShutdownHandle { flag }
}
