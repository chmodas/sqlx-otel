use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry_semantic_conventions::metric;

/// Explicit bucket boundaries, in seconds, for every latency histogram the crate records.
///
/// The `OTel` SDK's default boundaries (`0, 5, 10, 25, ... 10000`) are shaped for milliseconds,
/// but semconv requires these histograms be recorded in seconds. Left on the defaults, every
/// observation below five seconds falls into a single bucket, and quantiles computed from it are
/// interpolation artefacts rather than measurements. These boundaries span the range a database
/// client actually occupies: sub-millisecond local round trips up to the 30-second acquire
/// timeout `SQLx` applies by default.
///
/// This is instrument *advice*, not a hard configuration. An application that registers a
/// `View` with its own explicit aggregation overrides these boundaries, because the SDK only
/// applies advice when no matching view has already set an aggregation. The exception is a view
/// whose aggregation is incompatible with the instrument: the SDK rejects it, falls back to the
/// implicit default view, and applies this advice after all.
pub(crate) const LATENCY_BUCKETS_SECONDS: &[f64] = &[
    0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    30.0, 60.0,
];

/// Holds the OpenTelemetry metric instruments for per-operation recording.
///
/// Created once per [`Pool`](crate::Pool) and shared (via `Arc`) across all wrapper types
/// derived from that pool. Instruments are obtained from the globally configured
/// `MeterProvider`; when no provider is installed, they resolve to no-ops.
#[derive(Debug, Clone)]
pub(crate) struct Metrics {
    duration: Histogram<f64>,
    returned_rows: Histogram<f64>,
    /// Custom histogram (no `OTel` semconv equivalent) recording the database-confirmed
    /// `rows_affected()` count for `execute()` operations. Mirrors the existing
    /// `db.response.affected_rows` span attribute so dashboards can slice mutation
    /// throughput by the same dimensions.
    affected_rows: Histogram<f64>,
}

impl Metrics {
    /// Initialise instruments from the global `MeterProvider`.
    pub fn new() -> Self {
        let meter: Meter = opentelemetry::global::meter("sqlx-otel");
        let duration = meter
            .f64_histogram(metric::DB_CLIENT_OPERATION_DURATION)
            .with_unit("s")
            .with_description("Duration of database client operations.")
            .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
            .build();
        let returned_rows = meter
            .f64_histogram(metric::DB_CLIENT_RESPONSE_RETURNED_ROWS)
            .with_description("Number of rows returned by database operations.")
            .build();
        let affected_rows = meter
            .f64_histogram("db.client.response.affected_rows")
            .with_description("Number of rows affected by database operations.")
            .build();
        Self {
            duration,
            returned_rows,
            affected_rows,
        }
    }

    /// Record a completed operation: always the duration histogram; `returned_rows` and
    /// `affected_rows` histograms when their respective counts are `Some`. The two row-
    /// count parameters are mutually exclusive in practice (a `fetch*` operation sets
    /// `returned_rows`; an `execute` operation sets `affected_rows`), but the signature
    /// allows both for forward compatibility with backends that report both for a single
    /// operation.
    pub fn record(
        &self,
        elapsed: Duration,
        returned_rows: Option<u64>,
        affected_rows: Option<u64>,
        attributes: &[KeyValue],
    ) {
        self.duration.record(elapsed.as_secs_f64(), attributes);
        if let Some(count) = returned_rows {
            #[allow(clippy::cast_precision_loss)]
            self.returned_rows.record(count as f64, attributes);
        }
        if let Some(count) = affected_rows {
            #[allow(clippy::cast_precision_loss)]
            self.affected_rows.record(count as f64, attributes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LATENCY_BUCKETS_SECONDS;

    /// The boundary set must be well-formed for an explicit-bucket histogram.
    ///
    /// The `OTel` SDK requires strictly ascending, finite boundaries; a duplicate or out-of-order
    /// entry produces an empty or misattributed bucket rather than an error at build time. This
    /// reports a mistyped edit here, rather than as a vector diff in an integration test.
    #[test]
    fn latency_buckets_are_strictly_ascending_and_positive() {
        assert!(
            !LATENCY_BUCKETS_SECONDS.is_empty(),
            "boundary set must not be empty"
        );
        // Evaluate the comparison for every pair up front rather than inside an `assert!`
        // message, so the check itself runs on the passing path instead of only on failure.
        let ascending = LATENCY_BUCKETS_SECONDS
            .windows(2)
            .all(|pair| pair[0] < pair[1]);
        assert!(
            ascending,
            "boundaries must strictly ascend: {LATENCY_BUCKETS_SECONDS:?}"
        );
        for &bound in LATENCY_BUCKETS_SECONDS {
            assert!(bound.is_finite(), "boundary {bound} is not finite");
            assert!(bound > 0.0, "boundary {bound} must be positive");
        }
    }

    /// The set must cover the range a database client operates in.
    ///
    /// The lower bound has to resolve sub-millisecond local round trips, which is why the crate
    /// ships boundaries at all. The upper bound has to reach past `SQLx`'s 30-second default
    /// acquire timeout, or every timed-out acquisition lands in the overflow bucket and the p99
    /// of `wait_time` stops being readable when it matters most.
    #[test]
    fn latency_buckets_span_submillisecond_to_beyond_the_default_acquire_timeout() {
        let first = LATENCY_BUCKETS_SECONDS[0];
        let last = LATENCY_BUCKETS_SECONDS[LATENCY_BUCKETS_SECONDS.len() - 1];
        assert!(
            first <= 0.001,
            "lowest boundary {first} cannot resolve submillisecond acquisition"
        );
        assert!(
            last >= 30.0,
            "highest boundary {last} does not reach the default acquire timeout"
        );
    }
}
