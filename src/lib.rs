//! Lightweight `SQLx` wrapper that emits OpenTelemetry-native spans and metrics
//! following the database client semantic conventions.

#[macro_use]
mod annotations;
pub(crate) mod attributes;
mod connection;
mod database;
mod executor;
mod metrics;
mod obfuscate;
mod pool;
mod pool_metrics;
mod query_ext;
mod runtime;
mod transaction;

pub use annotations::{Annotated, AnnotatedMut, QueryAnnotations};
pub use attributes::QueryTextMode;
pub use connection::PoolConnection;
pub use database::Database;
pub use pool::{Pool, PoolBuilder};
pub use query_ext::{AnnotatedQuery, QueryAnnotateExt};
pub use transaction::Transaction;
