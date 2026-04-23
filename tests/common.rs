#![allow(dead_code)]

use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

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
    pub fn spans(&self) -> Vec<opentelemetry_sdk::trace::SpanData> {
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
}

impl Drop for TestTelemetry {
    fn drop(&mut self) {
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
    }
}
