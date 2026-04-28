//! End-to-end example: wrap an in-memory `SQLite` pool, install an in-memory `OpenTelemetry`
//! tracer/meter, run a small CRUD sequence, and print the emitted spans and metrics.
//!
//! Run with:
//!
//! ```text
//! cargo run --example sqlite --features sqlite,runtime-tokio
//! ```
//!
//! In a real application, replace the in-memory exporters with whichever exporter ships
//! to your collector (OTLP, stdout, Jaeger, etc.). The wrapper itself is unchanged –
//! installing a provider is the application's responsibility.

use opentelemetry::global;
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use sqlx::Row as _;
use sqlx_otel::{PoolBuilder, QueryAnnotateExt as _, QueryAnnotations};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let span_exporter = InMemorySpanExporter::default();
    let tracer_provider = SdkTracerProvider::builder()
        .with_simple_exporter(span_exporter.clone())
        .build();
    global::set_tracer_provider(tracer_provider.clone());

    let metric_exporter = InMemoryMetricExporter::default();
    let metric_reader = PeriodicReader::builder(metric_exporter.clone()).build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(metric_reader)
        .build();
    global::set_meter_provider(meter_provider.clone());

    let raw = sqlx::SqlitePool::connect(":memory:").await?;
    let pool = PoolBuilder::from(raw).build();

    sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .with_operation("CREATE", "users")
        .execute(&pool)
        .await?;

    sqlx::query("INSERT INTO users (id, name) VALUES (?, ?)")
        .bind(1_i64)
        .bind("Alice")
        .with_annotations(
            QueryAnnotations::new()
                .operation("INSERT")
                .collection("users"),
        )
        .execute(&pool)
        .await?;

    let row = sqlx::query("SELECT name FROM users WHERE id = ?")
        .bind(1_i64)
        .with_operation("SELECT", "users")
        .fetch_one(&pool)
        .await?;
    let name: String = row.get("name");
    println!("fetched user: {name}\n");

    let _ = tracer_provider.force_flush();
    let _ = meter_provider.force_flush();

    println!("--- emitted spans ---");
    for span in span_exporter.get_finished_spans()? {
        println!("span: {}", span.name);
        for kv in &span.attributes {
            println!("  {} = {:?}", kv.key, kv.value);
        }
    }

    println!("\n--- emitted metrics ---");
    for resource_metrics in metric_exporter.get_finished_metrics()? {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                println!("metric: {}", metric.name());
            }
        }
    }

    let _ = tracer_provider.shutdown();
    let _ = meter_provider.shutdown();
    Ok(())
}
