/// Per-backend contract providing the database system name, connect-attribute extraction,
/// and `rows_affected` projection.
///
/// Implemented by this crate for [`sqlx::Sqlite`], [`sqlx::Postgres`], and [`sqlx::MySql`]
/// behind their respective feature flags. The trait exists so the generic wrapper types
/// (`Pool`, `PoolConnection`, `Transaction`) can resolve connection attributes once at
/// pool construction and project `rows_affected` from the per-backend `QueryResult` types.
///
/// **The trait is sealed.** It is an internal generic-dispatch contract, not an extension
/// point. Additional backends would require both an upstream `sqlx::Database` impl and a release
/// of this crate.
///
/// [`sqlx::Sqlite`]: https://docs.rs/sqlx/latest/sqlx/struct.Sqlite.html
/// [`sqlx::Postgres`]: https://docs.rs/sqlx/latest/sqlx/struct.Postgres.html
/// [`sqlx::MySql`]: https://docs.rs/sqlx/latest/sqlx/struct.MySql.html
pub trait Database: sqlx::Database + sealed::Sealed {
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

/// Sealing module for [`Database`]. The supertrait bound on `Database: sealed::Sealed`
/// prevents downstream impls because only this crate can implement [`Sealed`](self::sealed::Sealed)
/// for the backend types.
mod sealed {
    /// Marker trait that prevents external impls of [`Database`](super::Database).
    pub trait Sealed {}

    #[cfg(feature = "sqlite")]
    impl Sealed for sqlx::Sqlite {}

    #[cfg(feature = "postgres")]
    impl Sealed for sqlx::Postgres {}

    #[cfg(feature = "mysql")]
    impl Sealed for sqlx::MySql {}
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
#[cfg_attr(docsrs, doc(cfg(feature = "sqlite")))]
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
#[cfg_attr(docsrs, doc(cfg(feature = "postgres")))]
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
#[cfg_attr(docsrs, doc(cfg(feature = "mysql")))]
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
