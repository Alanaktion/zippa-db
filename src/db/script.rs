//! Running every statement in a query buffer, one at a time.
//!
//! A script runs on its tab's [`PinnedConnection`], either inside a transaction
//! ([`ScriptMode::Transaction`]) or with each statement committing on its own
//! ([`ScriptMode::Autocommit`]). A statement that fails either pauses the run
//! for the user to answer ([`OnFailure::Ask`]) or is skipped and reported at
//! the end ([`OnFailure::Skip`]).
//!
//! The run is driven a step at a time rather than awaited whole: [`ScriptRun::
//! start`] runs until the end or the first failure that needs an answer, and
//! hands back a [`Step::Paused`] holding the run itself. The UI asks the
//! question and passes the answer to [`ScriptRun::resume`] as another task.
//! Nothing inside the database runtime ever waits on the UI, so a run left
//! paused holds the tab's connection and nothing else — and the test runtime,
//! which runs each task inline, drives it the same way.
//!
//! Inside a transaction every statement runs under a savepoint, so a failed
//! one can be undone on its own and the rest carried on with: Postgres aborts
//! the whole transaction at the first error otherwise. [`transaction_blocker`]
//! names the statements that cannot live inside a transaction at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use super::config::Engine;
use super::connection::{Connection, query_outcome};
use super::import::excerpt;
use super::pinned::{PinGuard, PinnedConnection, TxnState};
use super::query::QueryResult;
use super::query_log::{QueryOutcome, QuerySource};
use super::statement::{self, Statement};

/// The savepoint each statement runs under inside a transaction.
const SAVEPOINT: &str = "zippa_script";

/// The savepoint a script's transaction is, inside one the tab already had
/// open.
const OUTER_SAVEPOINT: &str = "zippa_script_run";

/// Whether the script is wrapped in one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptMode {
    /// Everything commits together at the end, or not at all.
    Transaction,
    /// Each statement commits as it runs.
    Autocommit,
}

/// What a failed statement does to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFailure {
    /// Pause and ask.
    Ask,
    /// Skip it and carry on; it is reported at the end.
    Skip,
}

/// The answer to a paused run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Leave the failed statement out and carry on.
    Skip,
    /// The same, and skip every later failure without asking.
    SkipAll,
    /// Roll the transaction back, or, without one, stop where it is.
    Abort,
}

/// A statement that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFailure {
    /// Zero-based position in the script.
    pub index: usize,
    /// How many statements the script holds.
    pub total: usize,
    /// One-based line of the buffer the statement starts on.
    pub line: usize,
    /// The statement, flattened to one line and shortened.
    pub statement: String,
    pub message: String,
}

impl ScriptFailure {
    /// `"Statement 3 of 12 (line 14)"`.
    pub fn position(&self) -> String {
        format!(
            "Statement {} of {} (line {})",
            self.index + 1,
            self.total,
            self.line
        )
    }
}

/// How a run went, once it is over.
#[derive(Debug, Clone)]
pub struct ScriptOutcome {
    /// One result per statement that ran, in order.
    pub results: Vec<QueryResult>,
    /// Every statement that failed, in order.
    pub failures: Vec<ScriptFailure>,
    pub total: usize,
    pub mode: ScriptMode,
    /// The transaction was rolled back, so nothing the script did was kept.
    pub rolled_back: bool,
    /// The run was stopped at a failure, without a transaction to undo what
    /// ran before it.
    pub stopped: bool,
    pub elapsed: Duration,
}

impl ScriptOutcome {
    /// Whether any of the script's changes were kept.
    pub fn applied_anything(&self) -> bool {
        !self.rolled_back && !self.results.is_empty()
    }

    /// What the status bar says about a run that did not simply succeed;
    /// `None` when every statement ran.
    pub fn summary(&self) -> Option<String> {
        let last = self.failures.last()?;
        let at = last.index + 1;
        let total = self.total;

        if self.rolled_back {
            return Some(format!(
                "Rolled back: statement {at} of {total} failed, so nothing was applied"
            ));
        }
        if self.stopped {
            let before = self.results.len();
            let applied = match before {
                0 => "nothing before it ran".to_string(),
                1 => "the 1 statement that ran before it was applied".to_string(),
                count => format!("the {count} statements that ran before it were applied"),
            };
            return Some(format!("Stopped at statement {at} of {total}; {applied}"));
        }

        let failed = self.failures.len();
        let unit = if failed == 1 {
            "statement"
        } else {
            "statements"
        };
        let kept = match self.mode {
            ScriptMode::Transaction => " and the rest committed",
            ScriptMode::Autocommit => "",
        };
        Some(format!(
            "Ran {total} statements in {} ms · {failed} {unit} failed and skipped{kept}",
            self.elapsed.as_millis()
        ))
    }

    /// The failures laid out as a result of their own, for the grid.
    pub fn failures_result(&self) -> Option<QueryResult> {
        if self.failures.is_empty() {
            return None;
        }

        let text = |value: String| Some(value);
        Some(QueryResult {
            columns: ["#", "line", "statement", "error"]
                .map(String::from)
                .to_vec(),
            column_types: ["INTEGER", "INTEGER", "TEXT", "TEXT"]
                .map(String::from)
                .to_vec(),
            rows: self
                .failures
                .iter()
                .map(|failure| {
                    vec![
                        text((failure.index + 1).to_string()),
                        text(failure.line.to_string()),
                        text(failure.statement.clone()),
                        text(failure.message.clone()),
                    ]
                })
                .collect(),
            elapsed: Duration::ZERO,
            affected: None,
        })
    }
}

/// Where a run is now.
pub enum Step {
    /// A statement failed and the run is waiting for an answer.
    /// Boxed: a run is much the larger of the two.
    Paused(Box<ScriptRun>, ScriptFailure),
    Finished(ScriptOutcome),
}

/// A script part-way through.
///
/// Holds the tab's pinned connection for the length of the run, and — inside
/// a transaction — everything the script has done so far. A run given up on
/// while paused rolls its transaction back through [`ScriptRun::close`], on
/// the database runtime; one dropped mid-statement closes the connection,
/// which rolls back whatever it had open.
pub struct ScriptRun {
    connection: Arc<Connection>,
    /// `None` once the run is over and the connection has been let go.
    session: Option<PinGuard>,
    /// The tab already had a transaction open, so the script's own is a
    /// savepoint inside it rather than a transaction of its own: a `BEGIN`
    /// there would be refused (SQLite), ignored with a warning (Postgres), or
    /// would commit the tab's transaction (MySQL).
    nested: bool,
    statements: Vec<Statement>,
    lines: Vec<usize>,
    next: usize,
    mode: ScriptMode,
    on_failure: OnFailure,
    results: Vec<QueryResult>,
    failures: Vec<ScriptFailure>,
    /// The failure the run is paused on.
    pending: Option<ScriptFailure>,
    started: Instant,
}

impl ScriptRun {
    /// Start running every statement in `sql` on a tab's pinned connection.
    ///
    /// A read-only connection refuses the script before anything runs, naming
    /// the first statement that writes.
    pub async fn start(
        pinned: PinnedConnection,
        sql: &str,
        mode: ScriptMode,
        on_failure: OnFailure,
    ) -> Result<Step> {
        let connection = pinned.connection().clone();
        let statements = statement::split(sql);
        for statement in &statements {
            connection.refuse_write(&statement.text)?;
        }
        let lines = statements
            .iter()
            .map(|statement| sql[..statement.start].matches('\n').count() + 1)
            .collect();

        let mut session = pinned.lock().await?;
        let nested = mode == ScriptMode::Transaction && session.state().is_open();
        if mode == ScriptMode::Transaction {
            if session.state() == TxnState::Failed {
                anyhow::bail!(
                    "the tab's transaction has failed; roll it back before running a script"
                );
            }
            let begin = if nested {
                format!("SAVEPOINT {OUTER_SAVEPOINT}")
            } else {
                "BEGIN".to_string()
            };
            session
                .execute(&begin)
                .await
                .context("could not start the script's transaction")?;
        }

        let run = ScriptRun {
            connection,
            session: Some(session),
            nested,
            statements,
            lines,
            next: 0,
            mode,
            on_failure,
            results: Vec::new(),
            failures: Vec::new(),
            pending: None,
            started: Instant::now(),
        };
        run.advance().await
    }

    /// Carry on from the failure the run is paused on.
    pub async fn resume(mut self, decision: Decision) -> Result<Step> {
        if let Some(failure) = self.pending.take() {
            self.failures.push(failure);
        }

        match decision {
            Decision::Skip => self.advance().await,
            Decision::SkipAll => {
                self.on_failure = OnFailure::Skip;
                self.advance().await
            }
            Decision::Abort => {
                let rolled_back = self.mode == ScriptMode::Transaction;
                if rolled_back {
                    self.roll_back().await;
                }
                Ok(Step::Finished(self.finish(rolled_back, !rolled_back).await))
            }
        }
    }

    /// Give up on a paused run, rolling back whatever transaction the script
    /// opened. Statements that ran outside one stay applied, as they would
    /// have had the run been stopped.
    pub async fn close(mut self) {
        if self.mode == ScriptMode::Transaction {
            self.roll_back().await;
        }
        self.finish(true, true).await;
    }

    pub fn mode(&self) -> ScriptMode {
        self.mode
    }

    /// Undo the script's transaction — or its savepoint, inside the tab's
    /// own. Best-effort: a connection that cannot even roll back is closed,
    /// which aborts everything it had open on its own.
    async fn roll_back(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let undone = if self.nested {
            match session
                .execute(&format!("ROLLBACK TO SAVEPOINT {OUTER_SAVEPOINT}"))
                .await
            {
                Ok(()) => {
                    session
                        .execute(&format!("RELEASE SAVEPOINT {OUTER_SAVEPOINT}"))
                        .await
                }
                Err(error) => Err(error),
            }
        } else {
            session.execute("ROLLBACK").await
        };
        if undone.is_err() {
            session.discard();
        }
    }

    /// Run statements until the end, or until one fails and the user is to
    /// be asked.
    async fn advance(mut self) -> Result<Step> {
        while self.next < self.statements.len() {
            let index = self.next;
            self.next += 1;

            match self.run_one(index).await? {
                Ok(result) => self.results.push(result),
                Err(message) => {
                    let failure = ScriptFailure {
                        index,
                        total: self.statements.len(),
                        line: self.lines[index],
                        statement: excerpt(&self.statements[index].text),
                        message,
                    };
                    match self.on_failure {
                        OnFailure::Skip => self.failures.push(failure),
                        OnFailure::Ask => {
                            self.pending = Some(failure.clone());
                            // The tab shows what is open while it asks.
                            self.session_mut()?.settle().await;
                            return Ok(Step::Paused(Box::new(self), failure));
                        }
                    }
                }
            }
        }

        if self.mode == ScriptMode::Transaction {
            let commit = if self.nested {
                format!("RELEASE SAVEPOINT {OUTER_SAVEPOINT}")
            } else {
                "COMMIT".to_string()
            };
            self.session_mut()?
                .execute(&commit)
                .await
                .context("could not commit the script")?;
        }
        Ok(Step::Finished(self.finish(false, false).await))
    }

    /// Run one statement, logging it for the console.
    ///
    /// The inner error is the statement's own failure, which the run answers;
    /// the outer one is the connection or the transaction going wrong under
    /// it, which ends the run.
    async fn run_one(&mut self, index: usize) -> Result<Result<QueryResult, String>> {
        let transaction = self.mode == ScriptMode::Transaction;
        let sql = self.statements[index].text.clone();
        let session = self.session.as_mut().context(CLOSED)?;

        if transaction {
            session.execute(&format!("SAVEPOINT {SAVEPOINT}")).await?;
        }

        let started = Instant::now();
        let result = session.fetch(&sql).await;
        let elapsed = started.elapsed();
        // Without a transaction of the script's own, the statement may be the
        // user's own `BEGIN` or `COMMIT`, which the tab carries on with.
        session.note(&sql, result.as_ref().err());

        let outcome = match result {
            Ok(result) => {
                if transaction {
                    session
                        .execute(&format!("RELEASE SAVEPOINT {SAVEPOINT}"))
                        .await?;
                }
                self.connection.log(
                    &sql,
                    QuerySource::User,
                    result.elapsed,
                    query_outcome(&result),
                );
                Ok(result)
            }
            Err(error) => {
                let message = format!("{error:#}");
                if transaction {
                    session
                        .execute(&format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}"))
                        .await
                        .context("could not undo the failed statement")?;
                    session
                        .execute(&format!("RELEASE SAVEPOINT {SAVEPOINT}"))
                        .await?;
                }
                self.connection.log(
                    &sql,
                    QuerySource::User,
                    elapsed,
                    QueryOutcome::Error(message.clone()),
                );
                Err(message)
            }
        };
        Ok(outcome)
    }

    fn session_mut(&mut self) -> Result<&mut PinGuard> {
        self.session.as_mut().context(CLOSED)
    }

    /// Let the connection go back to the tab and say how the run went.
    async fn finish(&mut self, rolled_back: bool, stopped: bool) -> ScriptOutcome {
        if let Some(mut session) = self.session.take() {
            session.settle().await;
        }
        ScriptOutcome {
            results: std::mem::take(&mut self.results),
            failures: std::mem::take(&mut self.failures),
            total: self.statements.len(),
            mode: self.mode,
            rolled_back,
            stopped,
            elapsed: self.started.elapsed(),
        }
    }
}

impl Drop for ScriptRun {
    /// A run dropped part-way — cancelled while a statement was out — closes
    /// the tab's connection rather than leave the script's transaction open
    /// on it for the tab's next run to fall into.
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.discard();
        }
    }
}

/// The error a run whose connection is already gone ends with.
const CLOSED: &str = "the script's connection has already been closed";

/// A statement that cannot run inside the transaction a script would wrap
/// itself in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    /// Zero-based position in the script.
    pub index: usize,
    /// The words that name it, e.g. `LOCK TABLES`.
    pub words: String,
}

impl Blocker {
    /// `"LOCK TABLES (statement 4) cannot run inside a transaction"`.
    pub fn message(&self) -> String {
        format!(
            "{} (statement {}) cannot run inside a transaction",
            self.words,
            self.index + 1
        )
    }
}

/// The first statement in `statements` that a wrapping transaction would
/// break, or that would break it.
///
/// That is the buffer's own transaction control on every engine, MySQL's
/// statements that commit implicitly (its DDL among them, which is why a
/// MySQL migration usually runs without one), and the handful of Postgres and
/// SQLite statements that refuse to run inside a transaction or quietly do
/// nothing there.
pub fn transaction_blocker(engine: Engine, statements: &[Statement]) -> Option<Blocker> {
    statements
        .iter()
        .enumerate()
        .find_map(|(index, statement)| {
            let words = statement::leading_words(&statement.text, 6);
            let shown = blocks(engine, &words)?;
            Some(Blocker {
                index,
                words: words[..shown.min(words.len())].join(" "),
            })
        })
}

/// Whether a statement, reduced to its upper-cased words, commits whatever
/// transaction is open before it runs — MySQL's DDL, `LOCK TABLES`, and the
/// rest of the list [`blocks`] keeps. Transaction control itself is left out:
/// it is followed on its own terms by `pinned`.
pub(crate) fn implicitly_commits(engine: Engine, words: &[String]) -> bool {
    let word = |index: usize| words.get(index).map(String::as_str).unwrap_or("");
    let control = matches!(word(0), "BEGIN" | "COMMIT" | "END" | "ABORT" | "ROLLBACK")
        || (word(0) == "START" && word(1) == "TRANSACTION");
    engine == Engine::MySql && !control && blocks(engine, words).is_some()
}

/// How many of `words` name the statement, when it is one a transaction
/// cannot hold.
fn blocks(engine: Engine, words: &[String]) -> Option<usize> {
    let word = |index: usize| words.get(index).map(String::as_str).unwrap_or("");
    let position = |needle: &str| words.iter().position(|word| word == needle);

    // Transaction control, everywhere. `ROLLBACK TO` a savepoint is fine.
    match word(0) {
        "BEGIN" | "COMMIT" | "END" | "ABORT" => return Some(1),
        "START" if word(1) == "TRANSACTION" => return Some(2),
        "ROLLBACK" if position("TO").is_none() => return Some(1),
        _ => {}
    }

    match engine {
        Engine::Postgres => match word(0) {
            "VACUUM" | "DISCARD" => Some(1),
            "CLUSTER" if words.len() == 1 => Some(1),
            "CREATE" | "DROP" if matches!(word(1), "DATABASE" | "TABLESPACE" | "SUBSCRIPTION") => {
                Some(2)
            }
            "ALTER" if word(1) == "SYSTEM" => Some(2),
            "CREATE" | "DROP" if position("INDEX").is_some() => {
                position("CONCURRENTLY").map(|at| at + 1)
            }
            "REINDEX" => position("CONCURRENTLY")
                .or_else(|| position("DATABASE"))
                .or_else(|| position("SYSTEM"))
                .map(|at| at + 1),
            _ => None,
        },
        Engine::MySql => match word(0) {
            "CREATE" | "DROP" if word(1) == "TEMPORARY" => None,
            "CREATE" | "DROP" | "ALTER" | "RENAME" => Some(2),
            "TRUNCATE" | "GRANT" | "REVOKE" | "ANALYZE" | "OPTIMIZE" | "REPAIR" | "CHECK"
            | "FLUSH" | "RESET" | "INSTALL" | "UNINSTALL" | "XA" | "CHANGE" | "STOP" => Some(1),
            "LOCK" | "UNLOCK" | "START" => Some(2),
            "CACHE" | "LOAD" if word(1) == "INDEX" => Some(2),
            "SET" if matches!(word(1), "PASSWORD" | "AUTOCOMMIT") => Some(2),
            _ => None,
        },
        Engine::Sqlite => match word(0) {
            "VACUUM" | "ATTACH" | "DETACH" => Some(1),
            // `PRAGMA main.foreign_keys` names the schema first.
            "PRAGMA" => words[1..words.len().min(3)]
                .iter()
                .position(|word| word == "FOREIGN_KEYS" || word == "JOURNAL_MODE")
                .map(|at| at + 2),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocker(engine: Engine, sql: &str) -> Option<String> {
        transaction_blocker(engine, &statement::split(sql)).map(|blocker| blocker.message())
    }

    #[test]
    fn plain_reads_and_writes_fit_in_a_transaction() {
        for engine in [Engine::Postgres, Engine::MySql, Engine::Sqlite] {
            let sql = "select 1; insert into t values (1); update t set a = 2; delete from t;";
            assert_eq!(blocker(engine, sql), None, "{engine:?}");
        }
    }

    #[test]
    fn the_buffers_own_transaction_control_blocks_everywhere() {
        for engine in [Engine::Postgres, Engine::MySql, Engine::Sqlite] {
            assert_eq!(
                blocker(engine, "insert into t values (1);\n-- go\nbegin;").as_deref(),
                Some("BEGIN (statement 2) cannot run inside a transaction"),
                "{engine:?}"
            );
            assert!(blocker(engine, "start transaction").is_some());
            assert!(blocker(engine, "commit").is_some());
            assert!(blocker(engine, "rollback").is_some());
            assert_eq!(blocker(engine, "rollback to savepoint a"), None);
        }
    }

    #[test]
    fn mysql_names_its_implicit_commits() {
        assert_eq!(
            blocker(Engine::MySql, "/* first */ lock tables t write").as_deref(),
            Some("LOCK TABLES (statement 1) cannot run inside a transaction")
        );
        assert!(blocker(Engine::MySql, "alter table t add column b int").is_some());
        assert!(blocker(Engine::MySql, "create table t (a int)").is_some());
        assert!(blocker(Engine::MySql, "truncate t").is_some());
        assert!(blocker(Engine::MySql, "set autocommit = 1").is_some());
        assert_eq!(
            blocker(Engine::MySql, "create temporary table t (a int)"),
            None
        );
        assert_eq!(blocker(Engine::MySql, "drop temporary table t"), None);
        assert_eq!(blocker(Engine::MySql, "set @a = 1"), None);
    }

    #[test]
    fn postgres_names_what_refuses_a_transaction() {
        assert_eq!(
            blocker(
                Engine::Postgres,
                "create unique index concurrently i on t (a)"
            )
            .as_deref(),
            Some("CREATE UNIQUE INDEX CONCURRENTLY (statement 1) cannot run inside a transaction")
        );
        assert!(blocker(Engine::Postgres, "vacuum t").is_some());
        assert!(blocker(Engine::Postgres, "create database d").is_some());
        assert!(blocker(Engine::Postgres, "alter system set work_mem = '8MB'").is_some());
        assert!(blocker(Engine::Postgres, "reindex table concurrently t").is_some());
        // Postgres runs its DDL transactionally.
        assert_eq!(blocker(Engine::Postgres, "create index i on t (a)"), None);
        assert_eq!(
            blocker(Engine::Postgres, "alter table t add column b int"),
            None
        );
        // Only Postgres has `CONCURRENTLY`.
        assert_eq!(blocker(Engine::MySql, "vacuum"), None);
    }

    #[test]
    fn sqlite_names_what_a_transaction_would_swallow() {
        assert!(blocker(Engine::Sqlite, "vacuum").is_some());
        assert!(blocker(Engine::Sqlite, "attach 'b.db' as b").is_some());
        assert_eq!(
            blocker(Engine::Sqlite, "pragma main.foreign_keys = off").as_deref(),
            Some("PRAGMA MAIN FOREIGN_KEYS (statement 1) cannot run inside a transaction")
        );
        assert_eq!(blocker(Engine::Sqlite, "pragma table_info(t)"), None);
        assert_eq!(blocker(Engine::Sqlite, "create table t (a)"), None);
    }

    #[test]
    fn a_run_that_did_not_simply_succeed_says_so() {
        let failure = |index| ScriptFailure {
            index,
            total: 4,
            line: 1,
            statement: "x".into(),
            message: "no".into(),
        };
        let outcome = |failures, rolled_back, stopped, results: usize| ScriptOutcome {
            results: vec![QueryResult::default(); results],
            failures,
            total: 4,
            mode: ScriptMode::Transaction,
            rolled_back,
            stopped,
            elapsed: Duration::from_millis(5),
        };

        assert_eq!(outcome(vec![], false, false, 4).summary(), None);
        assert_eq!(
            outcome(vec![failure(1)], true, false, 1)
                .summary()
                .as_deref(),
            Some("Rolled back: statement 2 of 4 failed, so nothing was applied")
        );
        assert_eq!(
            outcome(vec![failure(2)], false, true, 2)
                .summary()
                .as_deref(),
            Some("Stopped at statement 3 of 4; the 2 statements that ran before it were applied")
        );
        assert_eq!(
            outcome(vec![failure(0), failure(2)], false, false, 2)
                .summary()
                .as_deref(),
            Some(
                "Ran 4 statements in 5 ms · 2 statements failed and skipped and the rest committed"
            )
        );
    }
}
