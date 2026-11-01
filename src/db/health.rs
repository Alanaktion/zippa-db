//! Telling a dropped or unreachable server apart from an ordinary SQL error.
//!
//! A query that fails because the network went away comes back from sqlx as
//! an I/O error ("Connection reset by peer", "unexpected end of file") or a
//! pool timeout, which says nothing about what the user can do next. The
//! shared entry points on [`Connection`](super::Connection) pass every error
//! through [`plain`], which swaps those for a [`ConnectionTrouble`] that says
//! what happened in words and points at Reconnect. Every other error — a
//! syntax error, a constraint, a refused write — goes through untouched.

use std::fmt;
use std::time::Duration;

/// How long a statement waits for one of the pool's connections before
/// giving up, rather than sqlx's 30 s default: long enough for a busy pool to
/// free one up, short enough that a server that has stopped answering is told
/// to the user while they are still looking.
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// What went wrong with the connection itself, as opposed to the SQL sent
/// over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionTrouble {
    /// The server closed the connection, or the network between went away.
    Lost,
    /// No connection came free within [`ACQUIRE_TIMEOUT`]: every one is busy
    /// with a running statement, or the server is not answering new ones.
    TimedOut,
    /// The pool was closed under the statement — the connection was switched
    /// or reconnected while it waited.
    Closed,
}

impl fmt::Display for ConnectionTrouble {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionTrouble::Lost => {
                f.write_str("The connection to the server was lost. Reconnect to open a fresh one.")
            }
            ConnectionTrouble::TimedOut => write!(
                f,
                "No connection to the server came free within {} seconds: every connection \
                 is busy with a running query, or the server has stopped answering. Cancel a \
                 running query, or Reconnect.",
                ACQUIRE_TIMEOUT.as_secs()
            ),
            ConnectionTrouble::Closed => {
                f.write_str("This connection was closed. Reconnect to open a fresh one.")
            }
        }
    }
}

impl std::error::Error for ConnectionTrouble {}

impl ConnectionTrouble {
    /// The trouble behind `error`, if any of its causes is one.
    pub fn of(error: &anyhow::Error) -> Option<Self> {
        error.chain().find_map(|cause| {
            if let Some(trouble) = cause.downcast_ref::<ConnectionTrouble>() {
                return Some(*trouble);
            }
            cause.downcast_ref::<sqlx::Error>().and_then(classify)
        })
    }
}

/// Put `error` in words when the connection is to blame, and leave it alone
/// otherwise.
///
/// The replacement carries no source, so `{error:#}` shows the sentence alone
/// rather than the sentence followed by the driver's own text.
pub(crate) fn plain(error: anyhow::Error) -> anyhow::Error {
    match ConnectionTrouble::of(&error) {
        Some(trouble) => anyhow::Error::new(trouble),
        None => error,
    }
}

/// `error` in the words the UI shows: each cause in turn, as `{error:#}`
/// would, except that a server's own error is said once, in the server's
/// words.
///
/// sqlx displays a server error as `error returned from database: …` and
/// also hands the same error back as its source, so `{error:#}` printed the
/// message twice behind driver wording. Here the chain stops at that error
/// and shows its message, its code (which is what a search needs), and on
/// Postgres the `DETAIL` and `HINT` lines.
pub fn describe(error: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in error.chain() {
        if let Some(sqlx::Error::Database(database)) = cause.downcast_ref::<sqlx::Error>() {
            parts.push(server_message(database.as_ref()));
            break;
        }
        let text = cause.to_string();
        // A cause that only repeats the one before it (a `context` naming its
        // own source, or a wrapper printing its inner error) says nothing new.
        if parts.last().is_some_and(|last| last.ends_with(&text)) {
            continue;
        }
        parts.push(text);
    }
    parts.join(": ")
}

/// One server error: the message, then the engine's code in brackets.
fn server_message(error: &dyn sqlx::error::DatabaseError) -> String {
    if let Some(postgres) = error.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
        let mut text = format!("{} (SQLSTATE {})", postgres.message(), postgres.code());
        if let Some(detail) = postgres.detail() {
            text.push_str(&format!("\nDetail: {detail}"));
        }
        if let Some(hint) = postgres.hint() {
            text.push_str(&format!("\nHint: {hint}"));
        }
        return text;
    }
    if let Some(mysql) = error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
        return format!("{} (MySQL error {})", mysql.message(), mysql.number());
    }
    if error
        .try_downcast_ref::<sqlx::sqlite::SqliteError>()
        .is_some()
    {
        return match error.code() {
            Some(code) => format!("{} (SQLite error {code})", error.message()),
            None => error.message().to_string(),
        };
    }
    match error.code() {
        Some(code) => format!("{} ({code})", error.message()),
        None => error.message().to_string(),
    }
}

/// MySQL's `ER_CLIENT_INTERACTION_TIMEOUT`: the server closed a session that
/// sat idle past `wait_timeout`.
const MYSQL_IDLE_DISCONNECT: u16 = 4031;

/// The trouble one sqlx error stands for.
fn classify(error: &sqlx::Error) -> Option<ConnectionTrouble> {
    match error {
        sqlx::Error::Io(_) | sqlx::Error::WorkerCrashed => Some(ConnectionTrouble::Lost),
        sqlx::Error::PoolTimedOut => Some(ConnectionTrouble::TimedOut),
        sqlx::Error::PoolClosed => Some(ConnectionTrouble::Closed),
        // Postgres says so before it hangs up: `admin_shutdown` (the server
        // is stopping, or the backend was terminated), `crash_shutdown`,
        // `cannot_connect_now` (it is still starting up), `database_dropped`,
        // `idle_session_timeout`, and `idle_in_transaction_session_timeout`.
        // MySQL 8.0.24 and later send 4031 before closing an idle session.
        sqlx::Error::Database(error) => {
            let mysql = error
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .map(|error| error.number());
            (mysql == Some(MYSQL_IDLE_DISCONNECT)
                || matches!(
                    error.code().as_deref(),
                    Some("57P01" | "57P02" | "57P03" | "57P04" | "57P05" | "25P03")
                ) && mysql.is_none())
            .then_some(ConnectionTrouble::Lost)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(kind: std::io::ErrorKind) -> anyhow::Error {
        anyhow::Error::new(sqlx::Error::Io(std::io::Error::new(kind, "driver text")))
    }

    #[test]
    fn a_dropped_socket_reads_as_a_lost_connection() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let error = plain(io(kind));
            assert_eq!(ConnectionTrouble::of(&error), Some(ConnectionTrouble::Lost));
            let shown = format!("{error:#}");
            assert!(
                shown.contains("connection to the server was lost"),
                "{shown}"
            );
            assert!(!shown.contains("driver text"), "{shown}");
        }
    }

    #[test]
    fn a_pool_timeout_names_the_wait_and_what_to_do() {
        let shown = format!("{:#}", plain(sqlx::Error::PoolTimedOut.into()));
        assert!(shown.contains("10 seconds"), "{shown}");
        assert!(shown.contains("Reconnect"), "{shown}");
    }

    #[test]
    fn trouble_is_found_under_added_context() {
        let error = io(std::io::ErrorKind::ConnectionReset).context("statement 2 of 3");
        assert_eq!(ConnectionTrouble::of(&error), Some(ConnectionTrouble::Lost));
    }

    #[test]
    fn an_ordinary_error_is_left_alone() {
        let error = plain(anyhow::anyhow!("syntax error at or near \"SELEC\""));
        assert_eq!(ConnectionTrouble::of(&error), None);
        assert_eq!(format!("{error:#}"), "syntax error at or near \"SELEC\"");
        let error = plain(sqlx::Error::RowNotFound.into());
        assert_eq!(ConnectionTrouble::of(&error), None);
    }
    #[test]
    fn a_server_error_is_said_once_without_driver_wording() {
        let error = anyhow::Error::new(sqlx::Error::Protocol("unused".into()));
        // A non-server sqlx error keeps its own text.
        assert_eq!(describe(&error), format!("{error:#}"));

        let error = anyhow::anyhow!("no such table: runnerz").context("statement 2 of 2");
        assert_eq!(describe(&error), "statement 2 of 2: no such table: runnerz");
    }

    #[test]
    fn a_sqlite_error_shows_its_message_and_code_once() {
        let error = crate::db::runtime::block_on(async move {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .connect("sqlite::memory:")
                .await
                .expect("an in-memory pool");
            sqlx::query("SELECT * FROM runnerz")
                .fetch_all(&pool)
                .await
                .expect_err("the table does not exist")
        });
        let error = anyhow::Error::new(error);
        assert_eq!(describe(&error), "no such table: runnerz (SQLite error 1)");
        let error = error.context("statement 2 of 2");
        assert_eq!(
            describe(&error),
            "statement 2 of 2: no such table: runnerz (SQLite error 1)"
        );
    }
}
