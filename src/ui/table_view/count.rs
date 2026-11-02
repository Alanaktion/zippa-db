//! How many rows the table view's filters and search match, for the footer's
//! `Rows 501–1,000 of 48,210 · page 2 of 97`.
//!
//! Counted after the page lands, never before it, so a slow count does not
//! hold the rows back, and a count that fails just reads as unknown. A count
//! stays good across pages and sorts under the same filters, so only a change
//! of filter or search, a refresh, or a write counts again. A short page
//! answers for itself: its last row is the last row there is.
//!
//! On Postgres and MySQL a large table is not counted unasked: the server's
//! own estimate (`pg_class.reltuples`, `information_schema.tables.table_rows`)
//! is shown as `~48k`, and the footer's Count button gets the exact number.
//! Filtered rows of a large table have no estimate, so they wait for the
//! button too.

use gpui_kit::Context;

use crate::db::runtime;

use super::TableView;

/// A table the server estimates at this many rows or more is not counted
/// until asked.
const LARGE_TABLE: u64 = 100_000;

/// What the view knows of how many rows match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RowCount {
    /// Not counted, or the count failed.
    #[default]
    Unknown,
    /// A count is on its way.
    Counting,
    /// The server's estimate of the whole table.
    Estimated(u64),
    /// A large table under filters: counted only when asked.
    NotCounted,
    Exact(u64),
}

impl RowCount {
    /// Whether the footer offers to count exactly.
    pub(crate) fn can_count(self) -> bool {
        matches!(self, RowCount::Estimated(_) | RowCount::NotCounted)
    }

    pub(crate) fn exact(self) -> Option<u64> {
        match self {
            RowCount::Exact(rows) => Some(rows),
            _ => None,
        }
    }
}

/// How a count came back from the runtime.
enum Counted {
    Exact(u64),
    Estimated(u64),
    /// A large table under filters, left for the Count button.
    Deferred,
}

impl TableView {
    /// Bring the count up to date with the page that just landed.
    ///
    /// `keep` is set for a page turn, a sort, or a new limit, which change
    /// which rows are on screen but not how many match.
    pub(super) fn update_count(&mut self, keep: bool, cx: &mut Context<Self>) {
        let key = self.count_key(cx);

        // A short page is the last one, so it knows the total without asking.
        if self.loaded_rows < self.limit && (self.loaded_rows > 0 || self.page == 0) {
            self.count_read += 1;
            self.total = RowCount::Exact((self.page * self.limit + self.loaded_rows) as u64);
            self.counted_for = Some(key);
            return;
        }

        if keep && self.counted_for.as_ref() == Some(&key) && self.total != RowCount::Unknown {
            return;
        }
        self.count_rows(false, cx);
    }

    /// Count the matching rows: exactly when `exact`, otherwise by estimate
    /// first on a server that keeps one.
    pub(super) fn count_rows(&mut self, exact: bool, cx: &mut Context<Self>) {
        let key = self.count_key(cx);
        let narrowed = self.is_narrowed(cx);
        let (sql, params) = self.count_sql(cx);
        let connection = self.connection.clone();
        let object = self.object.clone();

        self.count_read += 1;
        let read = self.count_read;
        self.total = RowCount::Counting;
        self.counted_for = Some(key);
        cx.notify();

        let task = runtime::spawn(async move {
            if !exact
                && let Ok(Some(estimate)) = connection.estimated_rows(&object).await
                && estimate >= LARGE_TABLE
            {
                return Ok(match narrowed {
                    true => Counted::Deferred,
                    false => Counted::Estimated(estimate),
                });
            }
            connection
                .count_rows(&sql, params)
                .await
                .map(Counted::Exact)
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // Overtaken by a newer count, or by a short page that
                // answered for itself.
                if this.count_read != read {
                    return;
                }
                this.total = match result {
                    Ok(Ok(Counted::Exact(rows))) => RowCount::Exact(rows),
                    Ok(Ok(Counted::Estimated(rows))) => RowCount::Estimated(rows),
                    Ok(Ok(Counted::Deferred)) => RowCount::NotCounted,
                    // A count that fails is not an error the user has to
                    // read: the footer just leaves the total out.
                    Ok(Err(_)) | Err(_) => RowCount::Unknown,
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Forget the count, as when the database is switched.
    pub(super) fn forget_count(&mut self) {
        self.count_read += 1;
        self.total = RowCount::Unknown;
        self.counted_for = None;
    }
}
