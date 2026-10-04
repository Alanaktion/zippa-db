//! The connection a query tab runs on, held for as long as the tab is open.
//!
//! A pool hands each statement whichever connection is free, so a `BEGIN` run
//! in one go and the `UPDATE` run in the next can land on two different server
//! sessions — and the transaction the first one opened goes back into the pool
//! for a table view's edit to fall into. A query tab therefore checks one
//! connection out the first time it runs something and keeps it, the way
//! `psql` keeps one session: transactions, `SET`, `USE`, temporary tables and
//! `LOCK TABLES` all last from one run to the next. The pool is left for the
//! app's own reads and writes — the sidebar, table views, the structure tab.
//!
//! A pinned connection is never handed back to the pool: it is closed when the
//! tab lets it go, so whatever the tab left open is rolled back by the server
//! rather than inherited by someone else. A run that is cancelled part-way
//! closes it as well, since nothing can say what state a statement dropped in
//! the middle left behind; the next run checks out a fresh one.
//!
//! [`TxnState`] is whether the tab has a transaction open, read back after
//! every run so the tab can say so and the app can ask before throwing one
//! away. Postgres and SQLite are asked (see
//! [`Dedicated::transaction_status`]); MySQL offers nothing to ask that sqlx
//! exposes, so its state is followed from the statements themselves — its own
//! transaction control, the statements that commit implicitly, and
//! `autocommit`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, anyhow};
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit};

use super::config::Engine;
use super::connection::{Connection, query_outcome};
use super::dedicated::Dedicated;
use super::query::QueryResult;
use super::query_log::{QueryOutcome, QuerySource};
use super::{runtime, script, statement};

/// Whether a pinned connection is inside a transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TxnState {
    /// Every statement commits as it runs.
    #[default]
    Idle,
    /// A transaction is open: nothing since it began is visible elsewhere,
    /// and closing the tab would roll it back.
    Open,
    /// Postgres only: a statement failed inside the transaction, and the
    /// server ignores everything but `ROLLBACK` until it ends.
    Failed,
}

impl TxnState {
    /// Whether there is a transaction to commit or roll back.
    pub fn is_open(self) -> bool {
        self != TxnState::Idle
    }
}

/// The connection a query tab runs on; see the module docs.
///
/// Cheap to clone: every clone is the same connection. Nothing is checked out
/// until the first run.
#[derive(Clone)]
pub struct PinnedConnection {
    connection: Arc<Connection>,
    shared: Arc<Shared>,
}

struct Shared {
    held: Arc<tokio::sync::Mutex<Held>>,
    /// Kept outside `held`, so the UI can read it while a run holds the lock.
    tracker: Mutex<Tracker>,
    /// The server's id for the session, for a cancel sent from elsewhere.
    backend: Mutex<Option<u64>>,
}

/// The checked-out connection, and the pool slot it takes up.
#[derive(Default)]
pub(crate) struct Held {
    session: Option<(Dedicated, OwnedSemaphorePermit)>,
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let_go(session);
        }
    }
}

/// What is known about the session's transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tracker {
    state: TxnState,
    /// MySQL only: `autocommit` was turned off, so every statement opens a
    /// transaction if one is not already open.
    manual: bool,
}

impl PinnedConnection {
    pub fn new(connection: Arc<Connection>) -> Self {
        Self {
            connection,
            shared: Arc::new(Shared {
                held: Arc::new(tokio::sync::Mutex::new(Held::default())),
                tracker: Mutex::new(Tracker::default()),
                backend: Mutex::new(None),
            }),
        }
    }

    /// The connection this one was checked out of.
    pub fn connection(&self) -> &Arc<Connection> {
        &self.connection
    }

    /// Whether the tab has a transaction open, as of the last run.
    pub fn state(&self) -> TxnState {
        self.shared
            .tracker
            .lock()
            .map(|tracker| tracker.state)
            .unwrap_or_default()
    }

    /// Whether a run holds the connection right now.
    pub fn is_busy(&self) -> bool {
        self.shared.held.try_lock().is_err()
    }

    /// Run one statement the user typed and read back its rows, logged for
    /// the console as theirs.
    ///
    /// A read-only connection refuses a write here exactly as it does on the
    /// pool, before anything is checked out.
    pub async fn run_query(&self, sql: &str) -> Result<QueryResult> {
        self.connection.refuse_write(sql)?;
        let started = Instant::now();
        let result = self.fetch(sql).await;
        let (elapsed, outcome) = match &result {
            Ok(result) => (result.elapsed, query_outcome(result)),
            Err(error) => (started.elapsed(), QueryOutcome::Error(format!("{error:#}"))),
        };
        self.connection
            .log(sql, QuerySource::User, elapsed, outcome);
        result
    }

    async fn fetch(&self, sql: &str) -> Result<QueryResult> {
        let mut guard = self.lock().await?;
        let result = guard.fetch(sql).await;
        guard.note(sql, result.as_ref().err());
        guard.settle().await;
        result
    }

    /// Take the connection for a run, checking one out of the pool if the tab
    /// does not hold one yet.
    ///
    /// A tab runs one thing at a time — the session refuses a second run
    /// while one is in flight or a script is paused — so finding the lock
    /// taken is an error rather than something to wait for.
    pub(crate) async fn lock(&self) -> Result<PinGuard> {
        let mut held = self
            .shared
            .held
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow!("this tab is still running something"))?;

        if held.session.is_none() {
            let permit = self.connection.pinned_permit()?;
            let mut session = self.connection.dedicated().await?;
            let backend = session.backend_id().await.ok().flatten();
            if let Ok(mut slot) = self.shared.backend.lock() {
                *slot = backend;
            }
            if let Ok(mut tracker) = self.shared.tracker.lock() {
                *tracker = Tracker::default();
            }
            held.session = Some((session, permit));
        }

        Ok(PinGuard {
            held,
            shared: self.shared.clone(),
            engine: self.connection.config.engine,
        })
    }

    /// Ask the server to stop whatever this tab is running, from a pooled
    /// connection — the pinned one is busy running it.
    ///
    /// Best-effort and fire-and-forget: the run is abandoned on this side
    /// regardless, which closes the pinned connection, so this only spares
    /// the server finishing a statement no one is waiting for. SQLite has no
    /// server to tell.
    pub fn cancel(&self) {
        let backend = self.shared.backend.lock().ok().and_then(|slot| *slot);
        let Some(backend) = backend else {
            return;
        };
        let connection = self.connection.clone();
        drop(runtime::spawn(async move {
            connection.cancel_backend(backend).await.ok();
        }));
    }
}

/// The pinned connection, held for one run.
pub(crate) struct PinGuard {
    held: OwnedMutexGuard<Held>,
    shared: Arc<Shared>,
    engine: Engine,
}

impl PinGuard {
    /// The connection itself.
    pub(crate) fn session(&mut self) -> Result<&mut Dedicated> {
        self.held
            .session
            .as_mut()
            .map(|(session, _)| session)
            .ok_or_else(|| anyhow!("the tab's connection was closed"))
    }

    /// Run one statement and read back its rows.
    ///
    /// Should the run be dropped before the statement comes back — the user
    /// cancelling it — the connection is closed rather than kept: nothing
    /// says where the statement got to, or what it left open.
    pub(crate) async fn fetch(&mut self, sql: &str) -> Result<QueryResult> {
        let mut flight = Flight {
            guard: self,
            landed: false,
        };
        let result = flight.guard.session()?.fetch(sql).await;
        flight.landed = true;
        result
    }

    /// Run one statement, discarding what it returns. Dropped part-way, it
    /// closes the connection the way [`Self::fetch`] does.
    pub(crate) async fn execute(&mut self, sql: &str) -> Result<()> {
        let mut flight = Flight {
            guard: self,
            landed: false,
        };
        let result = flight.guard.session()?.execute(sql).await;
        flight.landed = true;
        result
    }

    /// Follow what one statement did to the transaction, where the server
    /// cannot simply be asked afterwards (MySQL).
    pub(crate) fn note(&mut self, sql: &str, error: Option<&anyhow::Error>) {
        if self.engine != Engine::MySql {
            return;
        }
        if let Ok(mut tracker) = self.shared.tracker.lock() {
            *tracker = mysql_after(*tracker, sql, error.and_then(mysql_error_number));
        }
    }

    /// Read the transaction state back from the server, where it can be
    /// asked (Postgres and SQLite).
    ///
    /// A connection that cannot even answer is closed, the same as one a
    /// cancel left behind: the next run starts on a fresh one.
    pub(crate) async fn settle(&mut self) {
        let status = match self.session() {
            Ok(session) => session.transaction_status().await,
            Err(_) => return,
        };
        match status {
            Ok(Some(state)) => {
                if let Ok(mut tracker) = self.shared.tracker.lock() {
                    tracker.state = state;
                }
            }
            Ok(None) => {}
            Err(_) => self.discard(),
        }
    }

    /// Close the connection, rolling back whatever it had open; the next run
    /// checks out a fresh one.
    pub(crate) fn discard(&mut self) {
        if let Some(session) = self.held.session.take() {
            let_go(session);
        }
        if let Ok(mut tracker) = self.shared.tracker.lock() {
            *tracker = Tracker::default();
        }
        if let Ok(mut backend) = self.shared.backend.lock() {
            *backend = None;
        }
    }

    /// The transaction state as of the last statement.
    pub(crate) fn state(&self) -> TxnState {
        self.shared
            .tracker
            .lock()
            .map(|tracker| tracker.state)
            .unwrap_or_default()
    }
}

/// One statement on its way to the server: if it is dropped before it lands,
/// the connection goes with it.
struct Flight<'a> {
    guard: &'a mut PinGuard,
    landed: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if !self.landed {
            self.guard.discard();
        }
    }
}

/// Let a checked-out connection go, on the database runtime: closing one
/// needs a Tokio context, and the last handle on a tab's connection is
/// usually dropped on the UI thread.
fn let_go(session: (Dedicated, OwnedSemaphorePermit)) {
    if tokio::runtime::Handle::try_current().is_ok() {
        drop(session);
    } else {
        drop(runtime::spawn(async move { drop(session) }));
    }
}

/// MySQL's error number, for the one error that ends a transaction on its own.
fn mysql_error_number(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<sqlx::Error>()
        .and_then(|error| error.as_database_error())
        .and_then(|error| error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .map(|error| error.number())
}

/// MySQL's deadlock error, which rolls the whole transaction back.
const MYSQL_DEADLOCK: u16 = 1213;

/// What one statement left MySQL's transaction state as, worked out from the
/// statement and whether it failed (`error` is MySQL's error number).
///
/// MySQL ends a transaction implicitly before most DDL, `LOCK TABLES`, and
/// the rest of the list `script::transaction_blocker` keeps, whether or not
/// the statement then succeeds. With `autocommit` off every statement opens a
/// transaction if none is open, and a `COMMIT` only ends the current one.
fn mysql_after(tracker: Tracker, sql: &str, error: Option<u16>) -> Tracker {
    let words = statement::leading_words(sql, 6);
    let word = |index: usize| words.get(index).map(String::as_str).unwrap_or("");
    let ok = error.is_none();
    let open = Tracker {
        state: TxnState::Open,
        ..tracker
    };
    let idle = Tracker {
        state: TxnState::Idle,
        ..tracker
    };

    if error == Some(MYSQL_DEADLOCK) {
        return idle;
    }

    match word(0) {
        "BEGIN" if !ok => tracker,
        "BEGIN" => open,
        "START" if word(1) == "TRANSACTION" => {
            if ok {
                open
            } else {
                tracker
            }
        }
        "COMMIT" | "ROLLBACK" if words.iter().any(|word| word == "TO") => tracker,
        "COMMIT" | "ROLLBACK" if !ok => tracker,
        // `AND CHAIN` starts the next transaction straight away.
        "COMMIT" | "ROLLBACK" if chains(&words) => open,
        "COMMIT" | "ROLLBACK" => idle,
        "SET" if words.iter().any(|word| word == "AUTOCOMMIT") => {
            if !ok {
                return tracker;
            }
            let at = words
                .iter()
                .position(|word| word == "AUTOCOMMIT")
                .unwrap_or_default();
            let value = match word(at + 1) {
                "=" => word(at + 2),
                value => value,
            };
            match value {
                // Turning it back on commits what was open.
                "1" | "ON" => Tracker {
                    state: TxnState::Idle,
                    manual: false,
                },
                "0" | "OFF" => Tracker {
                    manual: true,
                    ..tracker
                },
                _ => tracker,
            }
        }
        _ if script::implicitly_commits(Engine::MySql, &words) => {
            // The commit happens before the statement runs; with autocommit
            // off, a statement that then succeeds is in a new transaction —
            // but DDL commits after itself too, so it is left idle.
            idle
        }
        _ if ok && tracker.manual => open,
        _ => tracker,
    }
}

/// Whether a `COMMIT`/`ROLLBACK` carries `AND CHAIN` (and not `AND NO
/// CHAIN`), which opens the next transaction as it ends this one.
fn chains(words: &[String]) -> bool {
    words
        .iter()
        .position(|word| word == "CHAIN")
        .is_some_and(|at| at == 0 || words[at - 1] != "NO")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(steps: &[(&str, Option<u16>)]) -> TxnState {
        steps
            .iter()
            .fold(Tracker::default(), |tracker, (sql, error)| {
                mysql_after(tracker, sql, *error)
            })
            .state
    }

    #[test]
    fn mysql_transaction_control_opens_and_ends_one() {
        assert_eq!(after(&[("begin", None)]), TxnState::Open);
        assert_eq!(
            after(&[("start transaction read only", None)]),
            TxnState::Open
        );
        assert_eq!(after(&[("begin", None), ("commit", None)]), TxnState::Idle);
        assert_eq!(
            after(&[("begin", None), ("rollback", None)]),
            TxnState::Idle
        );
        assert_eq!(
            after(&[
                ("begin", None),
                ("savepoint a", None),
                ("rollback to a", None)
            ]),
            TxnState::Open
        );
        assert_eq!(
            after(&[("begin", None), ("commit and chain", None)]),
            TxnState::Open
        );
        assert_eq!(
            after(&[("begin", None), ("commit and no chain", None)]),
            TxnState::Idle
        );
        assert_eq!(
            after(&[
                ("begin", None),
                ("commit work and no chain no release", None)
            ]),
            TxnState::Idle
        );
        assert_eq!(
            after(&[("begin", None), ("commit work and chain no release", None)]),
            TxnState::Open
        );
        // A `BEGIN` the server refused opened nothing.
        assert_eq!(after(&[("begin", Some(1064))]), TxnState::Idle);
    }

    #[test]
    fn mysql_implicit_commits_end_a_transaction() {
        assert_eq!(
            after(&[("begin", None), ("create table t (a int)", None)]),
            TxnState::Idle
        );
        assert_eq!(
            after(&[("begin", None), ("lock tables t write", None)]),
            TxnState::Idle
        );
        // The commit comes first, so a DDL statement that fails still ends it.
        assert_eq!(
            after(&[("begin", None), ("alter table nope add b int", Some(1146))]),
            TxnState::Idle
        );
        // A temporary table does not commit.
        assert_eq!(
            after(&[("begin", None), ("create temporary table t (a int)", None)]),
            TxnState::Open
        );
    }

    #[test]
    fn mysql_errors_keep_a_transaction_open_except_a_deadlock() {
        assert_eq!(
            after(&[("begin", None), ("insert into nope values (1)", Some(1146))]),
            TxnState::Open
        );
        assert_eq!(
            after(&[
                ("begin", None),
                ("update t set a = 1", Some(MYSQL_DEADLOCK))
            ]),
            TxnState::Idle
        );
    }

    #[test]
    fn mysql_autocommit_off_opens_a_transaction_on_every_statement() {
        assert_eq!(after(&[("set autocommit = 0", None)]), TxnState::Idle);
        assert_eq!(
            after(&[("set @@autocommit = 0", None), ("select 1", None)]),
            TxnState::Open
        );
        assert_eq!(
            after(&[
                ("set autocommit = 0", None),
                ("insert into t values (1)", None)
            ]),
            TxnState::Open
        );
        assert_eq!(
            after(&[
                ("set autocommit = 0", None),
                ("insert into t values (1)", None),
                ("commit", None),
            ]),
            TxnState::Idle
        );
        assert_eq!(
            after(&[
                ("set autocommit = 0", None),
                ("insert into t values (1)", None),
                ("commit", None),
                ("select 1", None),
            ]),
            TxnState::Open
        );
        assert_eq!(
            after(&[
                ("set session autocommit = off", None),
                ("insert into t values (1)", None),
                ("set autocommit = 1", None),
                ("select 1", None),
            ]),
            TxnState::Idle
        );
    }

    #[test]
    fn plain_statements_leave_mysql_idle() {
        assert_eq!(
            after(&[("select 1", None), ("insert into t values (1)", None)]),
            TxnState::Idle
        );
    }
}
