use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry_semantic_conventions::metric as semconv_metric;

use crate::annotations::{Annotated, QueryAnnotations};
use crate::attributes::{ConnectionAttributes, QueryTextMode};
use crate::connection::PoolConnection;
use crate::database::Database;
use crate::metrics::Metrics;
use crate::transaction::Transaction;
use crate::transaction_metrics::TransactionMetrics;

/// Shared state propagated to every wrapper type derived from a pool.
#[derive(Debug, Clone)]
pub(crate) struct SharedState {
    pub attrs: Arc<ConnectionAttributes>,
    pub metrics: Arc<Metrics>,
}

/// Builder for constructing an instrumented [`Pool`] from a raw `sqlx::Pool`.
///
/// The builder auto-extracts connection attributes (host, port, database namespace) from
/// the underlying connect options via the [`Database`] trait, then lets you override any of
/// them before calling [`build`](Self::build). Settings on the wrapped `sqlx::Pool` itself
/// (max connections, idle timeout, etc.) should be applied to the `sqlx::Pool` *before*
/// passing it to the builder – `sqlx-otel` does not duplicate `SQLx`'s configuration
/// surface.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// use sqlx_otel::{PoolBuilder, QueryTextMode};
/// use std::time::Duration;
///
/// let raw = sqlx::SqlitePool::connect(":memory:").await?;
/// let pool = PoolBuilder::from(raw)
///     .with_database("my_db")
///     .with_query_text_mode(QueryTextMode::Obfuscated)
///     .with_pool_name("my-service-db")
///     .with_pool_metrics_interval(Duration::from_secs(5))
///     .build();
/// # let _ = pool;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct PoolBuilder<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
    host: Option<String>,
    port: Option<u16>,
    namespace: Option<String>,
    network_peer_address: Option<String>,
    network_peer_port: Option<u16>,
    network_protocol_name: Option<String>,
    network_transport: Option<String>,
    query_text_mode: QueryTextMode,
    pool_name: Option<String>,
    pool_metrics_interval: Duration,
}

impl<DB: Database> From<sqlx::Pool<DB>> for PoolBuilder<DB> {
    /// Create a builder from an existing `sqlx::Pool`, auto-extracting connection
    /// attributes from the backend's connect options. `network.protocol.name` is
    /// pre-populated from [`Database::DEFAULT_NETWORK_PROTOCOL_NAME`] (the wire protocol
    /// for Postgres / `MySQL`; absent for `SQLite`); override via
    /// [`with_network_protocol_name`](Self::with_network_protocol_name).
    fn from(pool: sqlx::Pool<DB>) -> Self {
        let (host, port, namespace) = DB::connection_attributes(&pool);
        Self {
            pool,
            host,
            port,
            namespace,
            network_peer_address: None,
            network_peer_port: None,
            network_protocol_name: DB::DEFAULT_NETWORK_PROTOCOL_NAME.map(String::from),
            network_transport: None,
            query_text_mode: QueryTextMode::default(),
            pool_name: None,
            pool_metrics_interval: Duration::from_secs(10),
        }
    }
}

impl<DB: Database> PoolBuilder<DB> {
    /// Override the `db.namespace` attribute (the database name).
    #[must_use]
    pub fn with_database(mut self, database: impl Into<String>) -> Self {
        self.namespace = Some(database.into());
        self
    }

    /// Override the `server.address` attribute (the logical hostname).
    #[must_use]
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Override the `server.port` attribute.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the `network.peer.address` attribute (the resolved IP address).
    #[must_use]
    pub fn with_network_peer_address(mut self, address: impl Into<String>) -> Self {
        self.network_peer_address = Some(address.into());
        self
    }

    /// Set the `network.peer.port` attribute (the resolved port).
    #[must_use]
    pub fn with_network_peer_port(mut self, port: u16) -> Self {
        self.network_peer_port = Some(port);
        self
    }

    /// Override the `network.protocol.name` attribute. Defaults to the backend's wire
    /// protocol via [`Database::DEFAULT_NETWORK_PROTOCOL_NAME`] (`"postgresql"` /
    /// `"mysql"`; absent for `SQLite`). Override when the connection is tunnelled through
    /// a different application-layer protocol or when reporting to a system that expects a
    /// specific name.
    #[must_use]
    pub fn with_network_protocol_name(mut self, name: impl Into<String>) -> Self {
        self.network_protocol_name = Some(name.into());
        self
    }

    /// Set the `network.transport` attribute (the OSI L4 transport: `"tcp"`, `"udp"`,
    /// `"pipe"`, `"unix"`, `"inproc"`). The wrapper does not infer transport from the
    /// connect string – callers who want this attribute on spans / metrics must set it
    /// explicitly so the value reflects the deployment configuration rather than a guess.
    #[must_use]
    pub fn with_network_transport(mut self, transport: impl Into<String>) -> Self {
        self.network_transport = Some(transport.into());
        self
    }

    /// Configure how `db.query.text` is captured on spans. Defaults to
    /// [`QueryTextMode::Full`].
    #[must_use]
    pub fn with_query_text_mode(mut self, mode: QueryTextMode) -> Self {
        self.query_text_mode = mode;
        self
    }

    /// Set the `db.client.connection.pool.name` attribute and enable the
    /// `db.client.connection.count` polling task.
    ///
    /// When a runtime feature (`runtime-tokio` or `runtime-async-std`) is also enabled, a
    /// background task is spawned that periodically records `db.client.connection.count`
    /// (idle / used). See [`with_pool_metrics_interval`](Self::with_pool_metrics_interval)
    /// to configure the polling frequency. The task is cancelled when the [`Pool`] (and
    /// every clone of it) is dropped.
    ///
    /// **Without a runtime feature, the name is recorded but no `connection.count` task is
    /// spawned and the gauge is never reported.** All other operation- and pool-level
    /// metrics still work in that configuration.
    #[must_use]
    pub fn with_pool_name(mut self, name: impl Into<String>) -> Self {
        self.pool_name = Some(name.into());
        self
    }

    /// Set the polling interval for `db.client.connection.count`. Defaults to 10 seconds.
    ///
    /// Has no effect unless [`with_pool_name`](Self::with_pool_name) is also called and a
    /// runtime feature is enabled.
    #[must_use]
    pub fn with_pool_metrics_interval(mut self, interval: Duration) -> Self {
        self.pool_metrics_interval = interval;
        self
    }

    /// Consume the builder and produce an instrumented [`Pool`].
    ///
    /// At this point the static pool gauges (`db.client.connection.max`,
    /// `db.client.connection.idle.max`, `db.client.connection.idle.min`) are recorded
    /// once with the connection-level attributes – they do not change over the pool's
    /// lifetime. The wait-time / use-time / timeout / pending-request instruments are
    /// created here and updated inline on every `acquire()` and connection drop. So are the
    /// abandoned-wait and transaction instruments, which have no series until first used.
    #[must_use]
    pub fn build(self) -> Pool<DB> {
        let metrics_shutdown = self.spawn_pool_metrics_task();

        let attrs = Arc::new(ConnectionAttributes {
            system: DB::SYSTEM,
            host: self.host,
            port: self.port,
            namespace: self.namespace,
            network_peer_address: self.network_peer_address,
            network_peer_port: self.network_peer_port,
            network_protocol_name: self.network_protocol_name,
            network_transport: self.network_transport,
            pool_name: self.pool_name,
            query_text_mode: self.query_text_mode,
        });
        let metrics = Arc::new(Metrics::new());
        let meter = opentelemetry::global::meter("sqlx-otel");

        // Record static pool configuration gauges once – these never change.
        let max_conns = i64::from(self.pool.options().get_max_connections());
        let min_conns = i64::from(self.pool.options().get_min_connections());
        let base_attrs = attrs.base_key_values();

        meter
            .i64_gauge(semconv_metric::DB_CLIENT_CONNECTION_MAX)
            .with_description("The maximum number of open connections allowed.")
            .build()
            .record(max_conns, &base_attrs);
        meter
            .i64_gauge(semconv_metric::DB_CLIENT_CONNECTION_IDLE_MAX)
            .with_description("The maximum number of idle open connections allowed.")
            .build()
            .record(max_conns, &base_attrs);
        meter
            .i64_gauge(semconv_metric::DB_CLIENT_CONNECTION_IDLE_MIN)
            .with_description("The minimum number of idle open connections allowed.")
            .build()
            .record(min_conns, &base_attrs);

        let pool = Pool {
            inner: self.pool,
            state: SharedState { attrs, metrics },
            metrics_shutdown,
            wait_time: Arc::new(
                meter
                    .f64_histogram(semconv_metric::DB_CLIENT_CONNECTION_WAIT_TIME)
                    .with_unit("s")
                    .with_description(
                        "The time it took to obtain an open connection from the pool.",
                    )
                    .with_boundaries(crate::metrics::LATENCY_BUCKETS_SECONDS.to_vec())
                    .build(),
            ),
            use_time: Arc::new(
                meter
                    .f64_histogram(semconv_metric::DB_CLIENT_CONNECTION_USE_TIME)
                    .with_unit("s")
                    .with_description(
                        "The time between borrowing a connection and returning it to the pool.",
                    )
                    .with_boundaries(crate::metrics::LATENCY_BUCKETS_SECONDS.to_vec())
                    .build(),
            ),
            timeouts: Arc::new(
                meter
                    .u64_counter(semconv_metric::DB_CLIENT_CONNECTION_TIMEOUTS)
                    .with_description(
                        "The number of connection pool acquire attempts that timed out.",
                    )
                    .build(),
            ),
            pending_requests: Arc::new(
                meter
                    .i64_up_down_counter(semconv_metric::DB_CLIENT_CONNECTION_PENDING_REQUESTS)
                    .with_description("The number of pending requests for an open connection.")
                    .build(),
            ),
            abandoned_wait: Arc::new(
                meter
                    .f64_histogram("sqlx_otel.connection.abandoned_wait.duration")
                    .with_unit("s")
                    .with_description(
                        "The time a caller waited for a connection before dropping the attempt.",
                    )
                    .with_boundaries(crate::metrics::LATENCY_BUCKETS_SECONDS.to_vec())
                    .build(),
            ),
            transactions: Arc::new(TransactionMetrics::new(&meter)),
        };
        pool.pending_requests.add(0, &base_attrs);
        pool.timeouts.add(0, &base_attrs);
        pool
    }

    /// Spawn the pool metrics background task if a pool name is set and a runtime is
    /// available. Returns the shutdown handle (or `None`).
    fn spawn_pool_metrics_task(&self) -> Option<crate::pool_metrics::ShutdownHandle> {
        let name = self.pool_name.as_ref()?;

        // Prefer tokio if both runtimes are enabled.
        #[cfg(feature = "runtime-tokio")]
        {
            Some(
                crate::pool_metrics::spawn::<crate::runtime::TokioRuntime, DB>(
                    self.pool.clone(),
                    name.clone(),
                    self.pool_metrics_interval,
                ),
            )
        }

        #[cfg(all(feature = "runtime-async-std", not(feature = "runtime-tokio")))]
        {
            Some(crate::pool_metrics::spawn::<
                crate::runtime::AsyncStdRuntime,
                DB,
            >(
                self.pool.clone(),
                name.clone(),
                self.pool_metrics_interval,
            ))
        }

        #[cfg(not(any(feature = "runtime-tokio", feature = "runtime-async-std")))]
        {
            let _ = name;
            None
        }
    }
}

/// An instrumented wrapper around `sqlx::Pool` that emits OpenTelemetry spans and metrics
/// for every database operation.
///
/// Create one via [`PoolBuilder`]. The wrapper is a drop-in replacement for `sqlx::Pool`:
/// `&Pool<DB>` implements [`sqlx::Executor`], so you can pass it straight into
/// `sqlx::query(...)`, `sqlx::query_as(...)`, and friends. Connections acquired via
/// [`acquire`](Self::acquire) and transactions started via [`begin`](Self::begin) inherit
/// the same instrumentation and produce spans / metrics with identical connection-level
/// attributes.
///
/// `Clone` is cheap – the inner `sqlx::Pool`, the connection-level attribute set, and the
/// metric instruments are all `Arc`-shared. Cloning never copies state; cloned pools share
/// the same underlying connection pool and metric stream.
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// use sqlx_otel::PoolBuilder;
///
/// let raw = sqlx::SqlitePool::connect(":memory:").await?;
/// let pool = PoolBuilder::from(raw).build();
///
/// // Pass `&pool` anywhere a `sqlx::Executor` is expected.
/// let row: (i64,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await?;
/// assert_eq!(row.0, 1);
/// # Ok(())
/// # }
/// ```
///
/// See also [`with_annotations`](Self::with_annotations) for per-query semantic-convention
/// attributes, and [`crate::QueryAnnotateExt`] for attaching annotations on the query side
/// instead of the executor side.
#[derive(Debug)]
pub struct Pool<DB: sqlx::Database> {
    pub(crate) inner: sqlx::Pool<DB>,
    pub(crate) state: SharedState,
    /// Dropping this handle signals the background polling task to stop.
    metrics_shutdown: Option<crate::pool_metrics::ShutdownHandle>,
    /// Histogram for `db.client.connection.wait_time`, recorded on each `acquire()`.
    wait_time: Arc<opentelemetry::metrics::Histogram<f64>>,
    /// Histogram for `db.client.connection.use_time`, recorded when a connection is dropped.
    pub(crate) use_time: Arc<opentelemetry::metrics::Histogram<f64>>,
    /// Counter for `db.client.connection.timeouts`, incremented on `PoolTimedOut`.
    timeouts: Arc<opentelemetry::metrics::Counter<u64>>,
    /// Up/down counter for `db.client.connection.pending_requests`, tracks callers
    /// currently waiting in `acquire()`.
    pending_requests: Arc<opentelemetry::metrics::UpDownCounter<i64>>,
    /// Histogram for `sqlx_otel.connection.abandoned_wait.duration`, recorded when an `acquire()`
    /// already waiting is dropped.
    abandoned_wait: Arc<opentelemetry::metrics::Histogram<f64>>,
    /// Instruments for the `sqlx_otel.transaction.*` family.
    transactions: Arc<TransactionMetrics>,
}

impl<DB: sqlx::Database> Clone for Pool<DB> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            state: self.state.clone(),
            metrics_shutdown: self.metrics_shutdown.clone(),
            wait_time: self.wait_time.clone(),
            use_time: self.use_time.clone(),
            timeouts: self.timeouts.clone(),
            pending_requests: self.pending_requests.clone(),
            abandoned_wait: self.abandoned_wait.clone(),
            transactions: self.transactions.clone(),
        }
    }
}

impl<DB: Database> Pool<DB> {
    /// Acquire a pooled connection instrumented for OpenTelemetry.
    ///
    /// Records `db.client.connection.wait_time` (time spent waiting for a connection),
    /// tracks `db.client.connection.pending_requests` while the call is in flight, and
    /// increments `db.client.connection.timeouts` on `sqlx::Error::PoolTimedOut`. The
    /// returned [`PoolConnection`] records `db.client.connection.use_time` when dropped
    /// and is itself an [`sqlx::Executor`] via `&mut conn`.
    ///
    /// Dropping this future while it waits records the time already waited once on
    /// `sqlx_otel.connection.abandoned_wait.duration`, not as a completed wait or a timeout. A
    /// future that was never polled never waited and records nothing. The active
    /// [`PoolAttribution`](crate::PoolAttribution) is captured on first poll and used for every
    /// measurement of this attempt and its lease.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if a connection cannot be obtained from the pool – typically
    /// `PoolTimedOut` when the configured acquire timeout elapses, or `PoolClosed` after
    /// [`close`](Self::close).
    pub async fn acquire(&self) -> Result<PoolConnection<DB>, sqlx::Error> {
        let pool_attrs = self.state.attrs.base_key_values();
        let attrs = crate::attribution::lease_attributes(&pool_attrs);
        // Initialize the workload series like the pool-wide series. A rate can include the
        // first timeout in a cumulative-sample rate only if the zero was exported first.
        self.timeouts.add(0, &attrs);
        self.pending_requests.add(1, &pool_attrs);
        let pending = PendingAcquisition {
            counter: &self.pending_requests,
            pool_attrs: &pool_attrs,
            abandoned_wait: &self.abandoned_wait,
            lease_attrs: &attrs,
            started_at: Instant::now(),
            completed: false,
        };
        let result = self.inner.acquire().await;
        let waited = pending.complete();
        self.wait_time.record(waited.as_secs_f64(), &attrs);

        if let Err(sqlx::Error::PoolTimedOut) = &result {
            self.timeouts.add(1, &attrs);
        }

        result.map(|inner| PoolConnection {
            inner,
            state: self.state.clone(),
            usage: crate::connection::ConnectionUsage {
                use_time: self.use_time.clone(),
                acquired_at: Instant::now(),
                lease_attrs: attrs,
            },
        })
    }

    /// Begin a new transaction instrumented for OpenTelemetry.
    ///
    /// The returned [`Transaction`] implements `sqlx::Executor` via `&mut tx` and emits
    /// the same per-operation spans and metrics as the pool itself. Call
    /// [`commit`](Transaction::commit) or [`rollback`](Transaction::rollback) to terminate
    /// it; dropping the value without doing either rolls back implicitly (per `SQLx`'s
    /// usual behaviour).
    ///
    /// Records one `sqlx_otel.transaction.begin.duration` sample once a connection is held,
    /// including when `BEGIN` fails or this future is dropped during it. Only a successful `BEGIN`
    /// starts `sqlx_otel.transaction.duration`; see [`Transaction`] for its outcomes.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if `BEGIN` fails – typically due to a connection problem or
    /// because the underlying connection cannot start a new transaction.
    pub async fn begin(&self) -> Result<Transaction<'static, DB>, sqlx::Error> {
        let PoolConnection {
            inner,
            state,
            usage,
        } = self.acquire().await?;
        let (inner, lifecycle) = self
            .transactions
            .observe_begin(&usage.lease_attrs, sqlx::Transaction::begin(inner, None))
            .await?;
        Ok(Transaction {
            inner,
            state,
            usage,
            lifecycle,
        })
    }

    /// Shut down the pool, waiting for all connections to be released.
    pub async fn close(&self) {
        self.inner.close().await;
    }

    /// Returns `true` if the pool has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Return an annotated executor that attaches per-query semantic-convention attributes
    /// (`db.operation.name`, `db.collection.name`, `db.query.summary`,
    /// `db.stored_procedure.name`) to every span created by the next operation.
    ///
    /// The returned wrapper borrows the pool and implements `sqlx::Executor`. Use the
    /// query-side equivalent ([`crate::QueryAnnotateExt`]) when the annotation belongs
    /// next to the query text rather than next to the executor.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "sqlite")]
    /// # async fn _doc() -> Result<(), sqlx::Error> {
    /// # use sqlx_otel::PoolBuilder;
    /// use sqlx::Executor as _;
    /// use sqlx_otel::QueryAnnotations;
    /// # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
    ///
    /// pool.with_annotations(
    ///     QueryAnnotations::new()
    ///         .operation("SELECT")
    ///         .collection("users"),
    /// )
    /// .fetch_all("SELECT * FROM users")
    /// .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_annotations(&self, annotations: QueryAnnotations) -> Annotated<'_, Self> {
        Annotated {
            inner: self,
            annotations,
            state: self.state.clone(),
        }
    }

    /// Shorthand for annotating the next operation with `db.operation.name` and
    /// `db.collection.name`.
    ///
    /// Equivalent to `self.with_annotations(QueryAnnotations::new().operation(op).collection(coll))`.
    #[must_use]
    pub fn with_operation(
        &self,
        operation: impl Into<String>,
        collection: impl Into<String>,
    ) -> Annotated<'_, Self> {
        self.with_annotations(
            QueryAnnotations::new()
                .operation(operation)
                .collection(collection),
        )
    }
}

/// Decrements `db.client.connection.pending_requests` on every exit path from
/// [`Pool::acquire`], and measures the attempts that never got an answer.
///
/// A caller timeout or task abort can drop the `acquire()` future while it is still awaiting
/// `SQLx`, which would otherwise leave the counter permanently incremented. Tying the decrement
/// to a `Drop` guard covers success, error, and cancellation alike. Cancellation records no
/// completed wait and is not classified as `PoolTimedOut`, because the pool never failed to
/// deliver – the caller left. It records the elapsed wait on its own instrument instead, exactly
/// once, because the guard exists only after a first poll and is dropped only once.
struct PendingAcquisition<'a> {
    counter: &'a opentelemetry::metrics::UpDownCounter<i64>,
    pool_attrs: &'a [opentelemetry::KeyValue],
    abandoned_wait: &'a opentelemetry::metrics::Histogram<f64>,
    lease_attrs: &'a [opentelemetry::KeyValue],
    started_at: Instant,
    completed: bool,
}

impl PendingAcquisition<'_> {
    /// `SQLx` answered, so this attempt was not abandoned. Returns how long it took.
    fn complete(mut self) -> Duration {
        self.completed = true;
        self.started_at.elapsed()
    }
}

impl Drop for PendingAcquisition<'_> {
    fn drop(&mut self) {
        self.counter.add(-1, self.pool_attrs);
        if !self.completed {
            self.abandoned_wait
                .record(self.started_at.elapsed().as_secs_f64(), self.lease_attrs);
        }
    }
}
