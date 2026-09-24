//! Importing a SQL dump into a database.
//!
//! A dump is read as a stream, split into statements, and run one at a time on
//! a single dedicated connection so that the session state a dump sets up —
//! `SET`, `USE`, `PRAGMA`, temporary tables — stays in effect for the whole
//! run. That connection is marked to close when it is dropped, so a cancelled
//! import cannot hand a half-open transaction back to the pool.
//!
//! The parts are:
//!
//! * [`reader`] — detecting compression and counting bytes for progress.
//! * [`splitter`] — streaming a dump into statements per dialect.
//! * the runner below — one statement at a time, with an error policy, and the
//!   transaction the policy asks for.

pub(crate) mod reader;
pub(crate) mod splitter;

pub use reader::Compression;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use sqlx::AssertSqlSafe;
use sqlx::pool::PoolConnection;
use tokio::sync::mpsc::UnboundedSender;

use super::config::Engine;
use splitter::Chunk;
pub use splitter::Dialect;

/// How much of a dump is looked at for the pre-flight warnings and for the
/// MySQL session settings.
pub(crate) const HEAD: usize = 64 * 1024;

/// How many errors are kept from a `Continue` run; the rest are counted only.
const MAX_ERRORS: usize = 200;

/// What to do when a statement fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnError {
    /// Stop at the first failure, leaving what already ran.
    #[default]
    Stop,
    /// Stop and undo the whole import.
    Rollback,
    /// Keep going, collecting the failures.
    Continue,
}

impl OnError {
    pub const ALL: [OnError; 3] = [OnError::Stop, OnError::Rollback, OnError::Continue];

    pub fn label(self) -> &'static str {
        match self {
            OnError::Stop => "Stop at the first error",
            OnError::Rollback => "Roll back everything",
            OnError::Continue => "Continue and log errors",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            OnError::Stop => "Keeps what ran before the failure",
            OnError::Rollback => "Undoes the whole import on any failure",
            OnError::Continue => "Skips failed statements and reports them at the end",
        }
    }
}

/// What the user asked for.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub path: PathBuf,
    pub on_error: OnError,
}

/// How far along a running import is.
#[derive(Debug, Clone, Default)]
pub struct ImportProgress {
    /// Compressed bytes read from the file.
    pub bytes: u64,
    /// Size of the file on disk.
    pub total: u64,
    pub statements: u64,
    pub errors: u64,
    /// The line the importer is on.
    pub line: u64,
}

impl ImportProgress {
    /// The bar's value, 0..=100. `0` when the size is unknown.
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.bytes as f64 / self.total as f64 * 100.0).clamp(0.0, 100.0) as f32
    }
}

/// One statement that failed.
#[derive(Debug, Clone)]
pub struct ImportError {
    pub line: u64,
    /// The statement, flattened and shortened for the log.
    pub statement: String,
    pub message: String,
}

/// How an import ended.
#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub statements: u64,
    pub errors: Vec<ImportError>,
    /// Failures beyond the ones kept in [`ImportSummary::errors`].
    pub dropped_errors: u64,
    pub elapsed: Duration,
    /// Whether a `Rollback` run undid everything.
    pub rolled_back: bool,
    /// Whether the dump carried its own `BEGIN`/`COMMIT`.
    pub dump_transaction: bool,
    /// Whether a requested rollback had to become a stop because the engine
    /// cannot roll a dump back.
    pub mysql_rollback_fallback: bool,
    pub total_bytes: u64,
}

impl ImportSummary {
    /// `"Imported 1,204 statements in 3.2 s · 2 errors"`, for the dialog.
    pub fn summary(&self) -> String {
        let unit = if self.statements == 1 {
            "statement"
        } else {
            "statements"
        };
        let mut text = format!(
            "Imported {} {unit} in {}",
            self.statements,
            elapsed(self.elapsed)
        );
        if self.rolled_back {
            text.push_str(" · rolled back");
        }

        let failures = self.errors.len() as u64 + self.dropped_errors;
        if failures > 0 {
            let unit = if failures == 1 { "error" } else { "errors" };
            text.push_str(&format!(" · {failures} {unit}"));
            if self.dropped_errors > 0 {
                text.push_str(&format!(" (first {} kept)", self.errors.len()));
            }
        }
        text
    }
}

/// A dedicated connection for the length of one import.
pub(crate) enum Session {
    Postgres(PoolConnection<sqlx::Postgres>),
    MySql(PoolConnection<sqlx::MySql>),
    Sqlite(PoolConnection<sqlx::Sqlite>),
}

impl Session {
    pub(crate) async fn postgres(pool: &sqlx::PgPool) -> Result<Self> {
        Ok(Session::Postgres(dedicated(pool).await?))
    }

    pub(crate) async fn mysql(pool: &sqlx::MySqlPool) -> Result<Self> {
        Ok(Session::MySql(dedicated(pool).await?))
    }

    pub(crate) async fn sqlite(pool: &sqlx::SqlitePool) -> Result<Self> {
        Ok(Session::Sqlite(dedicated(pool).await?))
    }

    pub(crate) fn engine(&self) -> Engine {
        match self {
            Session::Postgres(_) => Engine::Postgres,
            Session::MySql(_) => Engine::MySql,
            Session::Sqlite(_) => Engine::Sqlite,
        }
    }

    /// Run one statement, discarding whatever it returns.
    async fn execute(&mut self, sql: &str) -> Result<()> {
        match self {
            Session::Postgres(connection) => {
                let statement = AssertSqlSafe(sql.to_string());
                sqlx::query(statement).execute(&mut **connection).await?;
            }
            Session::MySql(connection) => {
                let statement = AssertSqlSafe(sql.to_string());
                sqlx::query(statement).execute(&mut **connection).await?;
            }
            Session::Sqlite(connection) => {
                let statement = AssertSqlSafe(sql.to_string());
                sqlx::query(statement).execute(&mut **connection).await?;
            }
        }
        Ok(())
    }
}

/// Check one connection out of `pool` and mark it to close rather than return
/// to the pool, so a cancelled import cannot leave a transaction behind.
async fn dedicated<DB>(pool: &sqlx::Pool<DB>) -> Result<PoolConnection<DB>>
where
    DB: sqlx::Database,
{
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    Ok(connection)
}

/// Connect `session`, read `request`'s dump, and run it.
pub(crate) async fn run(
    mut session: Session,
    dialect: Dialect,
    request: &ImportRequest,
    sender: UnboundedSender<ImportProgress>,
) -> Result<ImportSummary> {
    let started = Instant::now();
    let engine = session.engine();
    let opened = reader::open(&request.path)?;
    let mut progress = Progress {
        sender,
        consumed: opened.consumed.clone(),
        total: opened.total_bytes,
        last: Instant::now(),
    };
    progress.emit(0, 0, 0);

    let mut summary = ImportSummary {
        total_bytes: opened.total_bytes,
        ..ImportSummary::default()
    };

    // MySQL commits implicitly at every DDL statement, so a wrapping
    // transaction would not cover a dump. Falling back to `Stop` is reported
    // rather than silently pretended to be a rollback.
    let policy = match (request.on_error, engine) {
        (OnError::Rollback, Engine::MySql) => {
            summary.mysql_rollback_fallback = true;
            OnError::Stop
        }
        (policy, _) => policy,
    };

    // MySQL is told to skip foreign-key and unique checks, unless the dump
    // already turns them off itself.
    let mut mysql_settings = false;
    if engine == Engine::MySql {
        let head = reader::inspect(&request.path, HEAD)
            .map(|inspection| inspection.head)
            .unwrap_or_default();
        if !mentions(&head, "FOREIGN_KEY_CHECKS") && !mentions(&head, "UNIQUE_CHECKS") {
            session.execute("SET FOREIGN_KEY_CHECKS=0").await?;
            session.execute("SET UNIQUE_CHECKS=0").await?;
            mysql_settings = true;
        }
    }

    let mut chunks = splitter::new(opened.reader, dialect).peekable();
    let mut errors = Vec::new();
    let mut dropped_errors = 0u64;
    let mut began = false;
    let mut rolled_back = false;
    let mut stopped = false;
    let mut last_line = 0u64;

    while !stopped {
        let Some(chunk) = chunks.next() else {
            break;
        };
        let chunk = chunk?;

        let (statement, copying) = match chunk {
            Chunk::Statement(statement) => (statement, false),
            Chunk::Copy(statement) => (statement, true),
            // COPY data belongs to the header just past; a stray block means
            // the splitter and this runner disagree about the state.
            Chunk::CopyData(_) => continue,
        };

        // The dump's own transaction control is replaced by ours, so a dump
        // that already wraps itself does not nest a second transaction (which
        // would be an error on SQLite and a no-op-with-warning on Postgres).
        if transaction_control(&statement.sql) {
            summary.dump_transaction = true;
            continue;
        }

        if policy == OnError::Rollback && !began && !is_pragma(&statement.sql) {
            session.execute("BEGIN").await?;
            began = true;
        }

        summary.statements += 1;
        last_line = statement.line;
        progress.tick(summary.statements, errors.len() as u64, statement.line);

        let result = if copying {
            copy_block(
                &mut session,
                &statement.sql,
                &mut chunks,
                &mut progress,
                summary.statements,
                errors.len() as u64,
                statement.line,
            )
            .await
        } else {
            session.execute(&statement.sql).await
        };

        if let Err(error) = result {
            let entry = ImportError {
                line: statement.line,
                statement: excerpt(&statement.sql),
                message: format!("{error:#}"),
            };
            match policy {
                OnError::Stop => {
                    errors.push(entry);
                    stopped = true;
                }
                OnError::Rollback => {
                    session.execute("ROLLBACK").await.ok();
                    rolled_back = true;
                    errors.push(entry);
                    stopped = true;
                }
                OnError::Continue => {
                    if errors.len() < MAX_ERRORS {
                        errors.push(entry);
                    } else {
                        dropped_errors += 1;
                    }
                }
            }
        }
    }

    if policy == OnError::Rollback && began && !rolled_back {
        session.execute("COMMIT").await?;
    }

    if mysql_settings {
        session.execute("SET FOREIGN_KEY_CHECKS=1").await.ok();
        session.execute("SET UNIQUE_CHECKS=1").await.ok();
    }

    summary.errors = errors;
    summary.dropped_errors = dropped_errors;
    summary.rolled_back = rolled_back;
    summary.elapsed = started.elapsed();
    progress.emit(summary.statements, summary.errors.len() as u64, last_line);

    Ok(summary)
}

/// Run a `COPY ... FROM stdin` header together with the data that follows it.
///
/// The writer borrows the connection for the length of the block, so the data
/// is streamed rather than assembled: a table's export never sits in memory all
/// at once.
async fn copy_block<I>(
    session: &mut Session,
    statement: &str,
    chunks: &mut std::iter::Peekable<I>,
    progress: &mut Progress,
    statements: u64,
    errors: u64,
    line: u64,
) -> Result<()>
where
    I: Iterator<Item = Result<Chunk>>,
{
    match session {
        Session::Postgres(connection) => {
            let mut writer = connection.copy_in_raw(statement).await?;
            while matches!(chunks.peek(), Some(Ok(Chunk::CopyData(_)))) {
                match chunks.next() {
                    Some(Ok(Chunk::CopyData(data))) => {
                        writer.send(data).await?;
                        // A COPY of a large table is the one place a dump can
                        // read for a long time without a statement boundary, so
                        // the bar is fed from here as well.
                        progress.tick(statements, errors, line);
                    }
                    _ => break,
                }
            }
            writer.finish().await?;
            Ok(())
        }
        _ => bail!("COPY data can only be read on PostgreSQL"),
    }
}

/// Whether `sql` is a `PRAGMA`, which must run outside a transaction to take
/// effect — SQLite ignores `PRAGMA foreign_keys` while one is open.
fn is_pragma(sql: &str) -> bool {
    leading_word(sql).as_deref() == Some("PRAGMA")
}

/// Whether `sql` is a top-level `BEGIN`/`COMMIT` and friends, which the import
/// manages itself.
fn transaction_control(sql: &str) -> bool {
    matches!(
        leading_word(sql).as_deref(),
        Some("BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "ABORT")
    )
}

/// The first word of `sql`, skipping leading comments and whitespace.
fn leading_word(sql: &str) -> Option<String> {
    let characters: Vec<char> = sql.chars().collect();
    let mut index = 0;

    loop {
        while index < characters.len() && characters[index].is_whitespace() {
            index += 1;
        }
        match (characters.get(index), characters.get(index + 1)) {
            (Some('-'), Some('-')) | (Some('#'), _) => {
                while index < characters.len() && characters[index] != '\n' {
                    index += 1;
                }
            }
            (Some('/'), Some('*')) => {
                index += 2;
                while index < characters.len() {
                    if characters[index] == '*' && characters.get(index + 1) == Some(&'/') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => break,
        }
    }

    let mut word = String::new();
    while let Some(character) = characters.get(index) {
        if character.is_alphabetic() || *character == '_' {
            word.push(*character);
            index += 1;
        } else {
            break;
        }
    }

    (!word.is_empty()).then(|| word.to_ascii_uppercase())
}

/// Whether `head` mentions `needle`, up to case.
fn mentions(head: &str, needle: &str) -> bool {
    head.to_ascii_uppercase()
        .contains(&needle.to_ascii_uppercase())
}

/// One line's worth of `sql`, for an error log.
fn excerpt(sql: &str) -> String {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        let mut shortened: String = flat.chars().take(119).collect();
        shortened.push('…');
        shortened
    } else {
        flat
    }
}

/// `"3.2 s"` or `"840 ms"`.
fn elapsed(duration: Duration) -> String {
    if duration.as_secs_f64() >= 1.0 {
        format!("{:.1} s", duration.as_secs_f64())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

/// The progress channel, throttled so a fast dump does not flood the UI.
struct Progress {
    sender: UnboundedSender<ImportProgress>,
    consumed: Arc<AtomicU64>,
    total: u64,
    last: Instant,
}

impl Progress {
    fn tick(&mut self, statements: u64, errors: u64, line: u64) {
        if self.last.elapsed() < Duration::from_millis(100) {
            return;
        }
        self.emit(statements, errors, line);
    }

    fn emit(&mut self, statements: u64, errors: u64, line: u64) {
        self.last = Instant::now();
        let _ = self.sender.send(ImportProgress {
            bytes: self.consumed.load(Ordering::Relaxed),
            total: self.total,
            statements,
            errors,
            line,
        });
    }
}

/// A pre-flight look at a dump: what it is, and what stands out about it.
pub(crate) struct Preflight {
    pub compression: Compression,
    pub total_bytes: u64,
    /// The connection's engine does not match the dump's header.
    pub dialect_mismatch: Option<String>,
    /// How many `DROP`, `TRUNCATE`, or unconstrained `DELETE` statements the
    /// head holds.
    pub destructive: usize,
    /// The file's magic or head could not be read.
    pub error: Option<String>,
}

/// Read `path`'s head and describe it for the dialog.
pub(crate) fn preflight(path: &std::path::Path, engine: Engine) -> Preflight {
    match reader::inspect(path, HEAD) {
        Ok(inspection) => {
            let hint = dialect_hint(&inspection.head);
            let dialect_mismatch = match hint {
                Some(hint) if hint != engine => Some(format!(
                    "This looks like a {} dump, but the connection is {}.",
                    hint.label(),
                    engine.label()
                )),
                _ => None,
            };
            Preflight {
                compression: inspection.compression,
                total_bytes: inspection.total_bytes,
                dialect_mismatch,
                destructive: destructive_count(&inspection.head),
                error: None,
            }
        }
        Err(error) => Preflight {
            compression: Compression::None,
            total_bytes: 0,
            dialect_mismatch: None,
            destructive: 0,
            error: Some(format!("{error:#}")),
        },
    }
}

/// Guess the engine a dump was written by from its header.
fn dialect_hint(head: &str) -> Option<Engine> {
    let upper = head.to_ascii_uppercase();
    if upper.contains("POSTGRESQL DATABASE DUMP") || upper.contains("PG_DUMP") {
        return Some(Engine::Postgres);
    }
    if upper.contains("MYSQL DUMP") || upper.contains("MARIADB DUMP") || upper.contains("MYSQLDUMP")
    {
        return Some(Engine::MySql);
    }
    let trimmed = head.trim_start();
    if trimmed.starts_with("PRAGMA") || trimmed.starts_with("BEGIN TRANSACTION") {
        return Some(Engine::Sqlite);
    }
    None
}

/// Count the statements in `head` that would throw data away.
fn destructive_count(head: &str) -> usize {
    super::statement::split(head)
        .iter()
        .filter(|statement| {
            let upper = statement.text.to_ascii_uppercase();
            match upper.split_whitespace().next().unwrap_or_default() {
                "DROP" | "TRUNCATE" => true,
                "DELETE" => !contains_word(&upper, "WHERE"),
                _ => false,
            }
        })
        .count()
}

/// Whether `haystack` contains `needle` as a standalone word, not merely as a
/// substring — so `DELETE FROM t WHERE(id>0)` and a `WHERE` set off by a
/// newline both count as bounded, the way a plain `" WHERE "` search misses,
/// while an identifier like `wherefore` that happens to start with the same
/// letters does not falsely clear a real unbounded `DELETE`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + needle.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_control_is_recognised_behind_comments() {
        assert!(transaction_control("BEGIN"));
        assert!(transaction_control("  start transaction;"));
        assert!(transaction_control("-- a note\nCOMMIT"));
        assert!(transaction_control("/* c */ END"));
        assert!(!transaction_control("SELECT 1"));
        assert!(!transaction_control("DROP TABLE t"));
    }

    #[test]
    fn the_destructive_count_names_drop_truncate_and_unbounded_delete() {
        let head =
            "DROP TABLE a;\nTRUNCATE b;\nDELETE FROM c WHERE id = 1;\nDELETE FROM d;\nSELECT 1;";
        assert_eq!(destructive_count(head), 3);
    }

    /// A `WHERE` set off by punctuation rather than a bare space either side
    /// still bounds the `DELETE` — the bug a plain `" WHERE "` search missed.
    #[test]
    fn a_where_clause_with_no_space_before_it_still_bounds_the_delete() {
        let head = "DELETE FROM c WHERE(id > 0);\nDELETE FROM d WHERE\nid = 1;";
        assert_eq!(destructive_count(head), 0);
    }

    /// An identifier that merely starts with the same letters as the keyword
    /// must not be mistaken for one, in either direction.
    #[test]
    fn an_identifier_starting_with_where_is_not_mistaken_for_the_keyword() {
        let head = "DELETE FROM wherefore;";
        assert_eq!(destructive_count(head), 1);
    }

    #[test]
    fn a_header_tells_the_engine_apart() {
        assert_eq!(
            dialect_hint("--\n-- PostgreSQL database dump\n--"),
            Some(Engine::Postgres)
        );
        assert_eq!(
            dialect_hint("-- MySQL dump 10.13  Distrib 8.0"),
            Some(Engine::MySql)
        );
        assert_eq!(
            dialect_hint("PRAGMA foreign_keys=OFF;"),
            Some(Engine::Sqlite)
        );
        assert_eq!(dialect_hint("SELECT 1;"), None);
    }

    #[test]
    fn an_excerpt_is_flattened_and_shortened() {
        assert_eq!(excerpt("SELECT\n  1"), "SELECT 1");
        let long = "a".repeat(200);
        let short = excerpt(&long);
        assert!(short.chars().count() <= 120);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn progress_has_a_percentage() {
        let progress = ImportProgress {
            bytes: 50,
            total: 200,
            ..ImportProgress::default()
        };
        assert_eq!(progress.percent(), 25.0);
        assert_eq!(ImportProgress::default().percent(), 0.0);
    }
}

/// The runner, against a real (temporary) SQLite database.
#[cfg(test)]
mod database_tests {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use uuid::Uuid;

    use super::*;
    use crate::db::tests::TempDatabase;
    use crate::db::{Connection, ConnectionConfig, SafetyMode};

    /// A dump file that removes itself with the test.
    struct Dump {
        path: PathBuf,
    }

    impl Dump {
        fn text(contents: &str) -> Self {
            let path = std::env::temp_dir().join(format!("zippa-dump-{}.sql", Uuid::new_v4()));
            std::fs::write(&path, contents).expect("could not write the test dump");
            Self { path }
        }

        fn gzip(contents: &str) -> Self {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(contents.as_bytes())
                .expect("could not compress the test dump");
            let bytes = encoder.finish().expect("could not compress the test dump");
            let path = std::env::temp_dir().join(format!("zippa-dump-{}.sql.gz", Uuid::new_v4()));
            std::fs::write(&path, bytes).expect("could not write the test dump");
            Self { path }
        }
    }

    impl Drop for Dump {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    async fn import(
        connection: &Connection,
        path: &Path,
        on_error: OnError,
    ) -> Result<ImportSummary> {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        connection
            .import_dump(
                ImportRequest {
                    path: path.to_path_buf(),
                    on_error,
                },
                sender,
            )
            .await
    }

    async fn count(connection: &Connection, table: &str) -> Option<i64> {
        let result = connection
            .run_query(&format!("SELECT count(*) FROM {table}"))
            .await
            .ok()?;
        result.rows.first()?.first()?.as_ref()?.parse().ok()
    }

    /// A dump whose third statement cannot run, after one row has been written.
    const BROKEN: &str = "CREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\nTHIS IS NOT SQL;\nINSERT INTO t VALUES (2);\n";

    #[tokio::test]
    async fn a_dump_of_statements_is_imported() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::text(
            "CREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\nINSERT INTO t VALUES (2);\n",
        );

        let summary = import(&connection, &dump.path, OnError::Stop)
            .await
            .expect("the dump should import");
        assert_eq!(summary.statements, 3);
        assert!(summary.errors.is_empty());
        assert!(!summary.rolled_back);
        assert_eq!(count(&connection, "t").await, Some(2));

        connection.close().await;
    }

    #[tokio::test]
    async fn stopping_keeps_what_ran_before_the_failure() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::text(BROKEN);

        let summary = import(&connection, &dump.path, OnError::Stop)
            .await
            .expect("the import should finish");
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].line, 3, "the failing statement's line");
        assert!(!summary.rolled_back);
        assert_eq!(count(&connection, "t").await, Some(1));

        connection.close().await;
    }

    #[tokio::test]
    async fn rolling_back_undoes_the_whole_import() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::text(BROKEN);

        let summary = import(&connection, &dump.path, OnError::Rollback)
            .await
            .expect("the import should finish");
        assert!(summary.rolled_back);
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(
            count(&connection, "t").await,
            None,
            "the table created earlier in the dump should be gone"
        );

        connection.close().await;
    }

    #[tokio::test]
    async fn a_pragma_runs_outside_the_wrapping_transaction() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        // SQLite ignores `PRAGMA foreign_keys` inside a transaction, so the
        // pragma has to run before the wrapper begins for this insert — which
        // violates the foreign key — to be accepted.
        let dump = Dump::text(
            "PRAGMA foreign_keys=OFF;\n\
             CREATE TABLE parent (id int primary key);\n\
             CREATE TABLE child (id int primary key, parent_id int references parent(id));\n\
             INSERT INTO child VALUES (1, 99);\n",
        );

        let summary = import(&connection, &dump.path, OnError::Rollback)
            .await
            .expect("the dump should import");
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        assert_eq!(count(&connection, "child").await, Some(1));

        connection.close().await;
    }

    #[tokio::test]
    async fn continuing_past_a_failure_finishes_the_dump() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::text(BROKEN);

        let summary = import(&connection, &dump.path, OnError::Continue)
            .await
            .expect("the import should finish");
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.statements, 4);
        assert_eq!(count(&connection, "t").await, Some(2));

        connection.close().await;
    }

    #[tokio::test]
    async fn a_read_only_connection_refuses_a_dump() {
        let database = TempDatabase::new().await;
        let config = ConnectionConfig {
            safety: SafetyMode::ReadOnly,
            ..database.config()
        };
        let connection = Connection::open(config, None).await.unwrap();
        let dump = Dump::text("CREATE TABLE t (id int);\n");

        let error = import(&connection, &dump.path, OnError::Stop)
            .await
            .expect_err("a read-only connection should refuse");
        assert!(format!("{error:#}").contains("read-only"), "{error:#}");

        connection.close().await;
    }

    #[tokio::test]
    async fn a_dump_that_wraps_itself_is_not_nested() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::text(
            "BEGIN TRANSACTION;\nCREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\nCOMMIT;\n",
        );

        let summary = import(&connection, &dump.path, OnError::Stop)
            .await
            .expect("the dump should import");
        assert!(summary.dump_transaction, "the dump's transaction was noted");
        assert_eq!(summary.statements, 2, "BEGIN and COMMIT are ours to manage");
        assert_eq!(count(&connection, "t").await, Some(1));

        connection.close().await;
    }

    #[tokio::test]
    async fn a_gzipped_dump_is_decompressed_on_the_way_in() {
        let database = TempDatabase::new().await;
        let connection = Connection::open(database.config(), None).await.unwrap();
        let dump = Dump::gzip("CREATE TABLE t (id int);\nINSERT INTO t VALUES (7);\n");

        let summary = import(&connection, &dump.path, OnError::Stop)
            .await
            .expect("the dump should import");
        assert_eq!(summary.statements, 2);
        assert_eq!(count(&connection, "t").await, Some(1));

        connection.close().await;
    }
}
