use std::sync::Arc;

use crate::attributes::{ConnectionAttributes, QueryTextMode};
use crate::connection::PoolConnection;
use crate::database::Database;
use crate::metrics::Metrics;
use crate::transaction::Transaction;

/// Shared state propagated to every wrapper type derived from a pool.
#[derive(Debug, Clone)]
pub(crate) struct SharedState {
    pub attrs: Arc<ConnectionAttributes>,
    pub metrics: Arc<Metrics>,
}

/// Builder for constructing an instrumented [`Pool`] from a raw `sqlx::Pool`.
///
/// The builder auto-extracts connection attributes (host, port, namespace) from the
/// pool's connect options via the [`Database`] trait, then allows overriding any of them
/// before calling [`build()`](Self::build).
///
/// # Example
///
/// ```ignore
/// let pool = PoolBuilder::from(sqlx_pool)
///     .with_database("my_db")
///     .build();
/// ```
#[derive(Debug)]
pub struct PoolBuilder<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
    host: Option<String>,
    port: Option<u16>,
    namespace: Option<String>,
    network_peer_address: Option<String>,
    network_peer_port: Option<u16>,
    query_text_mode: QueryTextMode,
}

impl<DB: Database> From<sqlx::Pool<DB>> for PoolBuilder<DB> {
    /// Create a builder from an existing `sqlx::Pool`, auto-extracting connection
    /// attributes from the backend's connect options.
    fn from(pool: sqlx::Pool<DB>) -> Self {
        let (host, port, namespace) = DB::connection_attributes(&pool);
        Self {
            pool,
            host,
            port,
            namespace,
            network_peer_address: None,
            network_peer_port: None,
            query_text_mode: QueryTextMode::default(),
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

    /// Configure how `db.query.text` is captured on spans. Defaults to
    /// [`QueryTextMode::Full`].
    #[must_use]
    pub fn with_query_text_mode(mut self, mode: QueryTextMode) -> Self {
        self.query_text_mode = mode;
        self
    }

    /// Consume the builder and produce an instrumented [`Pool`].
    #[must_use]
    pub fn build(self) -> Pool<DB> {
        let attrs = Arc::new(ConnectionAttributes {
            system: DB::SYSTEM,
            host: self.host,
            port: self.port,
            namespace: self.namespace,
            network_peer_address: self.network_peer_address,
            network_peer_port: self.network_peer_port,
            query_text_mode: self.query_text_mode,
        });
        let metrics = Arc::new(Metrics::new());
        Pool {
            inner: self.pool,
            state: SharedState { attrs, metrics },
        }
    }
}

/// An instrumented wrapper around `sqlx::Pool` that emits OpenTelemetry spans and metrics
/// for every database operation.
///
/// Create one via [`PoolBuilder`]:
///
/// ```ignore
/// let pool: Pool<Postgres> = PoolBuilder::from(sqlx_pool).build();
/// ```
///
/// All connections acquired from this pool inherit its shared attributes and metric
/// instruments.
#[derive(Clone, Debug)]
pub struct Pool<DB: sqlx::Database> {
    pub(crate) inner: sqlx::Pool<DB>,
    pub(crate) state: SharedState,
}

impl<DB: Database> Pool<DB> {
    /// Acquire a pooled connection instrumented for OpenTelemetry.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if a connection cannot be obtained from the pool (e.g.
    /// timeout, pool closed).
    pub async fn acquire(&self) -> Result<PoolConnection<DB>, sqlx::Error> {
        self.inner.acquire().await.map(|inner| PoolConnection {
            inner,
            state: self.state.clone(),
        })
    }

    /// Begin a new transaction instrumented for OpenTelemetry.
    ///
    /// # Errors
    ///
    /// Returns `sqlx::Error` if beginning the transaction fails.
    pub async fn begin(&self) -> Result<Transaction<'_, DB>, sqlx::Error> {
        self.inner.begin().await.map(|inner| Transaction {
            inner,
            state: self.state.clone(),
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
}
