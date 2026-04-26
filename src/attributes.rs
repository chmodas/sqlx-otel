use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute;

/// Controls whether and how `db.query.text` is captured on spans.
///
/// # Automatic parameter capture
///
/// `SQLx` uses parameterised queries (`$1`, `?`) by default, so `Full` mode is safe for
/// most use cases. If you build SQL via `format!` with interpolated values, use
/// `Obfuscated` or `Off` to avoid capturing sensitive data.
///
/// Note that `db.query.parameter.<key>` capture is not supported automatically – `SQLx`'s
/// `Execute` trait does not expose bind parameter values. Users who need per-parameter
/// attributes can add them manually via the OpenTelemetry span context API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryTextMode {
    /// Capture the parameterised query text as-is. This is the default because `SQLx`
    /// queries use bind parameters (`$1`, `?`), so literal values are not present in the
    /// query string.
    #[default]
    Full,
    /// Replace literal values in the query text with placeholders. Useful when queries are
    /// built via string interpolation rather than bind parameters.
    Obfuscated,
    /// Do not capture `db.query.text` at all.
    Off,
}

/// Immutable, connection-level OpenTelemetry attributes shared by every span and metric
/// recording from a single pool.
///
/// Built once by [`PoolBuilder`](crate::PoolBuilder) and wrapped in `Arc` so that every
/// wrapper type (`Pool`, `PoolConnection`, `Transaction`, `Connection`) can reference the
/// same allocation.
#[derive(Debug, Clone)]
pub(crate) struct ConnectionAttributes {
    /// `db.system.name` – always present.
    pub system: &'static str,
    /// `server.address` – the logical hostname. May be `None` for embedded databases.
    pub host: Option<String>,
    /// `server.port`.
    pub port: Option<u16>,
    /// `db.namespace` – the database name.
    pub namespace: Option<String>,
    /// `network.peer.address` – the resolved IP address, user-provided.
    pub network_peer_address: Option<String>,
    /// `network.peer.port` – the resolved port, user-provided.
    pub network_peer_port: Option<u16>,
    /// Controls `db.query.text` capture.
    pub query_text_mode: QueryTextMode,
}

impl ConnectionAttributes {
    /// Produce the base `KeyValue` set for span and metric attribute lists. Only includes
    /// attributes that have a value – optional fields are omitted when `None`.
    pub fn base_key_values(&self) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(6);
        attrs.push(KeyValue::new(attribute::DB_SYSTEM_NAME, self.system));
        if let Some(ref host) = self.host {
            attrs.push(KeyValue::new(attribute::SERVER_ADDRESS, host.clone()));
        }
        if let Some(port) = self.port {
            attrs.push(KeyValue::new(attribute::SERVER_PORT, i64::from(port)));
        }
        if let Some(ref ns) = self.namespace {
            attrs.push(KeyValue::new(attribute::DB_NAMESPACE, ns.clone()));
        }
        if let Some(ref addr) = self.network_peer_address {
            attrs.push(KeyValue::new(attribute::NETWORK_PEER_ADDRESS, addr.clone()));
        }
        if let Some(port) = self.network_peer_port {
            attrs.push(KeyValue::new(attribute::NETWORK_PEER_PORT, i64::from(port)));
        }
        attrs
    }
}

/// Build a span name following the database client semconv hierarchy:
///
/// 1. `db.query.summary` when provided (wins unconditionally – this is the spec's
///    designated slot for callers who cannot guarantee a low-cardinality
///    `db.operation.name`).
/// 2. `"{db.operation.name} {db.collection.name}"` when both are provided.
/// 3. `"{db.operation.name}"` when only the operation is known.
/// 4. `"{db.system.name}"` as the final fallback.
pub(crate) fn span_name(
    system: &str,
    operation: Option<&str>,
    collection: Option<&str>,
    summary: Option<&str>,
) -> String {
    if let Some(s) = summary {
        return s.to_owned();
    }
    match (operation, collection) {
        (Some(op), Some(coll)) => format!("{op} {coll}"),
        (Some(op), None) => op.to_owned(),
        _ => system.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_name_with_operation_and_collection() {
        assert_eq!(
            span_name("postgresql", Some("SELECT"), Some("users"), None),
            "SELECT users"
        );
    }

    #[test]
    fn span_name_with_operation_only() {
        assert_eq!(
            span_name("postgresql", Some("SELECT"), None, None),
            "SELECT"
        );
    }

    #[test]
    fn span_name_fallback_to_system() {
        assert_eq!(span_name("sqlite", None, None, None), "sqlite");
    }

    #[test]
    fn span_name_collection_without_operation_falls_back() {
        assert_eq!(span_name("mysql", None, Some("orders"), None), "mysql");
    }

    #[test]
    fn span_name_summary_wins_over_operation_and_collection() {
        assert_eq!(
            span_name(
                "postgresql",
                Some("SELECT"),
                Some("users"),
                Some("daily report")
            ),
            "daily report"
        );
    }

    #[test]
    fn span_name_summary_alone() {
        assert_eq!(
            span_name("sqlite", None, None, Some("custom name")),
            "custom name"
        );
    }

    #[test]
    fn base_key_values_all_fields() {
        let attrs = ConnectionAttributes {
            system: "postgresql",
            host: Some("localhost".into()),
            port: Some(5432),
            namespace: Some("mydb".into()),
            network_peer_address: Some("127.0.0.1".into()),
            network_peer_port: Some(5432),
            query_text_mode: QueryTextMode::Full,
        };
        let kvs = attrs.base_key_values();
        assert_eq!(kvs.len(), 6);
        assert_eq!(kvs[0].key.as_str(), "db.system.name");
        assert_eq!(kvs[1].key.as_str(), "server.address");
        assert_eq!(kvs[2].key.as_str(), "server.port");
        assert_eq!(kvs[3].key.as_str(), "db.namespace");
        assert_eq!(kvs[4].key.as_str(), "network.peer.address");
        assert_eq!(kvs[5].key.as_str(), "network.peer.port");
    }

    #[test]
    fn base_key_values_minimal() {
        let attrs = ConnectionAttributes {
            system: "sqlite",
            host: None,
            port: None,
            namespace: None,
            network_peer_address: None,
            network_peer_port: None,
            query_text_mode: QueryTextMode::Off,
        };
        let kvs = attrs.base_key_values();
        assert_eq!(kvs.len(), 1);
        assert_eq!(kvs[0].key.as_str(), "db.system.name");
    }
}
