use std::borrow::Cow;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use futures::Stream;
use futures::stream::BoxStream;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context as OtelContext, KeyValue};
use opentelemetry_semantic_conventions::attribute;

use crate::attributes::{self, ConnectionAttributes, QueryTextMode};
use crate::database::Database;
use crate::metrics::Metrics;

// ---------------------------------------------------------------------------
// Span helpers
// ---------------------------------------------------------------------------

/// Build span attributes for a query, combining connection-level and per-query values.
fn build_attributes(
    attrs: &ConnectionAttributes,
    sql: Option<&str>,
    operation: Option<&str>,
    collection: Option<&str>,
) -> Vec<KeyValue> {
    let mut kv = attrs.base_key_values();
    if let Some(op) = operation {
        kv.push(KeyValue::new(attribute::DB_OPERATION_NAME, op.to_owned()));
    }
    if let Some(coll) = collection {
        kv.push(KeyValue::new(
            attribute::DB_COLLECTION_NAME,
            coll.to_owned(),
        ));
    }
    if let Some(sql) = sql {
        match attrs.query_text_mode {
            QueryTextMode::Full => {
                kv.push(KeyValue::new(attribute::DB_QUERY_TEXT, sql.to_owned()));
            }
            // Obfuscation not yet implemented; suppress to avoid leaking literals.
            QueryTextMode::Obfuscated | QueryTextMode::Off => {}
        }
    }
    kv
}

/// Create an OpenTelemetry span for a database operation and return a context containing it.
fn start_span(name: &str, span_attrs: Vec<KeyValue>) -> (OtelContext, Instant) {
    let tracer = opentelemetry::global::tracer("sqlx-otel");
    let span = tracer
        .span_builder(name.to_owned())
        .with_kind(SpanKind::Client)
        .with_attributes(span_attrs)
        .start(&tracer);
    let cx = OtelContext::current_with_span(span);
    (cx, Instant::now())
}

/// Classify a `sqlx::Error` variant into a string suitable for `error.type`.
fn error_type(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Configuration(_) => "Configuration",
        sqlx::Error::Database(_) => "Database",
        sqlx::Error::Io(_) => "Io",
        sqlx::Error::Tls(_) => "Tls",
        sqlx::Error::Protocol(_) => "Protocol",
        sqlx::Error::RowNotFound => "RowNotFound",
        sqlx::Error::TypeNotFound { .. } => "TypeNotFound",
        sqlx::Error::ColumnIndexOutOfBounds { .. } => "ColumnIndexOutOfBounds",
        sqlx::Error::ColumnNotFound(_) => "ColumnNotFound",
        sqlx::Error::ColumnDecode { .. } => "ColumnDecode",
        sqlx::Error::Decode(_) => "Decode",
        sqlx::Error::AnyDriverError(_) => "AnyDriverError",
        sqlx::Error::PoolTimedOut => "PoolTimedOut",
        sqlx::Error::PoolClosed => "PoolClosed",
        sqlx::Error::WorkerCrashed => "WorkerCrashed",
        sqlx::Error::Migrate(_) => "Migrate",
        _ => "Unknown",
    }
}

/// Record an error on the span within the given context: set status, `error.type`, and
/// add an exception event.
fn record_error(cx: &OtelContext, err: &sqlx::Error) {
    let span = cx.span();
    span.set_status(Status::Error {
        description: Cow::Owned(err.to_string()),
    });
    span.set_attribute(KeyValue::new(attribute::ERROR_TYPE, error_type(err)));
    // Extract SQLSTATE or database-specific error code when available.
    if let sqlx::Error::Database(db_err) = err {
        if let Some(code) = db_err.code() {
            span.set_attribute(KeyValue::new(
                attribute::DB_RESPONSE_STATUS_CODE,
                code.into_owned(),
            ));
        }
    }
    span.add_event(
        "exception",
        vec![
            KeyValue::new("exception.type", error_type(err)),
            KeyValue::new("exception.message", err.to_string()),
        ],
    );
}

/// Record success attributes (returned rows) on the span.
fn record_rows(cx: &OtelContext, rows: u64) {
    cx.span().set_attribute(KeyValue::new(
        attribute::DB_RESPONSE_RETURNED_ROWS,
        i64::try_from(rows).unwrap_or(i64::MAX),
    ));
}

/// End the span and record metrics.
/// End the span and record metrics.
fn finish(
    cx: &OtelContext,
    start: Instant,
    rows: Option<u64>,
    metrics: &Metrics,
    attrs: &[KeyValue],
) {
    cx.span().end();
    metrics.record(start.elapsed(), rows, attrs);
}

/// Await a future, record any error on the span, then finish. Used by `execute`, `prepare`,
/// `prepare_with`, and `describe` which share the same instrumentation pattern.
async fn execute_instrumented<T>(
    fut: futures::future::BoxFuture<'_, Result<T, sqlx::Error>>,
    cx: OtelContext,
    start: Instant,
    metrics: std::sync::Arc<Metrics>,
    metric_attrs: Vec<KeyValue>,
) -> Result<T, sqlx::Error> {
    let result = fut.await;
    if let Err(err) = &result {
        record_error(&cx, err);
    }
    finish(&cx, start, None, &metrics, &metric_attrs);
    result
}

// ---------------------------------------------------------------------------
// InstrumentedStream – keeps the span alive for streaming operations
// ---------------------------------------------------------------------------

/// Trait that determines how many rows a stream item represents.
trait RowCounter<T> {
    /// Return the number of rows this item contributes.
    fn count(item: &T) -> u64;
}

/// Counts every item as one row. Used for `fetch` (which yields `Row`).
struct CountAll;

impl<T> RowCounter<T> for CountAll {
    fn count(_item: &T) -> u64 {
        1
    }
}

/// Counts only `Either::Right` items as rows. Used for `fetch_many` (which yields
/// `Either<QueryResult, Row>`).
struct CountRight;

impl<L, R> RowCounter<sqlx::Either<L, R>> for CountRight {
    fn count(item: &sqlx::Either<L, R>) -> u64 {
        u64::from(item.is_right())
    }
}

/// Counts nothing. Used for `execute_many` (which yields `QueryResult`, not rows).
struct CountNone;

impl<T> RowCounter<T> for CountNone {
    fn count(_item: &T) -> u64 {
        0
    }
}

/// A stream wrapper that holds an OpenTelemetry context (keeping the span alive), counts rows,
/// and records metrics when the stream completes or is dropped.
struct InstrumentedStream<S, C> {
    inner: S,
    cx: OtelContext,
    start: Instant,
    rows: u64,
    metrics: std::sync::Arc<Metrics>,
    metric_attrs: Vec<KeyValue>,
    finished: bool,
    _counter: std::marker::PhantomData<C>,
}

impl<S, C> InstrumentedStream<S, C> {
    fn new(
        inner: S,
        cx: OtelContext,
        start: Instant,
        metrics: std::sync::Arc<Metrics>,
        metric_attrs: Vec<KeyValue>,
    ) -> Self {
        Self {
            inner,
            cx,
            start,
            rows: 0,
            metrics,
            metric_attrs,
            finished: false,
            _counter: std::marker::PhantomData,
        }
    }

    fn complete(&mut self) {
        if !self.finished {
            self.finished = true;
            record_rows(&self.cx, self.rows);
            finish(
                &self.cx,
                self.start,
                Some(self.rows),
                &self.metrics,
                &self.metric_attrs,
            );
        }
    }
}

// Safety: all fields are Unpin (inner S is bounded Unpin, the rest are owned values).
// PhantomData<C> prevents auto-Unpin, so we impl it explicitly.
impl<S: Unpin, C> Unpin for InstrumentedStream<S, C> {}

impl<S, T, C> Stream for InstrumentedStream<S, C>
where
    S: Stream<Item = Result<T, sqlx::Error>> + Unpin,
    C: RowCounter<T>,
{
    type Item = Result<T, sqlx::Error>;

    fn poll_next(mut self: Pin<&mut Self>, task_cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(task_cx) {
            Poll::Ready(Some(Ok(item))) => {
                self.rows += C::count(&item);
                Poll::Ready(Some(Ok(item)))
            }
            Poll::Ready(Some(Err(err))) => {
                record_error(&self.cx, &err);
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                self.complete();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S, C> Drop for InstrumentedStream<S, C> {
    fn drop(&mut self) {
        self.complete();
    }
}

// ---------------------------------------------------------------------------
// Macro to reduce Executor impl boilerplate
// ---------------------------------------------------------------------------

/// Generate the full `sqlx::Executor` implementation for one of our wrapper types.
///
/// Each method extracts the SQL string, builds an OpenTelemetry span with connection attributes,
/// delegates to the inner executor, and records metrics and errors on completion.
macro_rules! impl_executor {
    ($ty:ty, $self_:ident => $inner:expr) => {
        impl<'c, DB> sqlx::Executor<'c> for $ty
        where
            DB: Database,
            for<'a> &'a mut DB::Connection: sqlx::Executor<'a, Database = DB>,
        {
            type Database = DB;

            /// Execute the query and return the total number of rows affected.
            fn execute<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> futures::future::BoxFuture<
                'e,
                Result<<DB as sqlx::Database>::QueryResult, sqlx::Error>,
            >
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).execute(query);
                Box::pin(execute_instrumented(
                    fut, cx, start, state.metrics, metric_attrs,
                ))
            }

            /// Execute multiple queries and return the rows affected from each query, in a stream.
            fn execute_many<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> BoxStream<'e, Result<<DB as sqlx::Database>::QueryResult, sqlx::Error>>
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let stream = ($inner).execute_many(query);
                Box::pin(InstrumentedStream::<_, CountNone>::new(
                    stream,
                    cx,
                    start,
                    state.metrics,
                    metric_attrs,
                ))
            }

            /// Execute the query and return the generated results as a stream.
            fn fetch<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> BoxStream<'e, Result<<DB as sqlx::Database>::Row, sqlx::Error>>
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let stream = ($inner).fetch(query);
                Box::pin(InstrumentedStream::<_, CountAll>::new(
                    stream,
                    cx,
                    start,
                    state.metrics,
                    metric_attrs,
                ))
            }

            /// Execute multiple queries and return the generated results as a stream
            /// from each query, in a stream.
            fn fetch_many<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> BoxStream<
                'e,
                Result<
                    sqlx::Either<<DB as sqlx::Database>::QueryResult, <DB as sqlx::Database>::Row>,
                    sqlx::Error,
                >,
            >
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let stream = ($inner).fetch_many(query);
                Box::pin(InstrumentedStream::<_, CountRight>::new(
                    stream,
                    cx,
                    start,
                    state.metrics,
                    metric_attrs,
                ))
            }

            /// Execute the query and return all the generated results, collected into a [`Vec`].
            fn fetch_all<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> futures::future::BoxFuture<
                'e,
                Result<Vec<<DB as sqlx::Database>::Row>, sqlx::Error>,
            >
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).fetch_all(query);
                Box::pin(async move {
                    let result = fut.await;
                    match &result {
                        Ok(rows) => {
                            let count = rows.len() as u64;
                            record_rows(&cx, count);
                            finish(&cx, start, Some(count), &state.metrics, &metric_attrs);
                        }
                        Err(err) => {
                            record_error(&cx, err);
                            finish(&cx, start, None, &state.metrics, &metric_attrs);
                        }
                    }
                    result
                })
            }

            /// Execute the query and returns exactly one row.
            fn fetch_one<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> futures::future::BoxFuture<
                'e,
                Result<<DB as sqlx::Database>::Row, sqlx::Error>,
            >
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).fetch_one(query);
                Box::pin(async move {
                    let result = fut.await;
                    match &result {
                        Ok(_) => {
                            record_rows(&cx, 1);
                            finish(&cx, start, Some(1), &state.metrics, &metric_attrs);
                        }
                        Err(err) => {
                            record_error(&cx, err);
                            finish(&cx, start, None, &state.metrics, &metric_attrs);
                        }
                    }
                    result
                })
            }

            /// Execute the query and returns at most one row.
            fn fetch_optional<'e, 'q: 'e, E>(
                $self_,
                query: E,
            ) -> futures::future::BoxFuture<
                'e,
                Result<Option<<DB as sqlx::Database>::Row>, sqlx::Error>,
            >
            where
                E: 'q + sqlx::Execute<'q, DB>,
                'c: 'e,
            {
                let sql = query.sql().to_owned();
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(&sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).fetch_optional(query);
                Box::pin(async move {
                    let result = fut.await;
                    match &result {
                        Ok(maybe_row) => {
                            let count = u64::from(maybe_row.is_some());
                            record_rows(&cx, count);
                            finish(&cx, start, Some(count), &state.metrics, &metric_attrs);
                        }
                        Err(err) => {
                            record_error(&cx, err);
                            finish(&cx, start, None, &state.metrics, &metric_attrs);
                        }
                    }
                    result
                })
            }

            /// Prepare the SQL query to inspect the type information of its parameters
            /// and results.
            ///
            /// Be advised that when using the `query`, `query_as`, or `query_scalar` functions, the query
            /// is transparently prepared and executed.
            ///
            /// This explicit API is provided to allow access to the statement metadata available after
            /// it prepared but before the first row is returned.
            fn prepare<'e, 'q: 'e>(
                $self_,
                query: &'q str,
            ) -> futures::future::BoxFuture<
                'e,
                Result<<DB as sqlx::Database>::Statement<'q>, sqlx::Error>,
            >
            where
                'c: 'e,
            {
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(query), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).prepare(query);
                Box::pin(execute_instrumented(
                    fut, cx, start, state.metrics, metric_attrs,
                ))
            }

            /// Prepare the SQL query, with parameter type information, to inspect the
            /// type information about its parameters and results.
            ///
            /// Only some database drivers (Postgres, MSSQL) can take advantage of
            /// this extra information to influence parameter type inference.
            fn prepare_with<'e, 'q: 'e>(
                $self_,
                sql: &'q str,
                parameters: &'e [<DB as sqlx::Database>::TypeInfo],
            ) -> futures::future::BoxFuture<
                'e,
                Result<<DB as sqlx::Database>::Statement<'q>, sqlx::Error>,
            >
            where
                'c: 'e,
            {
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).prepare_with(sql, parameters);
                Box::pin(execute_instrumented(
                    fut, cx, start, state.metrics, metric_attrs,
                ))
            }

             /// Describe the SQL query and return type information about its parameters
             /// and results.
             ///
             /// This is used by compile-time verification in the query macros to
             /// power their type inference.
            #[doc(hidden)]
            fn describe<'e, 'q: 'e>(
                $self_,
                sql: &'q str,
            ) -> futures::future::BoxFuture<
                'e,
                Result<sqlx::Describe<DB>, sqlx::Error>,
            >
            where
                'c: 'e,
            {
                let state = $self_.state.clone();
                let name = attributes::span_name(state.attrs.system, None, None);
                let span_attrs = build_attributes(&state.attrs, Some(sql), None, None);
                let metric_attrs = state.attrs.base_key_values();
                let (cx, start) = start_span(&name, span_attrs);
                let fut = ($inner).describe(sql);
                Box::pin(execute_instrumented(
                    fut, cx, start, state.metrics, metric_attrs,
                ))
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Executor impls for each wrapper type
// ---------------------------------------------------------------------------

impl_executor!(&'_ crate::Pool<DB>, self => &self.inner);
impl_executor!(&'c mut crate::PoolConnection<DB>, self => self.inner.as_mut());
impl_executor!(&'c mut crate::Transaction<'_, DB>, self => &mut *self.inner);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::ConnectionAttributes;

    #[test]
    fn error_type_classification() {
        // Unit variants.
        assert_eq!(error_type(&sqlx::Error::RowNotFound), "RowNotFound");
        assert_eq!(error_type(&sqlx::Error::PoolTimedOut), "PoolTimedOut");
        assert_eq!(error_type(&sqlx::Error::PoolClosed), "PoolClosed");
        assert_eq!(error_type(&sqlx::Error::WorkerCrashed), "WorkerCrashed");

        // String / boxed-error variants.
        assert_eq!(
            error_type(&sqlx::Error::Configuration("bad".into())),
            "Configuration"
        );
        assert_eq!(
            error_type(&sqlx::Error::Io(std::io::Error::other("test"))),
            "Io"
        );
        assert_eq!(error_type(&sqlx::Error::Tls("tls".into())), "Tls");
        assert_eq!(
            error_type(&sqlx::Error::Protocol("proto".into())),
            "Protocol"
        );
        assert_eq!(error_type(&sqlx::Error::Decode("dec".into())), "Decode");
        assert_eq!(
            error_type(&sqlx::Error::AnyDriverError("any".into())),
            "AnyDriverError"
        );

        // Struct variants.
        assert_eq!(
            error_type(&sqlx::Error::ColumnNotFound("x".into())),
            "ColumnNotFound"
        );
        assert_eq!(
            error_type(&sqlx::Error::ColumnIndexOutOfBounds { index: 5, len: 3 }),
            "ColumnIndexOutOfBounds"
        );
        assert_eq!(
            error_type(&sqlx::Error::ColumnDecode {
                index: "0".into(),
                source: "bad".into(),
            }),
            "ColumnDecode"
        );
        assert_eq!(
            error_type(&sqlx::Error::TypeNotFound {
                type_name: "Foo".into(),
            }),
            "TypeNotFound"
        );

        // Migrate variant (behind sqlx's "migrate" default feature).
        assert_eq!(
            error_type(&sqlx::Error::Migrate(Box::new(
                sqlx::migrate::MigrateError::Execute(sqlx::Error::Protocol("test".into()))
            ))),
            "Migrate"
        );

        // The `_ => "Unknown"` branch covers future sqlx::Error variants that may be
        // added in newer sqlx releases. It cannot be tested directly since we cannot
        // construct an unknown variant, but it ensures forward compatibility.
    }

    fn test_attrs() -> ConnectionAttributes {
        ConnectionAttributes {
            system: "postgresql",
            host: Some("localhost".into()),
            port: Some(5432),
            namespace: Some("mydb".into()),
            network_peer_address: None,
            network_peer_port: None,
            query_text_mode: QueryTextMode::Full,
        }
    }

    #[test]
    fn build_attributes_with_full_query_text() {
        let attrs = test_attrs();
        let kv = build_attributes(&attrs, Some("SELECT 1"), None, None);
        let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
        assert!(keys.contains(&"db.query.text"));
    }

    #[test]
    fn build_attributes_with_off_query_text() {
        let mut attrs = test_attrs();
        attrs.query_text_mode = QueryTextMode::Off;
        let kv = build_attributes(&attrs, Some("SELECT 1"), None, None);
        let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
        assert!(!keys.contains(&"db.query.text"));
    }

    #[test]
    fn build_attributes_obfuscated_suppresses_query_text() {
        let mut attrs = test_attrs();
        attrs.query_text_mode = QueryTextMode::Obfuscated;
        let kv = build_attributes(&attrs, Some("SELECT secret"), None, None);
        let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
        assert!(!keys.contains(&"db.query.text"));
    }

    #[test]
    fn build_attributes_with_operation_and_collection() {
        let attrs = test_attrs();
        let kv = build_attributes(&attrs, None, Some("SELECT"), Some("users"));
        let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
        assert!(keys.contains(&"db.operation.name"));
        assert!(keys.contains(&"db.collection.name"));
    }

    #[test]
    fn build_attributes_no_sql_no_op() {
        let attrs = test_attrs();
        let kv = build_attributes(&attrs, None, None, None);
        let keys: Vec<&str> = kv.iter().map(|k| k.key.as_str()).collect();
        assert!(!keys.contains(&"db.query.text"));
        assert!(!keys.contains(&"db.operation.name"));
        assert!(!keys.contains(&"db.collection.name"));
        assert!(keys.contains(&"db.system.name"));
    }
}
