use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute;

/// Controls whether and how `db.query.text` is captured on spans.
///
/// Configured via [`PoolBuilder::with_query_text_mode`](crate::PoolBuilder::with_query_text_mode).
///
/// # Whitespace normalisation
///
/// For both [`Full`](Self::Full) and [`Obfuscated`](Self::Obfuscated) the emitted text has
/// inter-token whitespace runs collapsed to a single ASCII space and leading/trailing
/// whitespace trimmed. Whitespace **inside** string literals, quoted identifiers,
/// dollar-quoted bodies, and comments is preserved verbatim. Multi-line SQL written across
/// several Rust source lines therefore renders as a single readable line in `OTel` exports
/// without the embedded `\n` and indentation runs that come from source-level formatting.
/// [`Off`](Self::Off) is unaffected (no attribute is emitted).
///
/// # When to choose what
///
/// - **[`Full`](Self::Full)** (default) – appropriate when all SQL flows through `SQLx`
///   bind parameters. The captured text contains placeholders (`$1`, `?`), not literal
///   values, so user data does not leak into the span.
/// - **[`Obfuscated`](Self::Obfuscated)** – appropriate when SQL is built via string
///   interpolation (`format!`, query concatenation, dynamic identifiers) and may contain
///   literal values. Structure is preserved; literals (string, numeric, hex, boolean, and
///   `PostgreSQL` dollar-quoted) are replaced with `?`. Comments, identifiers (quoted or
///   otherwise), operators, and `NULL` are kept verbatim.
/// - **[`Off`](Self::Off)** – appropriate when the query text is itself sensitive
///   (proprietary schemas, query shapes that reveal business logic) or when query-text
///   cardinality must be eliminated entirely.
///
/// `db.query.parameter.<key>` capture is **not supported** – `SQLx`'s `Execute` trait does
/// not expose bind values, and reverse-engineering them from the encoded buffer would tie
/// the wrapper to driver internals. Callers who need per-parameter attributes can add
/// them manually via the active span using the OpenTelemetry API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryTextMode {
    /// Capture the parameterised query text. This is the default because `SQLx` queries
    /// use bind parameters (`$1`, `?`), so literal values are not present in the query
    /// string. Inter-token whitespace is collapsed to a single space and leading/trailing
    /// whitespace is trimmed; whitespace inside literals, identifiers, and comments is
    /// preserved verbatim.
    #[default]
    Full,
    /// Replace literal values in the query text with `?`. Useful when queries are built
    /// via string interpolation rather than bind parameters. The same whitespace
    /// normalisation as [`Full`](Self::Full) is applied after redaction.
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
    /// `server.address` – the logical hostname (it may be `None` for embedded databases).
    pub host: Option<String>,
    /// `server.port`.
    pub port: Option<u16>,
    /// `db.namespace` – the database name.
    pub namespace: Option<String>,
    /// `network.peer.address` – the resolved IP address, user-provided.
    pub network_peer_address: Option<String>,
    /// `network.peer.port` – the resolved port, user-provided.
    pub network_peer_port: Option<u16>,
    /// `network.protocol.name` – the OSI L7 wire protocol (e.g. `"postgresql"`, `"mysql"`).
    /// `None` for embedded backends that do not speak a wire protocol (e.g. `SQLite`).
    pub network_protocol_name: Option<String>,
    /// `network.transport` – the OSI L4 transport (`"tcp"`, `"udp"`, `"pipe"`, `"unix"`,
    /// `"inproc"`). User-provided via [`PoolBuilder::with_network_transport`](
    /// crate::PoolBuilder::with_network_transport); the wrapper does not infer it from the
    /// connect string.
    pub network_transport: Option<String>,
    /// `db.client.connection.pool.name` – user-provided pool identifier shared with the
    /// `db.client.connection.*` metric family. Surfaces on every span and per-operation
    /// metric so dashboards can correlate query latency with pool-level signals.
    pub pool_name: Option<String>,
    /// Controls `db.query.text` capture.
    pub query_text_mode: QueryTextMode,
}

impl ConnectionAttributes {
    /// Produce the base `KeyValue` set for span and metric attribute lists. Only includes
    /// attributes that have a value – optional fields are omitted when `None`.
    pub fn base_key_values(&self) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(9);
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
        if let Some(ref proto) = self.network_protocol_name {
            attrs.push(KeyValue::new(
                attribute::NETWORK_PROTOCOL_NAME,
                proto.clone(),
            ));
        }
        if let Some(ref transport) = self.network_transport {
            attrs.push(KeyValue::new(
                attribute::NETWORK_TRANSPORT,
                transport.clone(),
            ));
        }
        if let Some(ref name) = self.pool_name {
            attrs.push(KeyValue::new(
                attribute::DB_CLIENT_CONNECTION_POOL_NAME,
                name.clone(),
            ));
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
///
/// Empty-string inputs are treated as if absent: `Some("")` falls through to the next
/// branch in the hierarchy. This avoids emitting empty span names – which several
/// `OpenTelemetry` backends render as `<unnamed>` or treat as malformed – when a caller
/// passes a vacuous annotation value.
pub(crate) fn span_name(
    system: &str,
    operation: Option<&str>,
    collection: Option<&str>,
    summary: Option<&str>,
) -> String {
    fn nonempty(o: Option<&str>) -> Option<&str> {
        o.filter(|s| !s.is_empty())
    }
    if let Some(s) = nonempty(summary) {
        return s.to_owned();
    }
    match (nonempty(operation), nonempty(collection)) {
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

    /// Regression: `span_name("a", Some(""), None, None)` previously returned `""`. The
    /// minimal failing input was discovered by `span_name_is_non_empty` and shrunk by
    /// proptest. Pinning it here so a future change cannot reintroduce the empty span
    /// name.
    #[test]
    fn span_name_empty_operation_falls_through_to_system() {
        assert_eq!(span_name("sqlite", Some(""), None, None), "sqlite");
    }

    /// Empty `summary` does not win over the rest of the hierarchy: it is treated as
    /// missing so the `(op, coll)` synthesis still fires.
    #[test]
    fn span_name_empty_summary_falls_through() {
        assert_eq!(
            span_name("sqlite", Some("SELECT"), Some("users"), Some("")),
            "SELECT users"
        );
    }

    /// Empty `op` and empty `coll` together fall through to the bare-system branch.
    #[test]
    fn span_name_empty_op_and_coll_falls_through_to_system() {
        assert_eq!(span_name("sqlite", Some(""), Some(""), None), "sqlite");
    }

    /// Empty `op` with non-empty `coll` still falls through, because the hierarchy
    /// requires an operation before a collection contributes.
    #[test]
    fn span_name_empty_op_with_coll_falls_through_to_system() {
        assert_eq!(span_name("sqlite", Some(""), Some("users"), None), "sqlite");
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
            network_protocol_name: Some("postgresql".into()),
            network_transport: Some("tcp".into()),
            pool_name: Some("primary".into()),
            query_text_mode: QueryTextMode::Full,
        };
        let kvs = attrs.base_key_values();
        assert_eq!(kvs.len(), 9);
        assert_eq!(kvs[0].key.as_str(), "db.system.name");
        assert_eq!(kvs[1].key.as_str(), "server.address");
        assert_eq!(kvs[2].key.as_str(), "server.port");
        assert_eq!(kvs[3].key.as_str(), "db.namespace");
        assert_eq!(kvs[4].key.as_str(), "network.peer.address");
        assert_eq!(kvs[5].key.as_str(), "network.peer.port");
        assert_eq!(kvs[6].key.as_str(), "network.protocol.name");
        assert_eq!(kvs[7].key.as_str(), "network.transport");
        assert_eq!(kvs[8].key.as_str(), "db.client.connection.pool.name");
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
            network_protocol_name: None,
            network_transport: None,
            pool_name: None,
            query_text_mode: QueryTextMode::Off,
        };
        let kvs = attrs.base_key_values();
        assert_eq!(kvs.len(), 1);
        assert_eq!(kvs[0].key.as_str(), "db.system.name");
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// `span_name` is total: every combination of `(system, op, coll, summary)`
        /// yields a non-empty `String` provided `system` itself is non-empty. Empty
        /// optional values (`Some("")`) fall through to the next branch in the
        /// hierarchy, so the bare-system fallback always produces non-empty output.
        #[test]
        fn span_name_is_non_empty(
            system in "[a-z]{1,16}",
            op in proptest::option::of(".{0,64}"),
            coll in proptest::option::of(".{0,64}"),
            summary in proptest::option::of(".{0,64}"),
        ) {
            let name = span_name(&system, op.as_deref(), coll.as_deref(), summary.as_deref());
            prop_assert!(!name.is_empty());
        }

        /// When `summary` is `Some(s)` with `s` non-empty, the output equals `s`
        /// exactly: the summary branch wins unconditionally over the `(op, coll)`
        /// synthesis. Empty summaries fall through and are covered by the dedicated
        /// example test.
        #[test]
        fn span_name_summary_wins(
            system in ".{0,16}",
            op in proptest::option::of(".{0,64}"),
            coll in proptest::option::of(".{0,64}"),
            summary in ".{1,64}",
        ) {
            let name = span_name(&system, op.as_deref(), coll.as_deref(), Some(summary.as_str()));
            prop_assert_eq!(name, summary);
        }

        /// When `summary` is `None` and both `op` and `coll` are `Some` with non-empty
        /// values, the output is `"{op} {coll}"` exactly. Empty op/coll combinations
        /// fall through and are covered by dedicated example tests.
        #[test]
        fn span_name_op_coll_synthesis(
            system in ".{0,16}",
            op in ".{1,64}",
            coll in ".{1,64}",
        ) {
            let name = span_name(&system, Some(&op), Some(&coll), None);
            prop_assert_eq!(name, format!("{op} {coll}"));
        }

        /// When all of `op`, `coll`, and `summary` are `None`, the output equals
        /// `system` exactly.
        #[test]
        fn span_name_bare_system_fallback(system in ".{0,16}") {
            let name = span_name(&system, None, None, None);
            prop_assert_eq!(name, system);
        }

        /// Setting only `coll` without `op` falls through to the bare-system branch:
        /// the spec hierarchy requires an operation before a collection contributes
        /// to the span name.
        #[test]
        fn span_name_collection_alone_is_ignored(
            system in ".{0,16}",
            coll in ".{0,64}",
        ) {
            let name = span_name(&system, None, Some(&coll), None);
            prop_assert_eq!(name, system);
        }

        /// `span_name` does not panic on any combination of arbitrary unicode, including
        /// null bytes, multi-byte sequences, and combining characters.
        #[test]
        fn span_name_no_panic(
            system in any::<String>(),
            op in proptest::option::of(any::<String>()),
            coll in proptest::option::of(any::<String>()),
            summary in proptest::option::of(any::<String>()),
        ) {
            let _ = span_name(&system, op.as_deref(), coll.as_deref(), summary.as_deref());
        }

        /// `base_key_values` emits `1 + n` entries where `n` is the count of populated
        /// optional fields. `db.system.name` is always present, the others appear iff
        /// their corresponding field is `Some`.
        #[test]
        fn base_key_values_length_matches_populated_fields(
            host in proptest::option::of("[a-z]{1,16}"),
            port in proptest::option::of(any::<u16>()),
            namespace in proptest::option::of("[a-z]{1,16}"),
            network_peer_address in proptest::option::of("[0-9.:]{1,32}"),
            network_peer_port in proptest::option::of(any::<u16>()),
            network_protocol_name in proptest::option::of("[a-z]{1,16}"),
            network_transport in proptest::option::of("[a-z]{1,8}"),
            pool_name in proptest::option::of("[a-z0-9-]{1,32}"),
        ) {
            let attrs = ConnectionAttributes {
                system: "sqlite",
                host: host.clone(),
                port,
                namespace: namespace.clone(),
                network_peer_address: network_peer_address.clone(),
                network_peer_port,
                network_protocol_name: network_protocol_name.clone(),
                network_transport: network_transport.clone(),
                pool_name: pool_name.clone(),
                query_text_mode: QueryTextMode::Off,
            };
            let kvs = attrs.base_key_values();
            let expected = 1
                + usize::from(host.is_some())
                + usize::from(port.is_some())
                + usize::from(namespace.is_some())
                + usize::from(network_peer_address.is_some())
                + usize::from(network_peer_port.is_some())
                + usize::from(network_protocol_name.is_some())
                + usize::from(network_transport.is_some())
                + usize::from(pool_name.is_some());
            prop_assert_eq!(kvs.len(), expected);
            prop_assert_eq!(kvs[0].key.as_str(), "db.system.name");

            let keys: Vec<&str> = kvs.iter().map(|k| k.key.as_str()).collect();
            prop_assert_eq!(
                keys.contains(&"network.protocol.name"),
                network_protocol_name.is_some(),
            );
            prop_assert_eq!(
                keys.contains(&"network.transport"),
                network_transport.is_some(),
            );
            prop_assert_eq!(
                keys.contains(&"db.client.connection.pool.name"),
                pool_name.is_some(),
            );
        }
    }
}
