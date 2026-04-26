/// Per-backend contract providing the database system name and a method to extract
/// connection-level attributes from the backend's connect options.
///
/// Each supported `SQLx` backend (Postgres, Sqlite, Mysql) implements this trait behind its
/// corresponding feature flag. The trait is intentionally minimal – it exists solely to
/// let the generic wrapper types resolve connection attributes once at pool construction
/// time.
pub trait Database: sqlx::Database {
    /// The OpenTelemetry `db.system.name` value for this backend (e.g. `"postgresql"`,
    /// `"sqlite"`, `"mysql"`).
    const SYSTEM: &'static str;

    /// Extract host, port, and database namespace from the backend's connect options.
    ///
    /// Returns `(host, port, namespace)` where any component may be `None` if the backend
    /// does not support it (e.g. Sqlite has no host or port).
    fn connection_attributes(
        pool: &sqlx::Pool<Self>,
    ) -> (Option<String>, Option<u16>, Option<String>);

    /// Extract the number of rows affected from a `QueryResult`.
    ///
    /// Each `SQLx` backend defines its own `QueryResult` type with an inherent
    /// `rows_affected()` method. This trait method provides a uniform interface for the
    /// instrumentation layer.
    fn rows_affected(result: &<Self as sqlx::Database>::QueryResult) -> u64;
}

/// Extract `(host, port, namespace)` from a network-style backend's connect options by
/// rendering them to a URL and parsing the components.
///
/// Used by the Postgres and `MySQL` impls of [`Database::connection_attributes`] – both
/// share the same URL-based extraction logic. `SQLite` supplies a filename instead and
/// does not need this helper.
#[cfg(any(feature = "postgres", feature = "mysql"))]
fn url_based_connection_attributes<O: sqlx::ConnectOptions>(
    options: &O,
) -> (Option<String>, Option<u16>, Option<String>) {
    let url = options.to_url_lossy();
    let host = url.host_str().map(String::from);
    let port = url.port();
    let namespace = url
        .path_segments()
        .and_then(|mut segments| segments.next().map(String::from));
    (host, port, namespace)
}

#[cfg(feature = "sqlite")]
impl Database for sqlx::Sqlite {
    const SYSTEM: &'static str = "sqlite";

    fn connection_attributes(
        pool: &sqlx::Pool<Self>,
    ) -> (Option<String>, Option<u16>, Option<String>) {
        let namespace = pool
            .connect_options()
            .get_filename()
            .to_str()
            .map(String::from);
        (None, None, namespace)
    }

    fn rows_affected(result: &sqlx::sqlite::SqliteQueryResult) -> u64 {
        result.rows_affected()
    }
}

#[cfg(feature = "postgres")]
impl Database for sqlx::Postgres {
    const SYSTEM: &'static str = "postgresql";

    fn connection_attributes(
        pool: &sqlx::Pool<Self>,
    ) -> (Option<String>, Option<u16>, Option<String>) {
        url_based_connection_attributes(pool.connect_options().as_ref())
    }

    fn rows_affected(result: &sqlx::postgres::PgQueryResult) -> u64 {
        result.rows_affected()
    }
}

#[cfg(feature = "mysql")]
impl Database for sqlx::MySql {
    const SYSTEM: &'static str = "mysql";

    fn connection_attributes(
        pool: &sqlx::Pool<Self>,
    ) -> (Option<String>, Option<u16>, Option<String>) {
        url_based_connection_attributes(pool.connect_options().as_ref())
    }

    fn rows_affected(result: &sqlx::mysql::MySqlQueryResult) -> u64 {
        result.rows_affected()
    }
}
