use std::time::Duration;

/// Builds the one `sea_orm::DatabaseConnection` a process uses, from the same
/// `HIVE_DATABASE_URL`/`HIVE_DATABASE_USER`/`HIVE_DATABASE_PASSWORD` triple the Java tree reads
/// (`RTD-JDBC-URL-COMPAT`). Accepts the JDBC form the harness passes
/// (`jdbc:postgresql://host:port/db`) and the plain `postgres://` form.
///
/// During the Seaography rewrite (`docs/graphql-seaography-rewrite-plan.md`) this held a second,
/// independent `sqlx::PgPool` alongside `dynamic` for every repository not yet ported off `sqlx`
/// (`GSR-PHASE-0`'s correction to `GSR-CONNECTION`: `sea-orm`'s vendored `sqlx` is a structurally
/// different type from a direct `sqlx` dependency, so the two pools could not be bridged). `sqlx`
/// left the workspace's direct dependencies at `GSR-PHASE-P8`, once `audit` (the last repository
/// still built on it) was ported; `pool()` is removed and `dynamic` connects eagerly with no
/// connection cap, taking over the one-pool-per-process role `pool` used to have.
#[derive(Debug, Clone)]
pub struct ConnectionFactory {
    dynamic: sea_orm::DatabaseConnection,
}

impl ConnectionFactory {
    pub async fn connect(
        database_url: &str,
        user: &str,
        password: &str,
    ) -> Result<Self, sea_orm::DbErr> {
        let dynamic = connect_dynamic(database_url, user, password).await?;
        Ok(Self { dynamic })
    }

    pub fn dynamic(&self) -> &sea_orm::DatabaseConnection {
        &self.dynamic
    }
}

async fn connect_dynamic(
    database_url: &str,
    user: &str,
    password: &str,
) -> Result<sea_orm::DatabaseConnection, sea_orm::DbErr> {
    let (host, port, database) = parse_host_port_database(database_url)
        .map_err(|error| sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal(error)))?;
    // Local/CI passwords carry no reserved URL characters; percent-encoding is not needed.
    let url = format!("postgres://{user}:{password}@{host}:{port}/{database}");
    let mut options = sea_orm::ConnectOptions::new(url);
    // Eager, with no `max_connections` override: this is now the sole connection every request
    // and background task shares, the same role `sqlx::PgPoolOptions::new()` (also uncapped,
    // defaulting to 10) used to have — `GSR-PHASE-P8` retires the lazy, 5-connection-capped
    // configuration the interim two-pool split needed while almost nothing used this connection.
    options.acquire_timeout(Duration::from_secs(5));
    sea_orm::Database::connect(options).await
}

fn parse_host_port_database(database_url: &str) -> Result<(String, u16, String), String> {
    let without_scheme = database_url
        .strip_prefix("jdbc:postgresql://")
        .or_else(|| database_url.strip_prefix("postgres://"))
        .or_else(|| database_url.strip_prefix("postgresql://"))
        .ok_or_else(|| {
            format!("HIVE_DATABASE_URL '{database_url}' is not a recognized postgresql URL")
        })?;

    let (authority, database) = without_scheme.split_once('/').ok_or_else(|| {
        format!("HIVE_DATABASE_URL '{database_url}' has no database path segment")
    })?;
    let database = database.split(['?', ';']).next().unwrap_or(database);

    let (host, port) = authority
        .split_once(':')
        .ok_or_else(|| format!("HIVE_DATABASE_URL '{database_url}' has no port"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("HIVE_DATABASE_URL '{database_url}' has an invalid port"))?;

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
