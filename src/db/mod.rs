//! Database connectivity: connection configuration, pools, and query execution.
//!
//! Everything here is engine-agnostic; the per-engine modules own the sqlx
//! types and the mapping from SQL values to display strings.

pub mod mysql;
pub mod postgres;
pub mod query;
pub mod runtime;
pub mod sqlite;
pub mod statement;
pub mod store;

#[cfg(test)]
pub(crate) mod tests;

use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{
    AssertSqlSafe, Column, Database, Encode, Executor, IntoArguments, Row, SqlSafeStr, Type,
    TypeInfo,
};
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

/// How much ceremony a connection asks for before anything is written.
///
/// One scale, from the most careful to the least: refuse writes, ask about
/// each one, hold inline edits until they are applied, apply them as the user
/// moves on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyMode {
    /// Nothing may be written: the session is opened read-only and statements
    /// that do not plainly read are refused before they are sent.
    ReadOnly,
    /// Every write is shown to the user before it runs.
    ConfirmWrites,
    /// Inline edits wait in the grid until they are applied.
    #[default]
    Staged,
    /// Inline edits are written as soon as the selection leaves the row.
    AutoApply,
}

impl SafetyMode {
    pub const ALL: [SafetyMode; 4] = [
        SafetyMode::ReadOnly,
        SafetyMode::ConfirmWrites,
        SafetyMode::Staged,
        SafetyMode::AutoApply,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SafetyMode::ReadOnly => "Read-only",
            SafetyMode::ConfirmWrites => "Confirm writes",
            SafetyMode::Staged => "Staged edits",
            SafetyMode::AutoApply => "Auto-apply",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SafetyMode::ReadOnly => "Refuses anything that writes",
            SafetyMode::ConfirmWrites => "Shows every write before it runs",
            SafetyMode::Staged => "Edits wait until you apply them",
            SafetyMode::AutoApply => "Edits are written when you leave the row",
        }
    }

    pub fn is_read_only(self) -> bool {
        matches!(self, SafetyMode::ReadOnly)
    }

    pub fn confirms_writes(self) -> bool {
        matches!(self, SafetyMode::ConfirmWrites)
    }

    pub fn auto_applies(self) -> bool {
        matches!(self, SafetyMode::AutoApply)
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
    /// How careful this connection is about writes. Absent in files written
    /// before the setting existed, which read back as the safer mode.
    #[serde(default)]
    pub safety: SafetyMode,
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
            safety: SafetyMode::default(),
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

/// Quote `value` as a SQL string literal.
///
/// Used only for the metadata queries that cannot take a bind parameter (a
/// `PRAGMA` table function, for one); user data goes through [`Connection::execute`].
pub(crate) fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The placeholder for the `index`-th bind parameter, counting from one.
pub(crate) fn placeholder(engine: Engine, index: usize) -> String {
    match engine {
        Engine::Postgres => format!("${index}"),
        Engine::MySql | Engine::Sqlite => "?".to_string(),
    }
}

/// A placeholder that will be accepted where a `type_name` value belongs.
///
/// Every parameter is bound as text. MySQL and SQLite coerce that to the
/// column's type on their own; Postgres refuses it outright, so its
/// placeholder is cast. Type names come back from the driver (`INT4`,
/// `TIMESTAMPTZ`, `INT4[]`), and all of them are castable as written — bar a
/// user-defined type whose name is not lower case, which Postgres down-cases
/// and then fails to find.
pub(crate) fn typed_placeholder(engine: Engine, index: usize, type_name: &str) -> String {
    let placeholder = placeholder(engine, index);
    match engine {
        Engine::Postgres if !type_name.is_empty() => {
            format!("cast({placeholder} as {type_name})")
        }
        _ => placeholder,
    }
}

/// How the rows of a table can be addressed by a generated `UPDATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    /// Primary key columns, in key order. Already part of `select *`.
    Columns(Vec<String>),
    /// The engine's own row identifier, selected alongside the row because
    /// `select *` does not include it: `rowid` on SQLite, `ctid` on Postgres.
    RowId(&'static str),
    /// The rows cannot be addressed; the reason is shown to the user.
    Unavailable(&'static str),
}

impl RowKey {
    pub fn is_available(&self) -> bool {
        !matches!(self, RowKey::Unavailable(_))
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
        self.run_query_with(sql, Vec::new()).await
    }

    /// The same, binding `params` in order.
    ///
    /// Used by the generated reads — a filtered table view, for one — so a
    /// value the user typed stays a value rather than becoming SQL.
    pub async fn run_query_with(&self, sql: &str, params: Vec<Cell>) -> Result<QueryResult> {
        self.refuse_write(sql)?;
        match &self.pool {
            Pool::Postgres(pool) => fetch_all(pool, sql, params, postgres::cell).await,
            Pool::MySql(pool) => fetch_all(pool, sql, params, mysql::cell).await,
            Pool::Sqlite(pool) => fetch_all(pool, sql, params, sqlite::cell).await,
        }
    }

    /// How the rows of `object` can be addressed by a write.
    ///
    /// Read once when a table is opened: the answer only changes when the
    /// table itself does.
    pub async fn row_key(&self, object: &DatabaseObject) -> Result<RowKey> {
        if object.kind == ObjectKind::View {
            return Ok(RowKey::Unavailable("a view cannot be edited"));
        }

        let engine = self.config.engine;
        let sql = match engine {
            // `objects` drops the schema when it is the default one, so an
            // unqualified Postgres table is in `public`.
            Engine::Postgres => postgres::primary_key_sql(
                object.schema.as_deref().unwrap_or("public"),
                &object.name,
            ),
            Engine::MySql => mysql::primary_key_sql(&object.name),
            Engine::Sqlite => sqlite::primary_key_sql(&object.name),
        };

        let result = self.run_query(&sql).await?;
        let columns: Vec<String> = result
            .rows
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
            .collect();

        if !columns.is_empty() {
            return Ok(RowKey::Columns(columns));
        }

        Ok(match engine {
            Engine::Postgres => RowKey::RowId("ctid"),
            Engine::Sqlite => RowKey::RowId("rowid"),
            // MySQL has no row identifier to fall back on.
            Engine::MySql => RowKey::Unavailable("a table without a primary key cannot be edited"),
        })
    }

    /// Refuse a statement a read-only connection must not run.
    ///
    /// The server is told to refuse writes as well when the pool is opened;
    /// this is the half that can name the statement it stopped, and that
    /// stops it before it costs a round trip.
    fn refuse_write(&self, sql: &str) -> Result<()> {
        if !self.config.safety.is_read_only() {
            return Ok(());
        }
        if let Some(word) = statement::first_write(sql) {
            anyhow::bail!("this connection is read-only, so the {word} statement was not run");
        }
        Ok(())
    }

    /// Run a write and report how many rows it matched.
    ///
    /// Every parameter is bound as text or `NULL`; see [`typed_placeholder`]
    /// for why that is enough.
    pub async fn execute(&self, sql: &str, params: Vec<Cell>) -> Result<u64> {
        if self.config.safety.is_read_only() {
            anyhow::bail!("this connection is read-only");
        }
        match &self.pool {
            Pool::Postgres(pool) => execute_with(pool, sql, params, postgres::rows_affected).await,
            Pool::MySql(pool) => execute_with(pool, sql, params, mysql::rows_affected).await,
            Pool::Sqlite(pool) => execute_with(pool, sql, params, sqlite::rows_affected).await,
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
async fn fetch_all<DB, F>(
    pool: &sqlx::Pool<DB>,
    sql: &str,
    params: Vec<Cell>,
    cell: F,
) -> Result<QueryResult>
where
    DB: Database,
    for<'c> &'c sqlx::Pool<DB>: Executor<'c, Database = DB>,
    <DB as Database>::Arguments: IntoArguments<DB>,
    for<'q> Option<String>: Encode<'q, DB>,
    String: Type<DB>,
    F: Fn(&DB::Row, usize) -> Cell,
{
    // The statement comes from the user's editor: running it verbatim is the
    // whole point of a database client, so sqlx's injection guard is waived.
    let statement = AssertSqlSafe(sql.to_string()).into_sql_str();

    let started = Instant::now();
    let mut query = sqlx::query(statement.clone());
    for param in params {
        query = query.bind(param);
    }
    let rows = query.fetch_all(pool).await?;
    let elapsed = started.elapsed();

    let (columns, column_types): (Vec<String>, Vec<String>) = match rows.first() {
        Some(row) => describe_columns(row.columns()),
        None => Executor::describe(pool, statement)
            .await
            .map(|described| describe_columns(described.columns()))
            .unwrap_or_default(),
    };

    let rows = rows
        .iter()
        .map(|row| (0..columns.len()).map(|index| cell(row, index)).collect())
        .collect();

    Ok(QueryResult {
        columns,
        column_types,
        rows,
        elapsed,
    })
}

/// Split a driver's columns into their names and their type names.
fn describe_columns<C: Column>(columns: &[C]) -> (Vec<String>, Vec<String>) {
    columns
        .iter()
        .map(|column| {
            (
                column.name().to_string(),
                column.type_info().name().to_string(),
            )
        })
        .unzip()
}

/// Run a write, binding `params` in order, and report the rows it matched.
///
/// The count is the driver's own, which is matched rows rather than changed
/// rows on all three engines: sqlx asks MySQL for `FOUND_ROWS` when it
/// connects, so re-saving a row its own value still counts as one.
async fn execute_with<DB>(
    pool: &sqlx::Pool<DB>,
    sql: &str,
    params: Vec<Cell>,
    rows_affected: fn(&DB::QueryResult) -> u64,
) -> Result<u64>
where
    DB: Database,
    for<'c> &'c sqlx::Pool<DB>: Executor<'c, Database = DB>,
    <DB as Database>::Arguments: IntoArguments<DB>,
    for<'q> Option<String>: Encode<'q, DB>,
    String: Type<DB>,
{
    // The statement is generated here rather than typed by the user, and the
    // values in it are bound, so nothing in the string came from outside.
    let statement = AssertSqlSafe(sql.to_string()).into_sql_str();

    let mut query = sqlx::query(statement);
    for param in params {
        query = query.bind(param);
    }

    let result = query.execute(pool).await?;
    Ok(rows_affected(&result))
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
