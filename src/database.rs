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
}

#[cfg(feature = "postgres")]
impl Database for sqlx::Postgres {
    const SYSTEM: &'static str = "postgresql";

    fn connection_attributes(
        pool: &sqlx::Pool<Self>,
    ) -> (Option<String>, Option<u16>, Option<String>) {
        use sqlx::ConnectOptions;

        let url = pool.connect_options().to_url_lossy();
        let host = url.host_str().map(String::from);
        let port = url.port();
        let namespace = url
            .path_segments()
            .and_then(|mut segments| segments.next().map(String::from));
        (host, port, namespace)
    }
}
