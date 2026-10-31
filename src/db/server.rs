//! Server administration and diagnostics exposed by a live [`Connection`].
//!
//! These operations sit beside the core query path rather than inside it:
//! process and user management, server variables, query digests, connection
//! facts, and SQLite's local maintenance commands. Keeping them together gives
//! new server tools one focused home without growing `connection.rs`.

use anyhow::{Context as _, Result};

use super::config::Engine;
use super::connection::Connection;
use super::query::{Cell, QueryResult};
use super::query_log::QuerySource;
use super::{mysql, postgres, sql, sqlite, typed_placeholder};

/// What the query log shows in place of a password.
const REDACTED: &str = "********";

/// The outcome of asking for the slow/frequent query digest: either engine's
/// instrumentation is opt-in, so a missing extension or a server variable
/// that is off is not an error — it is told to the caller in words instead.
#[derive(Debug)]
pub enum QueryDigest {
    Available(QueryResult),
    Unavailable(String),
}

/// One connection's basic facts, for the connection information panel.
/// Everything here is either already known from the saved configuration or
/// one cheap read; a fact an engine cannot report cheaply stays `None` and
/// its row stays out of the panel rather than being guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub engine: Engine,
    /// Where the connection lives: `host:port` on a server engine, the file
    /// path on SQLite.
    pub server: String,
    /// The login name on a server engine; SQLite has no users.
    pub username: Option<String>,
    /// The server version string, as the server reports it.
    pub version: String,
    /// The database this connection is bound to; SQLite's database is its
    /// file, so this stays `None` there and the panel shows the path.
    pub database: Option<String>,
    /// The current database's size in bytes, where the engine reports one.
    pub size_bytes: Option<u64>,
    /// Seconds since the server started; SQLite has no server to ask.
    pub uptime_secs: Option<u64>,
    /// Backends currently connected to the server; SQLite has none.
    pub connections: Option<u64>,
}

/// A row cell as text, blank when the cell is missing or NULL — a missing
/// fact reads as an empty value, never a panic.
fn cell_text(row: &[Cell], index: usize) -> String {
    row.get(index).cloned().flatten().unwrap_or_default()
}

/// A row cell as an integer, `None` when it is missing, NULL, or does not
/// parse — the panel leaves that row out rather than failing on it.
fn cell_int(row: &[Cell], index: usize) -> Option<i64> {
    row.get(index)
        .cloned()
        .flatten()
        .and_then(|value| value.parse().ok())
}

/// A row cell as a float, `None` on the same terms as [`cell_int`].
fn cell_float(row: &[Cell], index: usize) -> Option<f64> {
    row.get(index)
        .cloned()
        .flatten()
        .and_then(|value| value.parse().ok())
}

impl Connection {
    /// Every other connection to the server and what it is doing right now.
    pub async fn processes(&self) -> Result<QueryResult> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::PROCESSES_SQL,
            Engine::MySql => mysql::PROCESSES_SQL,
            Engine::Sqlite => anyhow::bail!("SQLite has no server processes to list"),
        };
        self.run_query(sql).await
    }

    /// End a server process by the id [`Self::processes`] showed for it.
    pub async fn kill_process(&self, id: &str) -> Result<()> {
        self.refuse_read_only("the process was not ended")?;
        match self.config.engine {
            Engine::Postgres => {
                let sql = format!(
                    "select pg_terminate_backend({})",
                    typed_placeholder(Engine::Postgres, 1, "INT4")
                );
                self.execute(&sql, vec![Some(id.to_string())]).await?;
            }
            Engine::MySql => {
                let pid: u64 = id
                    .parse()
                    .map_err(|_| anyhow::anyhow!("not a process id: {id}"))?;
                self.execute(&format!("KILL {pid}"), vec![]).await?;
            }
            Engine::Sqlite => anyhow::bail!("SQLite has no server processes to end"),
        }
        Ok(())
    }

    /// Every user (Postgres: every role) on the server.
    pub async fn users(&self) -> Result<QueryResult> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::USERS_SQL,
            Engine::MySql => mysql::USERS_SQL,
            Engine::Sqlite => anyhow::bail!("SQLite has no users to list"),
        };
        self.run_query(sql).await
    }

    /// Create a login user. `host` is MySQL's `user@host` part.
    pub async fn create_user(&self, name: &str, host: Option<&str>, password: &str) -> Result<()> {
        self.refuse_read_only("the user was not created")?;
        let engine = self.config.engine;
        let sql = sql::create_user_sql(name, host, password, engine)
            .ok_or_else(|| anyhow::anyhow!("a valid name is required to create a user"))?;
        let shown = sql::create_user_sql(name, host, REDACTED, engine).unwrap_or_default();
        self.execute_logged_as(&sql, Vec::new(), &shown, QuerySource::Internal)
            .await?;
        Ok(())
    }

    /// Drop a user by the name (and host, on MySQL) [`Self::users`] showed.
    pub async fn drop_user(&self, name: &str, host: Option<&str>) -> Result<()> {
        self.refuse_read_only("the user was not dropped")?;
        let sql = sql::drop_user_sql(name, host, self.config.engine)
            .ok_or_else(|| anyhow::anyhow!("a valid name is required to drop a user"))?;
        self.execute(&sql, Vec::new()).await?;
        Ok(())
    }

    /// Set a user's password.
    pub async fn set_user_password(
        &self,
        name: &str,
        host: Option<&str>,
        password: &str,
    ) -> Result<()> {
        self.refuse_read_only("the password was not changed")?;
        let engine = self.config.engine;
        let sql = sql::set_user_password_sql(name, host, password, engine)
            .ok_or_else(|| anyhow::anyhow!("a valid name is required to set a password"))?;
        let shown = sql::set_user_password_sql(name, host, REDACTED, engine).unwrap_or_default();
        self.execute_logged_as(&sql, Vec::new(), &shown, QuerySource::Internal)
            .await?;
        Ok(())
    }

    /// Every server configuration setting, including whether it changed.
    pub async fn server_variables(&self) -> Result<QueryResult> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::VARIABLES_SQL,
            Engine::MySql => mysql::VARIABLES_SQL,
            Engine::Sqlite => anyhow::bail!("SQLite has no server variables to list"),
        };
        self.run_query(sql).await
    }

    /// The slow/frequent query digest, or a plain reason it is unavailable.
    pub async fn query_digest(&self) -> Result<QueryDigest> {
        match self.config.engine {
            Engine::Postgres => {
                let available = self.run_query(postgres::DIGEST_AVAILABLE_SQL).await?;
                if available.rows.is_empty() {
                    return Ok(QueryDigest::Unavailable(
                        "pg_stat_statements is not installed on this server. Enable it with \
                         `CREATE EXTENSION pg_stat_statements;` after adding it to \
                         shared_preload_libraries and restarting the server."
                            .to_string(),
                    ));
                }
                Ok(QueryDigest::Available(
                    self.run_query(postgres::DIGEST_SQL).await?,
                ))
            }
            Engine::MySql => {
                let available = self.run_query(mysql::DIGEST_AVAILABLE_SQL).await?;
                let on = available
                    .rows
                    .first()
                    .and_then(|row| row.get(1))
                    .and_then(|cell| cell.as_deref())
                    .is_some_and(|value| value.eq_ignore_ascii_case("ON"));
                if !on {
                    return Ok(QueryDigest::Unavailable(
                        "performance_schema is off on this server. It cannot be turned on \
                         while the server is running — set performance_schema=ON in its \
                         configuration and restart."
                            .to_string(),
                    ));
                }
                Ok(QueryDigest::Available(
                    self.run_query(mysql::DIGEST_SQL).await?,
                ))
            }
            Engine::Sqlite => anyhow::bail!("SQLite has no query digest to show"),
        }
    }

    /// The connection's basic facts for the connection information panel.
    pub async fn connection_info(&self) -> Result<ConnectionInfo> {
        let engine = self.config.engine;
        let mut info = ConnectionInfo {
            engine,
            server: match engine {
                Engine::Sqlite => self.config.database.clone(),
                _ => format!("{}:{}", self.config.host, self.config.port),
            },
            username: match engine {
                Engine::Sqlite => None,
                _ => Some(self.config.username.clone()),
            },
            version: String::new(),
            database: None,
            size_bytes: None,
            uptime_secs: None,
            connections: None,
        };

        match engine {
            Engine::Postgres => {
                let result = self.run_query(postgres::CONNECTION_INFO_SQL).await?;
                let row = result.rows.first().context("no connection info row")?;
                info.version = cell_text(row, 0);
                info.database = Some(cell_text(row, 1));
                info.size_bytes = cell_int(row, 2).map(|size| size as u64);
                info.uptime_secs = cell_float(row, 3).map(|secs| secs as u64);
                info.connections = cell_int(row, 4).map(|count| count as u64);
            }
            Engine::MySql => {
                let result = self.run_query(mysql::CONNECTION_INFO_SQL).await?;
                let row = result.rows.first().context("no connection info row")?;
                info.version = cell_text(row, 0);
                info.database = Some(cell_text(row, 1));
                info.size_bytes = cell_int(row, 2).map(|size| size as u64);
                info.uptime_secs = self
                    .global_status("Uptime")
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| value.parse().ok());
                info.connections = self
                    .global_status("Threads_connected")
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| value.parse().ok());
            }
            Engine::Sqlite => {
                let result = self.run_query(sqlite::CONNECTION_INFO_SQL).await?;
                let row = result.rows.first().context("no connection info row")?;
                info.version = cell_text(row, 0);
                info.size_bytes = std::fs::metadata(&self.config.database)
                    .ok()
                    .map(|metadata| metadata.len());
            }
        }

        if engine != Engine::Sqlite && info.database.as_deref() == Some("") {
            info.database = Some(self.config.database.clone());
        }

        Ok(info)
    }

    async fn global_status(&self, name: &str) -> Result<Option<String>> {
        let result = self
            .run_query(&format!("SHOW GLOBAL STATUS LIKE '{name}'"))
            .await?;
        Ok(result
            .rows
            .first()
            .and_then(|row| row.get(1))
            .cloned()
            .flatten())
    }

    /// Run one of SQLite's housekeeping commands.
    pub async fn run_maintenance(&self, task: sqlite::Maintenance) -> Result<QueryResult> {
        if self.config.engine != Engine::Sqlite {
            anyhow::bail!("maintenance commands are only offered for SQLite");
        }
        if task.writes() {
            self.refuse_read_only(&format!("{} was not run", task.label()))?;
        }
        self.run_query(task.sql()).await
    }
}
