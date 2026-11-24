//! Transaction lifecycle telemetry, distinct from the connection lease behind it.
//!
//! Every outcome names what the *client* observed. None of them is a statement about the server:
//! see the outcome variants for what each one does and does not prove.

use std::future::Future;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};

use crate::metrics::{LATENCY_BUCKETS_SECONDS, TRANSACTION_BUCKETS_SECONDS};

const BEGIN_DURATION: &str = "sqlx_otel.transaction.begin.duration";
const BEGIN_OUTCOME: &str = "sqlx_otel.transaction.begin.outcome";
const TRANSACTION_DURATION: &str = "sqlx_otel.transaction.duration";
const TRANSACTION_OUTCOME: &str = "sqlx_otel.transaction.outcome";

/// How a `BEGIN` attempt on an already leased connection ended, as seen by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginOutcome {
    /// `BEGIN` returned `Ok`. The only outcome that starts a [`TransactionLifecycle`].
    Ok,
    /// `BEGIN` returned `Err`.
    Error,
    /// The future was dropped after `BEGIN` started and before its result was observed.
    Abandoned,
}

impl BeginOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Abandoned => "abandoned",
        }
    }
}

/// How a started transaction ended, as seen by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionOutcome {
    /// `COMMIT` returned `Ok`. The server acknowledged the statement, which is not proof that data
    /// committed: `PostgreSQL` answers `COMMIT` on an already failed transaction with a `ROLLBACK`
    /// tag and no error.
    CommitOk,
    /// `COMMIT` returned `Err`. A database error means it did not commit; a transport error after
    /// the statement was sent leaves the server outcome unknown.
    CommitError,
    /// The `commit()` future was dropped mid-flight. `COMMIT` may already have reached the server,
    /// so the outcome is **unknown**: neither a rollback nor a failed commit.
    CommitAbandoned,
    /// `ROLLBACK` returned `Ok`.
    RollbackOk,
    /// `ROLLBACK` returned `Err`.
    RollbackError,
    /// The `rollback()` future was dropped mid-flight. `SQLx` queues another rollback for the
    /// connection's next use; completion is unobserved.
    RollbackAbandoned,
    /// The transaction was dropped without a polled `commit()` or `rollback()`. `SQLx` only queued
    /// a rollback for the connection's next use, so this never confirms a server rollback.
    Dropped,
}

impl TransactionOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CommitOk => "commit_ok",
            Self::CommitError => "commit_error",
            Self::CommitAbandoned => "commit_abandoned",
            Self::RollbackOk => "rollback_ok",
            Self::RollbackError => "rollback_error",
            Self::RollbackAbandoned => "rollback_abandoned",
            Self::Dropped => "dropped",
        }
    }
}

/// The client request that ends a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionTermination {
    Commit,
    Rollback,
}

impl TransactionTermination {
    const fn abandoned(self) -> TransactionOutcome {
        match self {
            Self::Commit => TransactionOutcome::CommitAbandoned,
            Self::Rollback => TransactionOutcome::RollbackAbandoned,
        }
    }

    const fn observed(self, succeeded: bool) -> TransactionOutcome {
        match (self, succeeded) {
            (Self::Commit, true) => TransactionOutcome::CommitOk,
            (Self::Commit, false) => TransactionOutcome::CommitError,
            (Self::Rollback, true) => TransactionOutcome::RollbackOk,
            (Self::Rollback, false) => TransactionOutcome::RollbackError,
        }
    }
}

/// Instruments shared by every transaction started from one [`Pool`](crate::Pool).
#[derive(Debug)]
pub(crate) struct TransactionMetrics {
    begin_duration: Histogram<f64>,
    duration: Histogram<f64>,
}

impl TransactionMetrics {
    pub(crate) fn new(meter: &Meter) -> Self {
        Self {
            begin_duration: meter
                .f64_histogram(BEGIN_DURATION)
                .with_unit("s")
                .with_description(
                    "Time from holding a connection to observing the result of BEGIN, by outcome.",
                )
                .with_boundaries(LATENCY_BUCKETS_SECONDS.to_vec())
                .build(),
            duration: meter
                .f64_histogram(TRANSACTION_DURATION)
                .with_unit("s")
                .with_description(
                    "Time from a successful BEGIN to the terminal client observation, by outcome.",
                )
                .with_boundaries(TRANSACTION_BUCKETS_SECONDS.to_vec())
                .build(),
        }
    }

    /// Await `begin` on an already leased connection, recording exactly one `BEGIN` sample, and
    /// start the lifecycle only if it succeeded.
    ///
    /// A failed or cancelled `BEGIN` never started a transaction, so it records no
    /// `sqlx_otel.transaction.duration` sample. `attrs` are the lease's captured attributes.
    pub(crate) async fn observe_begin<T, F>(
        &self,
        attrs: &[KeyValue],
        begin: F,
    ) -> Result<(T, TransactionLifecycle), sqlx::Error>
    where
        F: Future<Output = Result<T, sqlx::Error>>,
    {
        let mut attempt = BeginAttempt {
            histogram: &self.begin_duration,
            started_at: Instant::now(),
            attrs,
            outcome: BeginOutcome::Abandoned,
        };
        let result = begin.await;
        attempt.outcome = if result.is_ok() {
            BeginOutcome::Ok
        } else {
            BeginOutcome::Error
        };
        drop(attempt);
        result.map(|inner| (inner, TransactionLifecycle::start(&self.duration, attrs)))
    }
}

/// `attrs` plus one bounded outcome attribute.
fn with_outcome(attrs: &[KeyValue], key: &'static str, outcome: &'static str) -> Vec<KeyValue> {
    let mut with_outcome = Vec::with_capacity(attrs.len() + 1);
    with_outcome.extend_from_slice(attrs);
    with_outcome.push(KeyValue::new(key, outcome));
    with_outcome
}

/// Records one `BEGIN` sample when dropped, so cancellation is counted by the same path.
struct BeginAttempt<'a> {
    histogram: &'a Histogram<f64>,
    started_at: Instant,
    attrs: &'a [KeyValue],
    outcome: BeginOutcome,
}

impl Drop for BeginAttempt<'_> {
    fn drop(&mut self) {
        self.histogram.record(
            self.started_at.elapsed().as_secs_f64(),
            &with_outcome(self.attrs, BEGIN_OUTCOME, self.outcome.as_str()),
        );
    }
}

/// Records one `sqlx_otel.transaction.duration` sample when dropped.
///
/// It is owned by the [`Transaction`](crate::Transaction), so whichever of `commit`, `rollback` or
/// plain drop consumes the transaction also consumes this, exactly once. Dropping it does nothing
/// but record: `SQLx` keeps sole ownership of ending the transaction.
#[derive(Debug)]
pub(crate) struct TransactionLifecycle {
    histogram: Histogram<f64>,
    started_at: Instant,
    attrs: Vec<KeyValue>,
    outcome: TransactionOutcome,
}

impl TransactionLifecycle {
    fn start(histogram: &Histogram<f64>, attrs: &[KeyValue]) -> Self {
        Self {
            histogram: histogram.clone(),
            started_at: Instant::now(),
            attrs: attrs.to_vec(),
            outcome: TransactionOutcome::Dropped,
        }
    }

    /// Await the terminal `operation`, classifying it from the moment it first runs.
    ///
    /// Because the classification happens inside this future's body, a `commit()` that is created
    /// but never polled stays [`Dropped`](TransactionOutcome::Dropped): `COMMIT` was never sent.
    pub(crate) async fn finish<F>(
        mut self,
        termination: TransactionTermination,
        operation: F,
    ) -> Result<(), sqlx::Error>
    where
        F: Future<Output = Result<(), sqlx::Error>>,
    {
        self.outcome = termination.abandoned();
        let result = operation.await;
        self.outcome = termination.observed(result.is_ok());
        result
    }
}

impl Drop for TransactionLifecycle {
    fn drop(&mut self) {
        self.histogram.record(
            self.started_at.elapsed().as_secs_f64(),
            &with_outcome(&self.attrs, TRANSACTION_OUTCOME, self.outcome.as_str()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BEGIN_DURATION, BEGIN_OUTCOME, TRANSACTION_DURATION, TRANSACTION_OUTCOME,
        TransactionLifecycle, TransactionMetrics, TransactionTermination,
    };
    use futures::executor::block_on;
    use futures::future::{pending, ready};
    use opentelemetry::KeyValue;
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{
        AggregatedMetrics, HistogramDataPoint, MetricData, ResourceMetrics, ScopeMetrics,
    };
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use std::time::Duration;

    /// A meter of its own for each test: nothing here touches the global provider.
    struct Harness {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
        metrics: TransactionMetrics,
        attrs: Vec<KeyValue>,
    }

    impl Harness {
        fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            let metrics = TransactionMetrics::new(&provider.meter("transaction-metrics-test"));
            Self {
                provider,
                exporter,
                metrics,
                attrs: vec![KeyValue::new("test.kind", "a")],
            }
        }

        /// `(outcome, count, sum)` per data point of `name`, sorted by outcome.
        fn samples(&self, name: &str, outcome_key: &str) -> Vec<(String, u64, f64)> {
            self.provider.force_flush().unwrap();
            let snapshots = self.exporter.get_finished_metrics().unwrap();
            let Some(metric) = snapshots
                .iter()
                .rev()
                .flat_map(ResourceMetrics::scope_metrics)
                .flat_map(ScopeMetrics::metrics)
                .find(|metric| metric.name() == name)
            else {
                return Vec::new();
            };
            assert_eq!(metric.unit(), "s");
            let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
                panic!("{name} must be an f64 histogram");
            };
            let mut samples: Vec<_> = histogram
                .data_points()
                .map(|point: &HistogramDataPoint<f64>| {
                    assert!(
                        point
                            .attributes()
                            .any(|kv| kv == &KeyValue::new("test.kind", "a")),
                        "the captured attributes must be kept"
                    );
                    let outcome = point
                        .attributes()
                        .find(|kv| kv.key.as_str() == outcome_key)
                        .expect("every sample carries its outcome")
                        .value
                        .to_string();
                    (outcome, point.count(), point.sum())
                })
                .collect();
            samples.sort_by(|left, right| left.0.cmp(&right.0));
            samples
        }

        fn begins(&self) -> Vec<(String, u64)> {
            self.samples(BEGIN_DURATION, BEGIN_OUTCOME)
                .into_iter()
                .map(|(outcome, count, _)| (outcome, count))
                .collect()
        }

        fn transactions(&self) -> Vec<(String, u64)> {
            self.samples(TRANSACTION_DURATION, TRANSACTION_OUTCOME)
                .into_iter()
                .map(|(outcome, count, _)| (outcome, count))
                .collect()
        }

        fn started(&self) -> TransactionLifecycle {
            let ((), lifecycle) =
                block_on(self.metrics.observe_begin(&self.attrs, ready(Ok(())))).unwrap();
            lifecycle
        }
    }

    fn one(outcome: &str) -> Vec<(String, u64)> {
        vec![(outcome.to_owned(), 1)]
    }

    #[test]
    fn successful_begin_starts_a_lifecycle_that_records_only_when_it_ends() {
        let harness = Harness::new();
        let lifecycle = harness.started();
        assert_eq!(harness.begins(), one("ok"));
        assert_eq!(harness.transactions(), Vec::new(), "still open");
        drop(lifecycle);
        assert_eq!(harness.transactions(), one("dropped"));
    }

    #[test]
    fn failed_begin_records_one_error_and_no_transaction() {
        let harness = Harness::new();
        let result = block_on(
            harness
                .metrics
                .observe_begin::<(), _>(&harness.attrs, ready(Err(sqlx::Error::PoolClosed))),
        );
        assert!(matches!(result, Err(sqlx::Error::PoolClosed)));
        assert_eq!(harness.begins(), one("error"));
        assert_eq!(harness.transactions(), Vec::new());
    }

    #[test]
    fn dropped_begin_records_one_abandonment_and_no_transaction() {
        let harness = Harness::new();
        block_on(async {
            let mut begin = Box::pin(
                harness
                    .metrics
                    .observe_begin::<(), _>(&harness.attrs, pending()),
            );
            assert!(futures::poll!(&mut begin).is_pending());
            assert_eq!(harness.begins(), Vec::new(), "the attempt is still running");
            drop(begin);
        });
        assert_eq!(harness.begins(), one("abandoned"));
        assert_eq!(harness.transactions(), Vec::new());
    }

    #[test]
    fn never_polled_begin_records_nothing() {
        let harness = Harness::new();
        drop(
            harness
                .metrics
                .observe_begin::<(), _>(&harness.attrs, pending()),
        );
        assert_eq!(harness.begins(), Vec::new());
        assert_eq!(harness.transactions(), Vec::new());
    }

    #[test]
    fn every_terminal_result_maps_to_one_outcome() {
        for (termination, result, expected) in [
            (TransactionTermination::Commit, Ok(()), "commit_ok"),
            (
                TransactionTermination::Commit,
                Err(sqlx::Error::PoolClosed),
                "commit_error",
            ),
            (TransactionTermination::Rollback, Ok(()), "rollback_ok"),
            (
                TransactionTermination::Rollback,
                Err(sqlx::Error::PoolClosed),
                "rollback_error",
            ),
        ] {
            let harness = Harness::new();
            let is_ok = result.is_ok();
            let finished = block_on(harness.started().finish(termination, ready(result)));
            assert_eq!(finished.is_ok(), is_ok, "the result must pass through");
            assert_eq!(harness.transactions(), one(expected));
        }
    }

    /// A cancelled terminal call must say "abandoned", never rollback or a failed commit.
    #[test]
    fn dropping_a_started_terminal_call_records_its_abandonment_once() {
        for (termination, expected) in [
            (TransactionTermination::Commit, "commit_abandoned"),
            (TransactionTermination::Rollback, "rollback_abandoned"),
        ] {
            let harness = Harness::new();
            let lifecycle = harness.started();
            block_on(async {
                let mut finish = Box::pin(lifecycle.finish(termination, pending()));
                assert!(futures::poll!(&mut finish).is_pending());
                assert_eq!(harness.transactions(), Vec::new(), "still in flight");
                drop(finish);
            });
            assert_eq!(harness.transactions(), one(expected));
        }
    }

    /// `COMMIT` was never sent, so this must not read as an abandoned commit.
    #[test]
    fn a_never_polled_terminal_call_counts_as_a_plain_drop() {
        for termination in [
            TransactionTermination::Commit,
            TransactionTermination::Rollback,
        ] {
            let harness = Harness::new();
            drop(harness.started().finish(termination, pending()));
            assert_eq!(harness.transactions(), one("dropped"));
        }
    }

    #[tokio::test]
    async fn begin_time_and_transaction_time_do_not_overlap() {
        let harness = Harness::new();
        let ((), lifecycle) = harness
            .metrics
            .observe_begin(&harness.attrs, async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(())
            })
            .await
            .unwrap();
        lifecycle
            .finish(TransactionTermination::Commit, ready(Ok(())))
            .await
            .unwrap();
        let begin = harness.samples(BEGIN_DURATION, BEGIN_OUTCOME);
        let transaction = harness.samples(TRANSACTION_DURATION, TRANSACTION_OUTCOME);
        assert!(begin[0].2 >= 0.2, "BEGIN time: {}", begin[0].2);
        assert!(
            transaction[0].2 < 0.2,
            "transaction time starts after BEGIN: {}",
            transaction[0].2
        );
    }
}
