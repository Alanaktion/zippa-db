//! A live connection to one database, and the shared query path behind it.
//!
//! [`Connection`] owns the engine-specific pool and dispatches on it into the
//! generic [`fetch_all`]/[`execute_with`] helpers, which work over any
//! `sqlx::Database`. Everything engine-specific — pool setup, the SQL that
//! lists databases and objects, decoding a row cell — lives in the sibling
//! `postgres` / `mysql` / `sqlite` modules.

use std::time::Instant;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sqlx::Either;
use sqlx::{
    AssertSqlSafe, Column, Database, Encode, Executor, IntoArguments, Row, SqlSafeStr, Type,
    TypeInfo,
};

use super::catalog::{Catalog, CatalogEntry, CatalogKind, MAX_ENTRIES};
use super::config::{ConnectionConfig, Engine};
use super::import::{self, Dialect, ImportProgress, ImportRequest, ImportSummary};
use super::plan::{self, Explained, Plan};
use super::query::{Cell, QueryResult};
use super::{mysql, postgres, sqlite, statement};

/// Connections opened per saved connection.
pub(crate) const POOL_SIZE: u32 = 5;

/// An engine-specific connection pool.
#[derive(Debug)]
enum Pool {
    Postgres(sqlx::PgPool),
    MySql(sqlx::MySqlPool),
    Sqlite(sqlx::SqlitePool),
}

/// A table or view reported by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseObject {
    /// Schema the object lives in. `None` for engines without schemas.
    pub schema: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Table,
    View,
}

/// A function, procedure or sequence reported by the server.
///
/// Kept apart from [`DatabaseObject`]: those are what a tab can open and a
/// query can select from, and these are not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    /// Schema the object lives in. `None` for engines without schemas.
    pub schema: Option<String>,
    pub name: String,
    /// Argument types, so overloads of one function can be told apart.
    pub arguments: Option<String>,
    pub kind: StoredKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredKind {
    Function,
    Procedure,
    Sequence,
}

impl StoredObject {
    /// Name as shown in the sidebar: schema-qualified when the schema adds
    /// something, with the argument types of a routine after it.
    pub fn label(&self) -> String {
        let name = match &self.schema {
            Some(schema) => format!("{schema}.{}", self.name),
            None => self.name.clone(),
        };
        match &self.arguments {
            Some(arguments) => format!("{name}({arguments})"),
            None => name,
        }
    }
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

    /// Functions, procedures and sequences in the current database.
    pub async fn stored_objects(&self) -> Result<Vec<StoredObject>> {
        let sql = match self.config.engine {
            Engine::Postgres => postgres::ROUTINES_SQL,
            Engine::MySql => mysql::ROUTINES_SQL,
            // SQLite has no stored routines and no sequences.
            Engine::Sqlite => return Ok(Vec::new()),
        };

        let result = self.run_query(sql).await?;
        Ok(result
            .rows
            .iter()
            .filter_map(|row| {
                let name = row.get(1)?.clone()?;
                let kind = match row.get(2).and_then(|cell| cell.as_deref())? {
                    kind if kind.eq_ignore_ascii_case("PROCEDURE") => StoredKind::Procedure,
                    kind if kind.eq_ignore_ascii_case("SEQUENCE") => StoredKind::Sequence,
                    _ => StoredKind::Function,
                };
                let schema =
                    row.first()
                        .cloned()
                        .flatten()
                        .filter(|schema| match self.config.engine {
                            Engine::Postgres => schema != "public",
                            Engine::MySql | Engine::Sqlite => false,
                        });
                // Both engines give an empty list for a routine of no
                // arguments (Postgres directly, MySQL as a NULL); the
                // parentheses still tell it apart from a sequence.
                let arguments = match kind {
                    StoredKind::Sequence => None,
                    _ => Some(row.get(3).cloned().flatten().unwrap_or_default()),
                };

                Some(StoredObject {
                    schema,
                    name,
                    arguments,
                    kind,
                })
            })
            .collect())
    }

    /// A snapshot of the whole schema, for schema search.
    ///
    /// One bulk read per kind rather than per table, so a session pays for this
    /// once. Tables, views and routines reuse [`Self::objects`] and
    /// [`Self::stored_objects`]; columns, indexes and triggers are the three
    /// new queries. Capped at [`MAX_ENTRIES`], with the untruncated count kept
    /// so the caller can say what was left out.
    pub async fn catalog(&self) -> Result<Catalog> {
        let (columns, indexes, triggers) = match self.config.engine {
            Engine::Postgres => (
                postgres::CATALOG_COLUMNS_SQL,
                postgres::CATALOG_INDEXES_SQL,
                postgres::CATALOG_TRIGGERS_SQL,
            ),
            Engine::MySql => (
                mysql::CATALOG_COLUMNS_SQL,
                mysql::CATALOG_INDEXES_SQL,
                mysql::CATALOG_TRIGGERS_SQL,
            ),
            Engine::Sqlite => (
                sqlite::CATALOG_COLUMNS_SQL,
                sqlite::CATALOG_INDEXES_SQL,
                sqlite::CATALOG_TRIGGERS_SQL,
            ),
        };

        let objects = self.objects().await?;
        let stored = self.stored_objects().await?;
        let columns = self.run_query(columns).await?;
        let indexes = self.run_query(indexes).await?;
        let triggers = self.run_query(triggers).await?;

        let mut entries: Vec<CatalogEntry> = Vec::new();
        entries.extend(objects.into_iter().map(CatalogEntry::object));
        entries.extend(stored.into_iter().map(CatalogEntry::routine));
        for (rows, kind) in [
            (&columns.rows, CatalogKind::Column),
            (&indexes.rows, CatalogKind::Index),
            (&triggers.rows, CatalogKind::Trigger),
        ] {
            for row in rows {
                let (owner, name, detail) = self.catalog_row(row);
                entries.push(CatalogEntry::member(kind, owner, name, detail));
            }
        }

        let total = entries.len();
        entries.truncate(MAX_ENTRIES);
        Ok(Catalog { entries, total })
    }

    /// One catalog row: `(schema, owning table, its kind, entry name, detail)`,
    /// the layout every engine's catalog query shares.
    fn catalog_row(&self, row: &[Cell]) -> (DatabaseObject, String, String) {
        let text = |index: usize| {
            row.get(index)
                .and_then(|cell| cell.clone())
                .unwrap_or_default()
        };
        // Only qualify a name when the schema adds something, so a catalog
        // entry's owner is the same `DatabaseObject` the sidebar's `objects`
        // would hand back and an already-open tab is found again.
        let schema = text(0);
        let schema = match self.config.engine {
            Engine::Postgres => Some(schema).filter(|schema| schema != "public"),
            Engine::MySql | Engine::Sqlite => None,
        };
        let kind = match text(2) {
            kind if kind.eq_ignore_ascii_case("VIEW") => ObjectKind::View,
            _ => ObjectKind::Table,
        };
        (
            DatabaseObject {
                schema,
                name: text(1),
                kind,
            },
            text(3),
            text(4),
        )
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
        self.fetch(sql, params).await
    }

    /// Send `sql` with no client-side classification.
    ///
    /// The caller has already decided the statement is safe — [`Self::explain`]
    /// does that with `statement::explained` — so the read-only pool's own
    /// session remains the guard. Skipping [`Self::refuse_write`] is what lets a
    /// read-only connection run `EXPLAIN ANALYZE`, which the classifier would
    /// otherwise refuse for the `ANALYZE` word alone.
    async fn fetch(&self, sql: &str, params: Vec<Cell>) -> Result<QueryResult> {
        match &self.pool {
            Pool::Postgres(pool) => {
                fetch_all(pool, sql, params, postgres::cell, postgres::rows_affected).await
            }
            Pool::MySql(pool) => {
                fetch_all(pool, sql, params, mysql::cell, mysql::rows_affected).await
            }
            Pool::Sqlite(pool) => {
                fetch_all(pool, sql, params, sqlite::cell, sqlite::rows_affected).await
            }
        }
    }

    /// Read the plan for one statement.
    ///
    /// `sql` is the statement the caller picked — the selection, or the one the
    /// caret is in, as a run does. A statement that already carries its own
    /// `EXPLAIN` header is run as written rather than wrapped again; otherwise
    /// `analyze` chooses between a plain plan and one the server ran the
    /// statement to produce.
    ///
    /// `ANALYZE` is only allowed for a statement that reads: it runs what it
    /// explains, and a write would then happen. The app gates the button as
    /// well, so this refusal is the backstop behind it.
    ///
    /// SQLite has no `EXPLAIN ANALYZE`, so a request for one there is refused
    /// rather than answered with the plain plan.
    pub async fn explain(&self, sql: &str, analyze: bool) -> Result<Explained> {
        let statement = sql.trim().trim_end_matches(';').trim();
        // One statement is what a plan describes; a script has no single plan
        // to draw, and the caller's buffer may be a selection of many.
        if statement.is_empty() || statement::split(statement).len() != 1 {
            anyhow::bail!("Select one statement to explain.");
        }

        // What is being explained, and whether this request runs it: the
        // caller's flag, or an `ANALYZE` the user already wrote into a header
        // of their own.
        let header = statement::explained(statement);
        let (inner, runs) = match &header {
            Some((inner, analyzes)) => (inner.clone(), analyze || *analyzes),
            None => (statement.to_string(), analyze),
        };
        if runs && let Some(word) = statement::first_write(&inner) {
            anyhow::bail!("EXPLAIN ANALYZE would run this statement; it changes data ({word}).");
        }

        let already = header.is_some();
        match self.config.engine {
            Engine::Postgres => {
                let sql = match (already, analyze) {
                    (true, _) => statement.to_string(),
                    (false, true) => {
                        format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {inner}")
                    }
                    (false, false) => format!("EXPLAIN (FORMAT JSON) {inner}"),
                };
                let result = self.fetch(&sql, Vec::new()).await?;
                let text = result
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|cell| cell.clone())
                    .unwrap_or_else(|| "(no plan returned)".to_string());
                let mut plan = plan::postgres(&text, analyze).unwrap_or_else(|| Plan::raw(&text));
                plan.elapsed = result.elapsed;
                Ok(Explained::Plan(plan))
            }
            Engine::MySql => {
                // The tree format arrived in 8.0.16 and `EXPLAIN ANALYZE` in
                // 8.0.18; MariaDB has neither. A server that answers with
                // something the parser does not know, or refuses the format,
                // gets the classic table shown in the grid instead — detected
                // by trying, never by reading the version.
                let tree = match (already, analyze) {
                    (true, _) => statement.to_string(),
                    (false, true) => format!("EXPLAIN ANALYZE {inner}"),
                    (false, false) => format!("EXPLAIN FORMAT=TREE {inner}"),
                };
                if let Ok(result) = self.fetch(&tree, Vec::new()).await {
                    let text = result
                        .rows
                        .first()
                        .and_then(|row| row.first())
                        .and_then(|cell| cell.clone())
                        .unwrap_or_default();
                    if let Some(mut plan) = plan::mysql(&text, analyze) {
                        plan.elapsed = result.elapsed;
                        return Ok(Explained::Plan(plan));
                    }
                }

                let classic = format!("EXPLAIN {inner}");
                let result = self.fetch(&classic, Vec::new()).await?;
                Ok(Explained::Rows(result))
            }
            Engine::Sqlite => {
                // SQLite's planner can describe a plan but never reports how
                // long a step really took — there is no `EXPLAIN ANALYZE`.
                // Answering a request for actual times with the plain plan
                // would be claiming timings the server never measured, so it
                // is refused instead. The analyze button is disabled for
                // SQLite; this is the backstop behind it.
                if runs {
                    anyhow::bail!(
                        "SQLite has no EXPLAIN ANALYZE, so actual times are not available. \
                         Use Explain to see the query plan."
                    );
                }
                let sql = if already {
                    statement.to_string()
                } else {
                    format!("EXPLAIN QUERY PLAN {inner}")
                };
                let result = self.fetch(&sql, Vec::new()).await?;
                let mut plan = plan::sqlite(&result);
                plan.elapsed = result.elapsed;
                Ok(Explained::Plan(plan))
            }
        }
    }

    /// Run every statement in `sql`, in order.
    ///
    /// Each one's rows come back on their own, so a script that selects twice
    /// answers with two results. A statement that fails stops the run, and the
    /// ones before it have already happened: there is no transaction around
    /// this yet.
    pub async fn run_script(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let mut results = Vec::new();
        for (position, statement) in statement::split(sql).into_iter().enumerate() {
            let result = self
                .run_query(&statement.text)
                .await
                .with_context(|| format!("statement {}", position + 1))?;
            results.push(result);
        }
        Ok(results)
    }

    /// Run a SQL dump against this connection.
    ///
    /// Everything runs on one dedicated connection, so the session state a dump
    /// sets up survives from one statement to the next, and the selected error
    /// policy decides what a failure does. Progress is reported through
    /// `progress` as the dump is read.
    ///
    /// A read-only connection is refused here rather than by the server, so the
    /// reason can name the safety mode.
    pub async fn import_dump(
        &self,
        request: ImportRequest,
        progress: tokio::sync::mpsc::UnboundedSender<ImportProgress>,
    ) -> Result<ImportSummary> {
        if self.config.safety.is_read_only() {
            anyhow::bail!("this connection is read-only, so a dump cannot be imported");
        }

        let dialect = match self.config.engine {
            Engine::Postgres => Dialect::Postgres,
            Engine::MySql => Dialect::MySql,
            Engine::Sqlite => Dialect::Sqlite,
        };

        let session = match &self.pool {
            Pool::Postgres(pool) => import::Session::postgres(pool).await?,
            Pool::MySql(pool) => import::Session::mysql(pool).await?,
            Pool::Sqlite(pool) => import::Session::sqlite(pool).await?,
        };

        import::run(session, dialect, &request, progress).await
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
    /// Every parameter is bound as text or `NULL`; see
    /// [`typed_placeholder`](super::typed_placeholder) for why that is enough.
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

    /// Rebuild a SQLite table as one atomic change.
    ///
    /// The caller has already generated the body of the procedure (create the
    /// scratch table, copy the rows, drop the old one, rename, put its
    /// indexes/triggers/views back); this owns the parts that cannot be plain
    /// statements: one dedicated connection, foreign keys off around a
    /// transaction, and a foreign-key check before it commits.
    pub async fn rebuild_table(&self, statements: Vec<String>) -> Result<()> {
        if self.config.safety.is_read_only() {
            anyhow::bail!("this connection is read-only");
        }
        match &self.pool {
            Pool::Sqlite(pool) => sqlite::rebuild(pool, &statements).await,
            // Only SQLite needs the copy-and-swap: the other engines restate
            // the table in place.
            Pool::Postgres(_) | Pool::MySql(_) => {
                anyhow::bail!("a table rebuild is only used on SQLite")
            }
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
    rows_affected: fn(&DB::QueryResult) -> u64,
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

    // Rows and counts come back in one stream: a statement can both change
    // rows and return them, as `insert ... returning` does. The deprecation on
    // `fetch_many` is about running several statements in one prepared
    // statement; a script is split into single statements before it gets here.
    #[allow(deprecated)]
    let mut results = query.fetch_many(pool);
    let mut rows = Vec::new();
    let mut affected: Option<u64> = None;
    while let Some(result) = results.next().await {
        match result? {
            Either::Left(done) => {
                *affected.get_or_insert(0) += rows_affected(&done);
            }
            Either::Right(row) => rows.push(row),
        }
    }
    drop(results);
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
        affected,
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
