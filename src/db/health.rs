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

/// The trouble one sqlx error stands for.
fn classify(error: &sqlx::Error) -> Option<ConnectionTrouble> {
    match error {
        sqlx::Error::Io(_) | sqlx::Error::WorkerCrashed => Some(ConnectionTrouble::Lost),
        sqlx::Error::PoolTimedOut => Some(ConnectionTrouble::TimedOut),
        sqlx::Error::PoolClosed => Some(ConnectionTrouble::Closed),
        // Postgres says so before it hangs up: `admin_shutdown` (the server
        // is stopping, or the backend was terminated), `crash_shutdown`, and
        // `cannot_connect_now` (it is still starting up).
        sqlx::Error::Database(error) => {
            matches!(error.code().as_deref(), Some("57P01" | "57P02" | "57P03"))
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
}
