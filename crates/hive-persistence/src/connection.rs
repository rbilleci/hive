use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::time::Duration;

/// Builds the one `PgPool` a process uses, from the same `HIVE_DATABASE_URL` /
/// `HIVE_DATABASE_USER` / `HIVE_DATABASE_PASSWORD` triple the Java tree reads
/// (`RTD-JDBC-URL-COMPAT`). Accepts the JDBC form the harness passes
/// (`jdbc:postgresql://host:port/db`) and the plain `postgres://` form.
#[derive(Debug, Clone)]
pub struct ConnectionFactory {
    pool: PgPool,
}

impl ConnectionFactory {
    pub async fn connect(
        database_url: &str,
        user: &str,
        password: &str,
    ) -> Result<Self, sqlx::Error> {
        let options = parse_options(database_url, user, password)?;
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn parse_options(
    database_url: &str,
    user: &str,
    password: &str,
) -> Result<PgConnectOptions, sqlx::Error> {
    let (host, port, database) = parse_host_port_database(database_url)?;
    Ok(PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username(user)
        .password(password)
        .database(&database)
        .application_name("hive"))
}

fn parse_host_port_database(database_url: &str) -> Result<(String, u16, String), sqlx::Error> {
    let without_scheme = database_url
        .strip_prefix("jdbc:postgresql://")
        .or_else(|| database_url.strip_prefix("postgres://"))
        .or_else(|| database_url.strip_prefix("postgresql://"))
        .ok_or_else(|| {
            sqlx::Error::Configuration(
                format!("HIVE_DATABASE_URL '{database_url}' is not a recognized postgresql URL")
                    .into(),
            )
        })?;

    let (authority, database) = without_scheme.split_once('/').ok_or_else(|| {
        sqlx::Error::Configuration(
            format!("HIVE_DATABASE_URL '{database_url}' has no database path segment").into(),
        )
    })?;
    let database = database.split(['?', ';']).next().unwrap_or(database);

    let (host, port) = authority.split_once(':').ok_or_else(|| {
        sqlx::Error::Configuration(format!("HIVE_DATABASE_URL '{database_url}' has no port").into())
    })?;
    let port: u16 = port.parse().map_err(|_| {
        sqlx::Error::Configuration(
            format!("HIVE_DATABASE_URL '{database_url}' has an invalid port").into(),
        )
    })?;

    Ok((host.to_string(), port, database.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jdbc_form() {
        let (host, port, db) =
            parse_host_port_database("jdbc:postgresql://127.0.0.1:5432/hive").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 5432);
        assert_eq!(db, "hive");
    }

    #[test]
    fn parses_plain_postgres_form() {
        let (host, port, db) =
            parse_host_port_database("postgres://db.internal:6543/hive").unwrap();
        assert_eq!(host, "db.internal");
        assert_eq!(port, 6543);
        assert_eq!(db, "hive");
    }

    #[test]
    fn rejects_unrecognized_scheme() {
        assert!(parse_host_port_database("mysql://127.0.0.1:5432/hive").is_err());
    }
}
