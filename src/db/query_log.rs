//! A bounded record of every statement this connection has sent to the
//! server — the user's own runs and the app's own reads and writes alike —
//! for a console pane to show.
//!
//! Kept on [`super::Connection`] itself rather than pushed anywhere: `db/`
//! has no UI to push to, and every statement already passes through one of
//! a handful of shared entry points (`run_query_with`, `run_script`,
//! `execute`, `execute_script`), so recording it there costs the rest of the
//! app nothing. A console reads [`QueryLog::snapshot`] when it is opened and
//! when asked to refresh; nothing here wakes one on its own.
//!
//! Not covered: `import_dump` and `rebuild_table` run on a dedicated
//! connection outside the pool this log sits on, and already have their own
//! progress reporting in the UI.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Where a logged statement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySource {
    /// The exact text a query tab sent — run, run as a script, or explained.
    User,
    /// Everything else the app sent on its own: the catalog, a table view's
    /// paging/sort/filter reads, a staged edit going out as a write, DDL from
    /// the structure tab.
    Internal,
}

/// How a logged statement ended.
#[derive(Debug, Clone)]
pub enum QueryOutcome {
    /// A read: this many rows came back.
    Rows(usize),
    /// A write: this many rows were reported matched or changed.
    Affected(u64),
    /// Ran with no row count to report — a DDL batch, run as one unit.
    Ran,
    /// The statement failed; the text is what the driver or the app said.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LoggedQuery {
    pub sql: String,
    pub source: QuerySource,
    pub outcome: QueryOutcome,
    pub elapsed: Duration,
    pub at: SystemTime,
}

/// Entries past this many are dropped, oldest first, so a long session's log
/// costs a fixed amount of memory rather than growing without bound.
const CAPACITY: usize = 500;

#[derive(Debug, Default)]
pub struct QueryLog(Mutex<VecDeque<LoggedQuery>>);

impl QueryLog {
    /// Add one statement, dropping the oldest once [`CAPACITY`] is reached.
    pub(crate) fn record(&self, entry: LoggedQuery) {
        let mut log = self.lock();
        if log.len() >= CAPACITY {
            log.pop_front();
        }
        log.push_back(entry);
    }

    /// Every entry held, oldest first.
    pub fn snapshot(&self) -> Vec<LoggedQuery> {
        self.lock().iter().cloned().collect()
    }

    /// Forget everything logged so far.
    pub fn clear(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<LoggedQuery>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str) -> LoggedQuery {
        LoggedQuery {
            sql: sql.to_string(),
            source: QuerySource::Internal,
            outcome: QueryOutcome::Rows(0),
            elapsed: Duration::ZERO,
            at: SystemTime::now(),
        }
    }

    #[test]
    fn keeps_entries_in_order() {
        let log = QueryLog::default();
        log.record(entry("select 1"));
        log.record(entry("select 2"));

        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].sql, "select 1");
        assert_eq!(snapshot[1].sql, "select 2");
    }

    #[test]
    fn drops_the_oldest_past_capacity() {
        let log = QueryLog::default();
        for index in 0..CAPACITY + 10 {
            log.record(entry(&index.to_string()));
        }

        let snapshot = log.snapshot();
        assert_eq!(snapshot.len(), CAPACITY);
        assert_eq!(snapshot[0].sql, "10", "the first 10 should have aged out");
        assert_eq!(snapshot[snapshot.len() - 1].sql, (CAPACITY + 9).to_string());
    }

    #[test]
    fn clear_empties_the_log() {
        let log = QueryLog::default();
        log.record(entry("select 1"));
        log.clear();
        assert!(log.snapshot().is_empty());
    }
}
