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
//! must be dropped there — see `ScriptRun::close`.

use anyhow::Result;
use sqlx::AssertSqlSafe;
use sqlx::pool::PoolConnection;

use super::config::Engine;
use super::connection::fetch_on;
use super::query::QueryResult;
use super::{mysql, postgres, sqlite};

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
    pub(crate) async fn execute(&mut self, sql: &str) -> Result<()> {
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
                fetch_on::<sqlx::MySql, _>(
                    &mut **connection,
                    sql,
                    mysql::cell,
                    mysql::rows_affected,
                )
                .await
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
