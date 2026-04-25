use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry_semantic_conventions::metric;

/// Holds the OpenTelemetry metric instruments for per-operation recording.
///
/// Created once per [`Pool`](crate::Pool) and shared (via `Arc`) across all wrapper types
/// derived from that pool. Instruments are obtained from the globally configured
/// `MeterProvider`; when no provider is installed, they resolve to no-ops.
#[derive(Debug, Clone)]
pub(crate) struct Metrics {
    duration: Histogram<f64>,
    returned_rows: Histogram<f64>,
}

impl Metrics {
    /// Initialise instruments from the global `MeterProvider`.
    pub fn new() -> Self {
        let meter: Meter = opentelemetry::global::meter("sqlx-otel");
        let duration = meter
            .f64_histogram(metric::DB_CLIENT_OPERATION_DURATION)
            .with_unit("s")
            .with_description("Duration of database client operations.")
            .build();
        let returned_rows = meter
            .f64_histogram(metric::DB_CLIENT_RESPONSE_RETURNED_ROWS)
            .with_description("Number of rows returned by database operations.")
            .build();
        Self {
            duration,
            returned_rows,
        }
    }

    /// Record a completed operation's duration and, optionally, the number of rows returned.
    pub fn record(&self, elapsed: Duration, rows: Option<u64>, attributes: &[KeyValue]) {
        self.duration.record(elapsed.as_secs_f64(), attributes);
        if let Some(count) = rows {
            #[allow(clippy::cast_precision_loss)]
            self.returned_rows.record(count as f64, attributes);
        }
    }
}
