//! Query history: the statements the user ran from a query tab, kept per
//! connection so they can be found and reopened later.
//!
//! Where the console ([`super::query_log`]) is every statement this run of the
//! app sent — the app's own reads included — and goes when the app closes,
//! history is only what the user typed, and by default it outlives the app.
//!
//! **One store per connection.** Each saved connection's history is its own
//! JSON Lines file, `query-history/<connection id>.jsonl` under the config
//! dir, so an entry belongs to exactly one connection and nothing has to tell
//! two connections apart. The file is named by the connection's id, not its
//! name, so a rename keeps its history; deleting the connection deletes the
//! file ([`clear`]). The "All connections" view reads every file and merges
//! them by time.
//!
//! **Retention** ([`Retention`]) is a [`Keep`] mode — on disk, in memory until
//! the app quits, or off — plus a count cap per connection and an optional
//! age limit. The caps apply whatever the mode: reading drops what they
//! exclude at once, and the file is rewritten without it on the next
//! compaction (the first write to a file in a run, then every so many
//! writes, and whenever the retention changes).
//!
//! **What is recorded.** [`super::Connection::remember`] is called from the
//! query tab's paths — a run (`PinnedConnection::run_query_recorded`) and
//! each statement of a script (`ScriptRun`) — with the text as the user wrote
//! it: before the tab's implicit `LIMIT` and before `:name` variables become
//! bind parameters, so reopening an entry gives back what was typed. `EXPLAIN`
//! from the plan button, the app's own reads and writes, and the Agents
//! panel's statements are not history. Passwords never are either, but SQL
//! text can hold anything the user typed, so the file is owner-only like
//! every other file that can hold a pasted secret, and the Off and in-memory
//! modes exist for a machine that should not keep it.
//!
//! **Writes never wait.** A statement finishes on the database runtime, which
//! must not block on the disk, so appends, compactions, and deletions are
//! handed to one writer thread and done there in order; [`flush`] waits for
//! the queue to drain at quit. A line torn by a crash is skipped on reading
//! and dropped by the next compaction. Under `#[cfg(test)]` the writer runs
//! inline, and the state is per thread, so each test sees only its own
//! history and its own scratch config dir.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::query_log::QueryOutcome;
use super::store;

/// Directory under the config dir holding one file per connection.
const DIR: &str = "query-history";

/// Entries kept per connection until the user says otherwise.
pub const DEFAULT_MAX_ENTRIES: usize = 1_000;

/// The largest count cap the settings accept; `0` means no cap at all.
pub const MAX_ENTRIES_LIMIT: usize = 100_000;

/// A statement longer than this is kept cut short, with a comment saying so,
/// so one pasted dump cannot make every later read of the file slow.
pub const MAX_SQL_BYTES: usize = 64 * 1024;

/// Where history is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Keep {
    /// In a file per connection, so it is there after a restart.
    #[default]
    Disk,
    /// In memory only, gone when the app quits.
    Memory,
    /// Not recorded at all.
    Off,
}

impl Keep {
    pub const ALL: [Self; 3] = [Self::Disk, Self::Memory, Self::Off];

    /// Stable name, used as the dropdown's value and in the JSON file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Memory => "memory",
            Self::Off => "off",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Disk => "On this machine",
            Self::Memory => "Until the app quits",
            Self::Off => "Off",
        }
    }

    /// The mode a dropdown value names.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|keep| keep.key() == key)
            .unwrap_or_default()
    }
}

/// How much history is kept, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub keep: Keep,
    /// Entries kept per connection, newest first; `0` keeps every one.
    pub max_entries: usize,
    /// Days an entry is kept; `0` keeps it however old it is.
    pub max_age_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            keep: Keep::default(),
            max_entries: DEFAULT_MAX_ENTRIES,
            max_age_days: 0,
        }
    }
}

impl Retention {
    /// One connection's entries, oldest first, less what the caps exclude:
    /// anything older than the age limit, then the oldest past the count.
    pub fn apply(&self, mut entries: Vec<HistoryEntry>, now: DateTime<Utc>) -> Vec<HistoryEntry> {
        if self.max_age_days > 0 {
            // A limit past what a date can hold (a hand-edited settings
            // file) keeps everything rather than panicking.
            let cutoff = chrono::Duration::try_days(i64::from(self.max_age_days))
                .and_then(|age| now.checked_sub_signed(age));
            if let Some(cutoff) = cutoff {
                entries.retain(|entry| entry.at >= cutoff);
            }
        }
        if self.max_entries > 0 && entries.len() > self.max_entries {
            entries.drain(..entries.len() - self.max_entries);
        }
        entries
    }

    /// Appends to one file between compactions: a fifth of the cap, so a
    /// file never holds more than 20% past it, or a fixed number without one
    /// (the age limit still needs applying now and then).
    fn compact_every(&self) -> usize {
        if self.max_entries > 0 {
            (self.max_entries / 5).max(20)
        } else {
            500
        }
    }
}

/// One statement the user ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: DateTime<Utc>,
    /// The saved connection it ran on — also the file it is kept in.
    pub connection: Uuid,
    /// The database the connection was on, as the connection names it (a
    /// file path for SQLite).
    #[serde(default)]
    pub database: String,
    pub sql: String,
    pub outcome: QueryOutcome,
    #[serde(default)]
    pub elapsed_ms: u64,
}

impl HistoryEntry {
    /// An entry for a statement that just finished, its text cut to
    /// [`MAX_SQL_BYTES`].
    pub fn new(
        connection: Uuid,
        database: &str,
        sql: &str,
        outcome: QueryOutcome,
        elapsed: Duration,
    ) -> Self {
        Self {
            at: Utc::now(),
            connection,
            database: database.to_string(),
            sql: truncate(sql),
            outcome,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// `sql`, or as much of it as fits in [`MAX_SQL_BYTES`] with a trailing
/// comment saying how much was left out, so a reopened entry cannot be
/// mistaken for the whole statement.
fn truncate(sql: &str) -> String {
    if sql.len() <= MAX_SQL_BYTES {
        return sql.to_string();
    }
    let mut end = MAX_SQL_BYTES;
    while !sql.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n-- (cut short: {} more bytes were not kept in history)",
        &sql[..end],
        sql.len() - end
    )
}

/// What this process has recorded in memory, and the retention in force.
struct State {
    retention: Retention,
    /// The in-memory mode's entries, oldest first, per connection.
    memory: HashMap<Uuid, VecDeque<HistoryEntry>>,
}

impl State {
    fn new() -> Self {
        Self {
            // A test records nothing unless it asks: every UI test runs
            // queries, and none of them should leave files behind.
            retention: if cfg!(test) {
                Retention {
                    keep: Keep::Off,
                    ..Retention::default()
                }
            } else {
                Retention::default()
            },
            memory: HashMap::new(),
        }
    }

    fn prune_memory(&mut self, now: DateTime<Utc>) {
        let retention = self.retention;
        for entries in self.memory.values_mut() {
            let kept = retention.apply(entries.drain(..).collect(), now);
            entries.extend(kept);
        }
        self.memory.retain(|_, entries| !entries.is_empty());
    }
}

#[cfg(not(test))]
fn with_state<R>(change: impl FnOnce(&mut State) -> R) -> R {
    static STATE: std::sync::LazyLock<std::sync::Mutex<State>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(State::new()));
    let mut state = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    change(&mut state)
}

#[cfg(test)]
std::thread_local! {
    static TEST_STATE: std::cell::RefCell<State> = std::cell::RefCell::new(State::new());
    static TEST_WRITER: std::cell::RefCell<Writer> = std::cell::RefCell::new(Writer::default());
}

#[cfg(test)]
fn with_state<R>(change: impl FnOnce(&mut State) -> R) -> R {
    TEST_STATE.with(|state| change(&mut state.borrow_mut()))
}

/// Put `retention` in force. Called at startup and whenever the settings
/// change; the files on disk are compacted to it in the background.
pub fn configure(retention: Retention) {
    let dir = history_dir();
    // The compaction is queued under the same lock `record` holds, so no
    // append decided under the old retention can land after it.
    with_state(|state| {
        state.retention = retention;
        state.prune_memory(Utc::now());
        if let Ok(dir) = dir {
            submit(Job::CompactAll { dir, retention });
        }
    });
}

/// The retention in force.
pub fn retention() -> Retention {
    with_state(|state| state.retention)
}

/// Keep `entry`, wherever the retention says to. A blank statement is not
/// worth an entry.
pub fn record(entry: HistoryEntry) {
    if entry.sql.trim().is_empty() {
        return;
    }
    let path = history_file(entry.connection);
    // One lock across reading the retention and acting on it, so a switch to
    // Off cannot be overtaken by an append decided just before it.
    with_state(|state| {
        let retention = state.retention;
        match retention.keep {
            Keep::Off => {}
            Keep::Memory => {
                let entries = state.memory.entry(entry.connection).or_default();
                entries.push_back(entry);
                let kept = retention.apply(entries.drain(..).collect(), Utc::now());
                entries.extend(kept);
            }
            Keep::Disk => match path {
                Ok(path) => submit(Job::Append {
                    path,
                    entry,
                    retention,
                }),
                Err(error) => eprintln!("could not record query history: {error:#}"),
            },
        }
    });
}

/// One connection's history, newest first: its file and whatever this run
/// kept in memory, less what the retention excludes.
///
/// Reads the file, so call it off the UI thread.
pub fn entries(connection: Uuid) -> Vec<HistoryEntry> {
    // A run's append is queued just before its tab asks for a re-read; let
    // it land first so the statement that just finished is in what is read.
    flush();
    let mut entries = history_file(connection)
        .map(|path| read_file(&path))
        .unwrap_or_default();
    entries.extend(memory_entries(Some(connection)));
    newest_first(retention().apply(oldest_first(entries), Utc::now()))
}

/// Every connection's history merged by time, newest first, each connection
/// capped the way [`entries`] caps it.
///
/// Reads every file, so call it off the UI thread.
pub fn all_entries() -> Vec<HistoryEntry> {
    flush();
    let mut by_connection: HashMap<Uuid, Vec<HistoryEntry>> = HashMap::new();
    if let Ok(dir) = history_dir() {
        for (connection, path) in history_files(&dir) {
            by_connection
                .entry(connection)
                .or_default()
                .extend(read_file(&path));
        }
    }
    for entry in memory_entries(None) {
        by_connection
            .entry(entry.connection)
            .or_default()
            .push(entry);
    }

    let retention = retention();
    let now = Utc::now();
    let merged = by_connection
        .into_values()
        .flat_map(|entries| retention.apply(oldest_first(entries), now))
        .collect();
    newest_first(oldest_first(merged))
}

/// Forget one connection's history — `None` forgets every connection's —
/// in memory at once and on disk in the background.
pub fn clear(connection: Option<Uuid>) {
    with_state(|state| match connection {
        Some(id) => {
            state.memory.remove(&id);
        }
        None => state.memory.clear(),
    });
    let path = match connection {
        Some(id) => history_file(id),
        None => history_dir(),
    };
    match path {
        Ok(path) => submit(Job::Remove { path }),
        Err(error) => eprintln!("could not clear query history: {error:#}"),
    }
}

/// Wait, briefly, for the writes already handed off to land: at quit, so the
/// last statement run is not lost with the process, and before a read, so it
/// sees them. Bounded, so a stuck disk cannot hang the app on its way out.
pub fn flush() {
    #[cfg(not(test))]
    {
        let (done, wait) = std::sync::mpsc::channel();
        if writer().is_some_and(|sender| sender.send(Job::Flush(done)).is_ok()) {
            let _ = wait.recv_timeout(Duration::from_secs(2));
        }
    }
}

fn memory_entries(connection: Option<Uuid>) -> Vec<HistoryEntry> {
    with_state(|state| {
        state
            .memory
            .iter()
            .filter(|(id, _)| connection.is_none_or(|wanted| **id == wanted))
            .flat_map(|(_, entries)| entries.iter().cloned())
            .collect()
    })
}

fn oldest_first(mut entries: Vec<HistoryEntry>) -> Vec<HistoryEntry> {
    // Stable, so two entries in the same instant keep the order they were
    // written in.
    entries.sort_by_key(|entry| entry.at);
    entries
}

fn newest_first(mut entries: Vec<HistoryEntry>) -> Vec<HistoryEntry> {
    entries.reverse();
    entries
}

/// Where every connection's history file lives.
pub fn history_dir() -> Result<PathBuf> {
    Ok(store::config_dir()?.join(DIR))
}

/// One connection's history file.
pub fn history_file(connection: Uuid) -> Result<PathBuf> {
    Ok(history_dir()?.join(format!("{connection}.jsonl")))
}

/// Every `<uuid>.jsonl` in `dir`.
fn history_files(dir: &Path) -> Vec<(Uuid, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }
            let id = Uuid::parse_str(path.file_stem()?.to_str()?).ok()?;
            Some((id, path))
        })
        .collect()
}

/// A history file's entries in file order. A missing file is no history; a
/// line that does not parse — torn by a crash, or written by a newer build
/// in a shape this one cannot read — is skipped rather than costing the rest.
fn read_file(path: &Path) -> Vec<HistoryEntry> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    parse_lines(&bytes).0
}

/// Every entry in a file's bytes, and how many non-blank lines there were.
/// Split as bytes rather than read as one string, so a line torn in the
/// middle of a multi-byte character costs only itself, not the whole file.
fn parse_lines(bytes: &[u8]) -> (Vec<HistoryEntry>, usize) {
    let lines: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .collect();
    let count = lines.len();
    let entries = lines
        .into_iter()
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    (entries, count)
}

/// Disk work, done in order on the writer thread.
enum Job {
    Append {
        path: PathBuf,
        entry: HistoryEntry,
        retention: Retention,
    },
    CompactAll {
        dir: PathBuf,
        retention: Retention,
    },
    /// A file or a whole directory.
    Remove {
        path: PathBuf,
    },
    /// Answer once everything sent before has been done.
    #[cfg_attr(test, allow(dead_code))]
    Flush(std::sync::mpsc::Sender<()>),
}

/// The writer thread's own bookkeeping.
#[derive(Default)]
struct Writer {
    /// Appends to each file since it was last compacted. A file with no
    /// count has not been compacted in this run, so its first append
    /// compacts it first — that is when a cap lowered since the last run, or
    /// an age limit that has passed, takes effect on the file.
    appended: HashMap<PathBuf, usize>,
}

impl Writer {
    fn run(&mut self, job: Job) {
        let result = match job {
            Job::Append {
                path,
                entry,
                retention,
            } => self.append(&path, &entry, retention),
            Job::CompactAll { dir, retention } => {
                for (_, path) in history_files(&dir) {
                    if let Err(error) = compact(&path, retention) {
                        eprintln!("could not compact query history: {error:#}");
                    }
                    self.appended.insert(path, 0);
                }
                Ok(())
            }
            Job::Remove { path } => {
                self.appended.retain(|file, _| !file.starts_with(&path));
                remove(&path)
            }
            Job::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
        };
        if let Err(error) = result {
            eprintln!("could not write query history: {error:#}");
        }
    }

    fn append(&mut self, path: &Path, entry: &HistoryEntry, retention: Retention) -> Result<()> {
        if !self.appended.contains_key(path) {
            compact(path, retention)?;
            self.appended.insert(path.to_path_buf(), 0);
        }
        append_line(path, entry)?;
        let count = self.appended.entry(path.to_path_buf()).or_default();
        *count += 1;
        if *count >= retention.compact_every() {
            *count = 0;
            compact(path, retention)?;
        }
        Ok(())
    }
}

/// Hand `job` to the writer thread, or — when there is none to be had — do it
/// here rather than lose it.
#[cfg(not(test))]
fn submit(job: Job) {
    match writer() {
        Some(sender) => {
            if let Err(std::sync::mpsc::SendError(job)) = sender.send(job) {
                Writer::default().run(job);
            }
        }
        // The thread failed to start; slower in place beats lost.
        None => Writer::default().run(job),
    }
}

#[cfg(not(test))]
fn writer() -> Option<&'static std::sync::mpsc::Sender<Job>> {
    static SENDER: std::sync::LazyLock<Option<std::sync::mpsc::Sender<Job>>> =
        std::sync::LazyLock::new(|| {
            let (sender, jobs) = std::sync::mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("query-history".into())
                .spawn(move || {
                    let mut writer = Writer::default();
                    for job in jobs {
                        writer.run(job);
                    }
                })
                .ok()
                .map(|_| sender)
        });
    SENDER.as_ref()
}

#[cfg(test)]
fn submit(job: Job) {
    TEST_WRITER.with(|writer| writer.borrow_mut().run(job));
}

/// Add one line to `path`, owner-only from the first byte. A file whose last
/// line was torn by a crash gets a newline first, so the new entry does not
/// glue onto the broken one and go unread with it.
fn append_line(path: &Path, entry: &HistoryEntry) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("could not open {}", path.display()))?;

    let mut line = String::new();
    if file.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&serde_json::to_string(entry)?);
    line.push('\n');
    file.write_all(line.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))
}

/// Rewrite `path` with only what `retention` keeps, and without lines that
/// do not parse. Left alone when nothing would change; removed when nothing
/// is left.
fn compact(path: &Path, retention: Retention) -> Result<()> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(());
    };
    let (entries, lines) = parse_lines(&bytes);
    let kept = retention.apply(oldest_first(entries), Utc::now());
    if kept.len() == lines {
        return Ok(());
    }
    if kept.is_empty() {
        return remove(path);
    }
    let mut contents = String::new();
    for entry in &kept {
        contents.push_str(&serde_json::to_string(entry)?);
        contents.push('\n');
    }
    store::write_atomic(path, &contents, store::ticket())
}

/// Delete a file or a directory; deleting what is already gone is not an
/// error.
fn remove(path: &Path) -> Result<()> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not delete {}", path.display())),
    }
}

/// History entries whose SQL matches `filter`, for the History tab's search.
pub fn matching<'a>(
    entries: &'a [HistoryEntry],
    filter: Option<&regex::Regex>,
) -> impl Iterator<Item = &'a HistoryEntry> {
    entries
        .iter()
        .filter(move |entry| filter.is_none_or(|filter| filter.is_match(&entry.sql)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::store::set_config_dir_for_test;

    struct ScratchDir(PathBuf);

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A scratch config dir for this test's thread, with history on disk.
    fn scratch(retention: Retention) -> ScratchDir {
        let dir = std::env::temp_dir().join(format!(
            "zippa-db-history-tests-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("the scratch dir should exist");
        set_config_dir_for_test(dir.clone());
        configure(retention);
        ScratchDir(dir)
    }

    fn on_disk() -> Retention {
        Retention::default()
    }

    fn entry(connection: Uuid, sql: &str) -> HistoryEntry {
        HistoryEntry::new(
            connection,
            "app",
            sql,
            QueryOutcome::Rows(1),
            Duration::from_millis(3),
        )
    }

    fn aged(connection: Uuid, sql: &str, days: i64) -> HistoryEntry {
        HistoryEntry {
            at: Utc::now() - chrono::Duration::days(days),
            ..entry(connection, sql)
        }
    }

    fn sql_of(entries: &[HistoryEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.sql.as_str()).collect()
    }

    #[test]
    fn a_recorded_statement_reads_back_newest_first() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        record(entry(connection, "select 1"));
        record(entry(connection, "select 2"));

        assert_eq!(sql_of(&entries(connection)), ["select 2", "select 1"]);
    }

    #[test]
    fn each_connection_keeps_its_own_file() {
        let _dir = scratch(on_disk());
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        record(entry(first, "select 'first'"));
        record(entry(second, "select 'second'"));

        assert_eq!(sql_of(&entries(first)), ["select 'first'"]);
        assert_eq!(sql_of(&entries(second)), ["select 'second'"]);
        assert!(history_file(first).expect("a path").exists());
        assert!(history_file(second).expect("a path").exists());
        assert_eq!(
            sql_of(&all_entries()),
            ["select 'second'", "select 'first'"],
            "every connection's history merges by time"
        );
    }

    #[test]
    fn the_file_is_owner_only() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        record(entry(connection, "select 'secret'"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(history_file(connection).expect("a path"))
                .expect("the file should exist")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn memory_mode_writes_nothing_to_disk() {
        let _dir = scratch(Retention {
            keep: Keep::Memory,
            ..Retention::default()
        });
        let connection = Uuid::new_v4();
        record(entry(connection, "select 1"));

        assert_eq!(sql_of(&entries(connection)), ["select 1"]);
        assert!(!history_file(connection).expect("a path").exists());
    }

    #[test]
    fn off_records_nothing() {
        let _dir = scratch(Retention {
            keep: Keep::Off,
            ..Retention::default()
        });
        let connection = Uuid::new_v4();
        record(entry(connection, "select 1"));

        assert!(entries(connection).is_empty());
        assert!(!history_file(connection).expect("a path").exists());
    }

    #[test]
    fn a_blank_statement_is_not_recorded() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        record(entry(connection, "   \n"));
        assert!(entries(connection).is_empty());
    }

    #[test]
    fn the_count_cap_keeps_the_newest() {
        let _dir = scratch(Retention {
            max_entries: 3,
            ..Retention::default()
        });
        let connection = Uuid::new_v4();
        for index in 0..30 {
            record(entry(connection, &format!("select {index}")));
        }

        assert_eq!(
            sql_of(&entries(connection)),
            ["select 29", "select 28", "select 27"]
        );
        let lines = fs::read_to_string(history_file(connection).expect("a path"))
            .expect("the file should exist")
            .lines()
            .count();
        assert!(
            lines <= 3 + Retention::default().compact_every().min(20),
            "compaction keeps the file near the cap, not {lines} lines"
        );
    }

    #[test]
    fn the_age_limit_drops_old_entries() {
        let retention = Retention {
            max_age_days: 30,
            ..Retention::default()
        };
        let connection = Uuid::new_v4();
        let kept = retention.apply(
            vec![aged(connection, "old", 45), aged(connection, "recent", 2)],
            Utc::now(),
        );
        assert_eq!(sql_of(&kept), ["recent"]);
    }

    #[test]
    fn lowering_the_cap_compacts_the_file() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        for index in 0..10 {
            record(entry(connection, &format!("select {index}")));
        }

        configure(Retention {
            max_entries: 2,
            ..Retention::default()
        });
        let text = fs::read_to_string(history_file(connection).expect("a path"))
            .expect("the file should exist");
        assert_eq!(text.lines().count(), 2);
        assert_eq!(sql_of(&entries(connection)), ["select 9", "select 8"]);
    }

    #[test]
    fn a_torn_line_costs_only_itself() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        record(entry(connection, "select 1"));
        let path = history_file(connection).expect("a path");
        // A crash mid-write leaves half a line with no newline.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the file should open");
        file.write_all(br#"{"at":"2026-"#)
            .expect("the torn line should write");
        drop(file);

        record(entry(connection, "select 2"));
        assert_eq!(sql_of(&entries(connection)), ["select 2", "select 1"]);
    }

    #[test]
    fn a_line_torn_mid_character_costs_only_itself() {
        let _dir = scratch(on_disk());
        let connection = Uuid::new_v4();
        record(entry(connection, "select 'café'"));
        let path = history_file(connection).expect("a path");
        // Half of a two-byte character, then the crash.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the file should open");
        file.write_all(b"{\"sql\":\"caf\xc3")
            .expect("the torn line should write");
        drop(file);

        record(entry(connection, "select 2"));
        assert_eq!(sql_of(&entries(connection)), ["select 2", "select 'café'"]);
    }

    #[test]
    fn an_age_limit_past_any_date_keeps_everything() {
        let retention = Retention {
            max_age_days: u32::MAX,
            ..Retention::default()
        };
        let connection = Uuid::new_v4();
        let kept = retention.apply(vec![aged(connection, "old", 400)], Utc::now());
        assert_eq!(sql_of(&kept), ["old"]);
    }

    #[test]
    fn clearing_one_connection_keeps_the_others() {
        let _dir = scratch(on_disk());
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        record(entry(first, "select 1"));
        record(entry(second, "select 2"));

        clear(Some(first));
        assert!(entries(first).is_empty());
        assert!(!history_file(first).expect("a path").exists());
        assert_eq!(sql_of(&entries(second)), ["select 2"]);

        clear(None);
        assert!(all_entries().is_empty());
        assert!(!history_dir().expect("a path").exists());
    }

    #[test]
    fn a_long_statement_is_cut_short_and_says_so() {
        let sql = "x".repeat(MAX_SQL_BYTES + 1000);
        let entry = entry(Uuid::new_v4(), &sql);
        assert!(entry.sql.len() < sql.len());
        assert!(entry.sql.starts_with(&sql[..MAX_SQL_BYTES]));
        assert!(
            entry
                .sql
                .ends_with("1000 more bytes were not kept in history)")
        );
    }

    #[test]
    fn an_entry_survives_a_round_trip_through_a_line() {
        let entry = HistoryEntry {
            outcome: QueryOutcome::Error("syntax error".into()),
            ..entry(Uuid::new_v4(), "selec 1")
        };
        let line = serde_json::to_string(&entry).expect("the entry should serialize");
        assert!(line.contains(r#""outcome":{"error":"syntax error"}"#));
        let read: HistoryEntry = serde_json::from_str(&line).expect("the line should parse");
        assert_eq!(read, entry);
    }

    #[test]
    fn the_search_matches_sql_case_insensitively() {
        let connection = Uuid::new_v4();
        let entries = vec![
            entry(connection, "SELECT * FROM users"),
            entry(connection, "select * from orders"),
        ];
        let filter = regex::RegexBuilder::new("USERS")
            .case_insensitive(true)
            .build()
            .expect("the pattern should compile");
        let found: Vec<_> = matching(&entries, Some(&filter)).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].sql, "SELECT * FROM users");
    }
}
