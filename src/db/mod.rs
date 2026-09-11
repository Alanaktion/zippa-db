//! Database connectivity: connection configuration, pools, and query execution.
//!
//! Everything here is engine-agnostic; the per-engine modules own the sqlx
//! types and the mapping from SQL values to display strings.

pub mod mysql;
pub mod postgres;
pub mod query;
pub mod runtime;
pub mod sqlite;
pub mod store;

#[cfg(test)]
pub(crate) mod tests;

use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, Column, Database, Executor, IntoArguments, Row, SqlSafeStr};
use uuid::Uuid;

use query::{Cell, QueryResult};

/// Connections opened per saved connection.
const POOL_SIZE: u32 = 5;

/// Database engine a connection targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Postgres,
    MySql,
    Sqlite,
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::Sqlite];

    pub fn label(self) -> &'static str {
        match self {
            Engine::Postgres => "PostgreSQL",
            Engine::MySql => "MySQL",
            Engine::Sqlite => "SQLite",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::MySql => 3306,
            Engine::Sqlite => 0,
        }
    }

    /// SQLite connects to a file, so it has no host, port, or credentials.
    pub fn is_file_based(self) -> bool {
        matches!(self, Engine::Sqlite)
    }
}

/// A saved connection as configured by the user.
///
/// The password is not part of this struct: it lives in the OS keychain, keyed
/// by [`ConnectionConfig::id`]. See [`store`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Database name, or the file path for SQLite.
    pub database: String,
}

impl ConnectionConfig {
    pub fn new(engine: Engine) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            engine,
            host: "localhost".into(),
            port: engine.default_port(),
            username: String::new(),
            database: String::new(),
        }
    }

    /// Name to show when the user has not given the connection one.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }

        // Showing the whole path is the target line's job; a name wants to be
        // short enough to sit in a tab.
        if self.engine.is_file_based() {
            return file_name(&self.database);
        }
        self.display_target()
    }

    /// `localhost:5432/app`, or the file's path for SQLite.
    pub fn display_target(&self) -> String {
        if self.engine.is_file_based() {
            return self.database.clone();
        }
        format!("{}:{}/{}", self.host, self.port, self.database)
    }
}

/// The file's own name, or the whole path when it has none.
pub(crate) fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self::new(Engine::Postgres)
    }
}

/// An engine-specific connection pool.
#[derive(Debug)]
enum Pool {
    Postgres(sqlx::PgPool),
    MySql(sqlx::MySqlPool),
    Sqlite(sqlx::SqlitePool),
}

/// A table or view reported by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseObject {
    /// Schema the object lives in. `None` for engines without schemas.
    pub schema: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
}

impl DatabaseObject {
    /// Name as shown in the sidebar, qualified only when the schema adds
    /// something the user cannot already see.
    pub fn label(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{schema}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Quote `name` when it would not survive being pasted into a statement bare.
///
/// Plain lower-case identifiers are left alone so the generated SQL reads the
/// way someone would type it.
pub fn quote_identifier(name: &str, engine: Engine) -> String {
    let bare = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });

    if bare {
        return name.to_string();
    }

    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        Engine::Postgres | Engine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// A live connection to one database.
#[derive(Debug)]
pub struct Connection {
    pub config: ConnectionConfig,
    /// Kept in memory so switching databases can reopen the pool without
    /// asking the user for the password again. Never written to disk.
    password: Option<String>,
    pool: Pool,
}

impl Connection {
    /// Open a pool and verify it by acquiring one connection.
    pub async fn open(config: ConnectionConfig, password: Option<String>) -> Result<Self> {
        let password = password.as_deref();
        let pool = match config.engine {
            Engine::Postgres => Pool::Postgres(postgres::connect(&config, password).await?),
            Engine::MySql => Pool::MySql(mysql::connect(&config, password).await?),
            Engine::Sqlite => Pool::Sqlite(sqlite::connect(&config).await?),
        };
        Ok(Self {
            config,
            password: password.map(str::to_string),
            pool,
        })
    }

    /// The database this connection is bound to.
    pub fn database(&self) -> &str {
        &self.config.database
    }

    /// Open a second connection to `database` on the same server.
    ///
    /// Neither engine can move an existing pool to another database, so this
    /// reopens one; the caller closes the connection it replaces.
    pub async fn with_database(&self, database: &str) -> Result<Self> {
        let mut config = self.config.clone();
        config.database = database.to_string();
        Self::open(config, self.password.clone()).await
    }

    /// Databases the user can switch to on this server.
    pub async fn databases(&self) -> Result<Vec<String>> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::DATABASES_SQL,
            Engine::MySql => mysql::DATABASES_SQL,
            Engine::Sqlite => sqlite::DATABASES_SQL,
        };

        let result = self.run_query(sql).await?;
        Ok(result
            .rows
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
            .collect())
    }

    /// Tables and views in the current database.
    pub async fn objects(&self) -> Result<Vec<DatabaseObject>> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::OBJECTS_SQL,
            Engine::MySql => mysql::OBJECTS_SQL,
            Engine::Sqlite => sqlite::OBJECTS_SQL,
        };

        let result = self.run_query(sql).await?;
        Ok(result
            .rows
            .iter()
            .filter_map(|row| {
                let name = row.get(1)?.clone()?;
                let kind = match row.get(2).and_then(|cell| cell.as_deref()) {
                    Some(kind) if kind.eq_ignore_ascii_case("VIEW") => ObjectKind::View,
                    _ => ObjectKind::Table,
                };

                // Only qualify a name when the schema adds something: MySQL's
                // schema is always the current database, and Postgres tables
                // usually sit in `public`.
                let schema =
                    row.first()
                        .cloned()
                        .flatten()
                        .filter(|schema| match self.config.engine {
                            Engine::Postgres => schema != "public",
                            Engine::MySql | Engine::Sqlite => false,
                        });

                Some(DatabaseObject { schema, name, kind })
            })
            .collect())
    }

    pub async fn run_query(&self, sql: &str) -> Result<QueryResult> {
        match &self.pool {
            Pool::Postgres(pool) => fetch_all(pool, sql, postgres::cell).await,
            Pool::MySql(pool) => fetch_all(pool, sql, mysql::cell).await,
            Pool::Sqlite(pool) => fetch_all(pool, sql, sqlite::cell).await,
        }
    }

    pub async fn close(&self) {
        match &self.pool {
            Pool::Postgres(pool) => pool.close().await,
            Pool::MySql(pool) => pool.close().await,
            Pool::Sqlite(pool) => pool.close().await,
        }
    }
}

/// Run `sql` and turn every value into a display string with `cell`.
///
/// Column names come from the returned rows; when a statement returns none,
/// the server is asked to describe it so the grid can still show its shape.
async fn fetch_all<DB, F>(pool: &sqlx::Pool<DB>, sql: &str, cell: F) -> Result<QueryResult>
where
    DB: Database,
    for<'c> &'c sqlx::Pool<DB>: Executor<'c, Database = DB>,
    <DB as Database>::Arguments: IntoArguments<DB>,
    F: Fn(&DB::Row, usize) -> Cell,
{
    // The statement comes from the user's editor: running it verbatim is the
    // whole point of a database client, so sqlx's injection guard is waived.
    let statement = AssertSqlSafe(sql.to_string()).into_sql_str();

    let started = Instant::now();
    let rows = sqlx::query(statement.clone()).fetch_all(pool).await?;
    let elapsed = started.elapsed();

    let columns: Vec<String> = match rows.first() {
        Some(row) => row
            .columns()
            .iter()
            .map(|column| column.name().to_string())
            .collect(),
        None => Executor::describe(pool, statement)
            .await
            .map(|described| {
                described
                    .columns()
                    .iter()
                    .map(|column| column.name().to_string())
                    .collect()
            })
            .unwrap_or_default(),
    };

    let rows = rows
        .iter()
        .map(|row| (0..columns.len()).map(|index| cell(row, index)).collect())
        .collect();

    Ok(QueryResult {
        columns,
        rows,
        elapsed,
    })
}

/// Decode a column into a display string, or `None` when the decode fails.
macro_rules! decode {
    ($row:expr, $index:expr, $ty:ty) => {
        $row.try_get::<$ty, _>($index)
            .ok()
            .map(|value| value.to_string())
    };
}

pub(crate) use decode;
