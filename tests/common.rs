#![allow(dead_code, clippy::must_use_candidate, clippy::missing_panics_doc)]

use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};

/// Test harness that installs in-memory span and metric exporters as the global providers,
/// collects telemetry in-process, and cleans up on drop.
pub struct TestTelemetry {
    span_exporter: InMemorySpanExporter,
    metric_exporter: InMemoryMetricExporter,
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
}

impl TestTelemetry {
    /// Install in-memory exporters as the global tracer and meter providers.
    #[must_use]
    pub fn install() -> Self {
        let span_exporter = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder()
            .with_simple_exporter(span_exporter.clone())
            .build();
        opentelemetry::global::set_tracer_provider(tracer_provider.clone());

        let metric_exporter = InMemoryMetricExporter::default();
        let reader = PeriodicReader::builder(metric_exporter.clone()).build();
        let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();
        opentelemetry::global::set_meter_provider(meter_provider.clone());

        Self {
            span_exporter,
            metric_exporter,
            tracer_provider,
            meter_provider,
        }
    }

    /// Return all finished spans, flushing the provider first.
    #[must_use]
    pub fn spans(&self) -> Vec<SpanData> {
        let _ = self.tracer_provider.force_flush();
        self.span_exporter.get_finished_spans().unwrap_or_default()
    }

    /// Return all finished metrics, flushing the provider first.
    #[must_use]
    pub fn metrics(&self) -> Vec<opentelemetry_sdk::metrics::data::ResourceMetrics> {
        let _ = self.meter_provider.force_flush();
        self.metric_exporter
            .get_finished_metrics()
            .unwrap_or_default()
    }

    /// Drain the in-memory exporters so the next call to [`spans`](Self::spans) or
    /// [`metrics`](Self::metrics) sees a fresh window.
    ///
    /// Use this between sections of a single test that want to assert on a bounded set of
    /// spans/metrics, instead of re-installing the global telemetry providers (which is
    /// racy: a fresh `install()` mid-test replaces the global tracer/meter providers and
    /// silently changes which exporter receives subsequent operations).
    pub fn reset(&self) {
        let _ = self.tracer_provider.force_flush();
        let _ = self.meter_provider.force_flush();
        self.span_exporter.reset();
        self.metric_exporter.reset();
    }
}

impl Drop for TestTelemetry {
    fn drop(&mut self) {
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Shared assertion helpers
// ---------------------------------------------------------------------------

/// Find the attribute value for a given key in a span.
pub fn attr(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

/// Assert that a span carries the common attributes every instrumented operation must have.
///
/// `system` is the expected `db.system.name` value (e.g. `"sqlite"`, `"postgresql"`).
///
/// `db.namespace` and `db.query.text` are checked for non-empty string content rather
/// than mere presence, so a regression that emits empty strings or non-string values is
/// caught at the helper level.
pub fn assert_common_span_attributes(span: &SpanData, system: &str) {
    assert_eq!(span.span_kind, SpanKind::Client);
    assert_eq!(
        span.name, system,
        "span name should fall back to db.system.name"
    );
    assert_eq!(
        attr(span, "db.system.name"),
        Some(opentelemetry::Value::String(system.to_owned().into())),
        "db.system.name missing or wrong"
    );
    let namespace = attr(span, "db.namespace");
    assert!(
        matches!(&namespace, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "db.namespace should be a non-empty string, got {namespace:?}",
    );
    let query_text = attr(span, "db.query.text");
    assert!(
        matches!(&query_text, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "db.query.text should be a non-empty string, got {query_text:?}",
    );
}

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Row shape for the shared `macro_users` table used by the macro-form integration tests.
///
/// Generic over the primary-key type so each backend can pick the variant that matches its
/// column type (sqlite `INTEGER` decodes to `i64`; postgres `INTEGER` and mysql `INT`
/// decode to `i32`).
#[derive(Debug, sqlx::FromRow)]
pub struct MacroUser<Id> {
    pub id: Id,
    pub name: String,
}

/// Assert that a span has error status, an `error.type` attribute, and an exception event
/// with `exception.type` and `exception.message`.
///
/// All three attribute values are required to be non-empty strings, so a regression that
/// emits empty exception metadata is caught here rather than slipping past the suite.
pub fn assert_error_span(span: &SpanData) {
    assert!(
        matches!(&span.status, Status::Error { .. }),
        "span status should be Error, got {:?}",
        span.status
    );
    let error_type = attr(span, "error.type");
    assert!(
        matches!(&error_type, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "error.type should be a non-empty string, got {error_type:?}",
    );
    let event = span
        .events
        .iter()
        .find(|e| e.name == "exception")
        .expect("exception event missing");
    let exception_type = event
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "exception.type")
        .map(|kv| kv.value.clone());
    assert!(
        matches!(&exception_type, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "exception.type should be a non-empty string, got {exception_type:?}",
    );
    let exception_message = event
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "exception.message")
        .map(|kv| kv.value.clone());
    assert!(
        matches!(&exception_message, Some(opentelemetry::Value::String(s)) if !s.as_str().is_empty()),
        "exception.message should be a non-empty string, got {exception_message:?}",
    );
}
