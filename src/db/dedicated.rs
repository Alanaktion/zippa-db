//! One connection checked out of a pool for the length of a longer job.
//!
//! An import and a script run both need the session state they set up — `SET`,
//! `USE`, `PRAGMA`, temporary tables, an open transaction — to last from one
//! statement to the next, which a pool handing out whichever connection is free
//! cannot promise. The connection is marked to close rather than go back to the
//! pool, so a job that is cancelled partway cannot hand a half-open transaction
//! to the next caller: the server rolls it back when the connection goes.
//!
//! Dropping one closes the connection on the Tokio runtime, so a [`Dedicated`]
//! must be dropped there — see `ScriptRun::close` and `pinned::let_go`.
//!
//! A query tab holds one for as long as it is open, behind a
//! [`PinnedConnection`](super::PinnedConnection), so a `BEGIN` in one run and
//! the `COMMIT` in the next reach the same server session.

use anyhow::Result;
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Row as _};

use super::config::Engine;
use super::connection::fetch_on;
use super::pinned::TxnState;
use super::query::QueryResult;
use super::{health, mysql, postgres, sqlite};

/// A dedicated connection to one engine.
pub(crate) enum Dedicated {
    /// The connection, and the `money` scale its pool probed — see
    /// [`postgres::money_scale`].
    Postgres(PoolConnection<sqlx::Postgres>, i64),
    MySql(PoolConnection<sqlx::MySql>),
    Sqlite(PoolConnection<sqlx::Sqlite>),
}

impl Dedicated {
    pub(crate) async fn postgres(pool: &sqlx::PgPool, money_scale: i64) -> Result<Self> {
        Ok(Dedicated::Postgres(checkout(pool).await?, money_scale))
    }

    pub(crate) async fn mysql(pool: &sqlx::MySqlPool) -> Result<Self> {
        Ok(Dedicated::MySql(checkout(pool).await?))
    }

    pub(crate) async fn sqlite(pool: &sqlx::SqlitePool) -> Result<Self> {
        Ok(Dedicated::Sqlite(checkout(pool).await?))
    }

    pub(crate) fn engine(&self) -> Engine {
        match self {
            Dedicated::Postgres(..) => Engine::Postgres,
            Dedicated::MySql(_) => Engine::MySql,
            Dedicated::Sqlite(_) => Engine::Sqlite,
        }
    }

    /// Run one statement, discarding whatever it returns.
    ///
    /// MySQL gets the text protocol rather than a prepared statement: the
    /// prepared protocol refuses `BEGIN`, `START TRANSACTION`, every
    /// `SAVEPOINT` form, `LOCK TABLES`, and `USE` (error 1295), which are
    /// exactly what a script's transaction and a `mysqldump` file send.
    pub(crate) async fn execute(&mut self, sql: &str) -> Result<()> {
        self.execute_raw(sql).await.map_err(health::plain)
    }

    async fn execute_raw(&mut self, sql: &str) -> Result<()> {
        // The statement is either the user's own or one of the job's fixed
        // transaction-control statements; nothing in it is bound. It goes by
        // the text protocol, since MySQL refuses `BEGIN` and `SAVEPOINT` over
        // the prepared-statement one (error 1295).
        let statement = AssertSqlSafe(sql.to_string());
        match self {
            Dedicated::Postgres(connection, _) => {
                sqlx::raw_sql(statement).execute(&mut **connection).await?;
            }
            Dedicated::MySql(connection) => {
                sqlx::raw_sql(statement).execute(&mut **connection).await?;
            }
            Dedicated::Sqlite(connection) => {
                sqlx::raw_sql(statement).execute(&mut **connection).await?;
            }
        }
        Ok(())
    }

    /// Run one statement and read back its rows, the way a query tab shows
    /// them.
    pub(crate) async fn fetch(&mut self, sql: &str) -> Result<QueryResult> {
        self.fetch_raw(sql).await.map_err(health::plain)
    }

    async fn fetch_raw(&mut self, sql: &str) -> Result<QueryResult> {
        match self {
            Dedicated::Postgres(connection, scale) => {
                let scale = *scale;
                fetch_on::<sqlx::Postgres, _>(
                    &mut **connection,
                    sql,
                    move |row, index| postgres::cell(row, index, scale),
                    postgres::rows_affected,
                )
                .await
            }
            Dedicated::MySql(connection) => {
                let fetched = fetch_on::<sqlx::MySql, _>(
                    &mut **connection,
                    sql,
                    mysql::cell,
                    mysql::rows_affected,
                )
                .await;
                match fetched {
                    // A statement the prepared protocol refuses (`LOCK
                    // TABLES`, `USE`, …) is refused before it runs, so it is
                    // safe to send again as text. None of them return rows.
                    Err(error) if mysql::unpreparable(&error) => {
                        let started = std::time::Instant::now();
                        let done = sqlx::raw_sql(AssertSqlSafe(sql.to_string()))
                            .execute(&mut **connection)
                            .await?;
                        Ok(QueryResult {
                            columns: Vec::new(),
                            column_types: Vec::new(),
                            rows: Vec::new(),
                            elapsed: started.elapsed(),
                            affected: Some(mysql::rows_affected(&done)),
                        })
                    }
                    fetched => fetched,
                }
            }
            Dedicated::Sqlite(connection) => {
                fetch_on::<sqlx::Sqlite, _>(
                    &mut **connection,
                    sql,
                    sqlite::cell,
                    sqlite::rows_affected,
                )
                .await
            }
        }
    }
}

impl Dedicated {
    /// The server's own id for this session — `pg_backend_pid()` or
    /// `CONNECTION_ID()` — which is what a cancel from another connection
    /// names. `None` on SQLite, which has no server to ask.
    pub(crate) async fn backend_id(&mut self) -> Result<Option<u64>> {
        let sql = match self {
            Dedicated::Postgres(..) => "SELECT pg_backend_pid()",
            Dedicated::MySql(_) => "SELECT CONNECTION_ID()",
            Dedicated::Sqlite(_) => return Ok(None),
        };
        let result = self.fetch(sql).await?;
        Ok(result
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(|cell| cell.as_deref())
            .and_then(|id| id.parse().ok()))
    }

    /// Whether the session is inside a transaction, as the server itself
    /// sees it — `None` where it cannot be asked and the caller has to work
    /// it out from the statements that ran (MySQL).
    ///
    /// Postgres has no function that answers this directly, but inside a
    /// transaction block `now()` is fixed at the transaction's start while
    /// `statement_timestamp()` moves on with each statement, and outside one
    /// the two are the same instant. A transaction an error has aborted
    /// refuses the question itself with `25P02`, which is the answer too.
    /// SQLite answers through `sqlite3_get_autocommit`.
    pub(crate) async fn transaction_status(&mut self) -> Result<Option<TxnState>> {
        match self {
            Dedicated::Postgres(connection, _) => {
                // The simple protocol, so the probe is one message: over the
                // extended protocol the statement's clock starts at a later
                // message than its implicit transaction's, and the two never
                // match even outside a transaction block.
                let probe = sqlx::raw_sql("SELECT now() <> statement_timestamp()")
                    .fetch_one(&mut **connection)
                    .await
                    .and_then(|row| row.try_get::<bool, _>(0));
                match probe {
                    Ok(true) => Ok(Some(TxnState::Open)),
                    Ok(false) => Ok(Some(TxnState::Idle)),
                    Err(error)
                        if error
                            .as_database_error()
                            .and_then(|error| error.code())
                            .is_some_and(|code| code == "25P02") =>
                    {
                        Ok(Some(TxnState::Failed))
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Dedicated::MySql(_) => Ok(None),
            Dedicated::Sqlite(connection) => {
                let mut handle = connection.lock_handle().await?;
                // SAFETY: the handle is locked for the length of the call, so
                // nothing else is using the connection, and
                // `sqlite3_get_autocommit` only reads a flag off it.
                let autocommit = unsafe {
                    libsqlite3_sys::sqlite3_get_autocommit(handle.as_raw_handle().as_ptr())
                };
                Ok(Some(if autocommit == 0 {
                    TxnState::Open
                } else {
                    TxnState::Idle
                }))
            }
        }
    }
}

/// Check one connection out of `pool` and mark it to close rather than return
/// to the pool.
async fn checkout<DB>(pool: &sqlx::Pool<DB>) -> Result<PoolConnection<DB>>
where
    DB: sqlx::Database,
{
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    Ok(connection)
}
