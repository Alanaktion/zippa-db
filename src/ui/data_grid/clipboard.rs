//! Copying out of the grid: `Cmd+C`, `Cmd+Shift+C`, the row menu's `Copy as`
//! submenu, and [`DataGrid::snapshot`], the one place that decides which rows
//! and cells a copy or an export takes.

use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Context, Window};

use crate::db::Engine;
use crate::db::export::{self, Format};
use crate::db::query::{self, Cell};

use super::{Copied, CopyAs, CopyValue, CopyWithHeaders, DataGrid, Scope, Snapshot};

impl DataGrid {
    pub(super) fn on_copy_value(
        &mut self,
        _: &CopyValue,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy_selection(cx);
    }

    pub(super) fn on_copy_with_headers(
        &mut self,
        _: &CopyWithHeaders,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.copy_as(CopyAs::Rows(Format::Tsv), cx);
    }

    /// Put what is selected on the clipboard.
    ///
    /// Rows picked out win over the selected cell: once a row is checked, a
    /// row is what the user is working with. Rows go out as one line each,
    /// columns separated by tabs, which is what a spreadsheet reads back as
    /// cells. A `NULL` copies as nothing, the way an empty cell does.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        self.copy(None, cx);
    }

    /// Copy one cell, or whatever is selected when no cell is named — which is
    /// how the row menu copies the cell the click landed on rather than the
    /// one the selection happens to be on.
    pub(super) fn copy(&mut self, cell: Option<(usize, usize)>, cx: &mut Context<Self>) {
        self.commit_editor(cx);

        if let Some((row_ix, col_ix)) = cell {
            self.copy_cell(row_ix, col_ix, cx);
            return;
        }

        if !self.table.read(cx).delegate().rows_selected.is_empty() {
            let (snapshot, total) = self.snapshot_capped(Scope::Picked, MAX_COPY_ROWS, cx);
            let rows = snapshot.rows.len();
            let mut skipped = 0;
            let text = snapshot
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| {
                            let (text, read_back) = cell_text(cell);
                            skipped += usize::from(!read_back);
                            // A tab or a line break inside a value would split
                            // the row when pasted, so it goes as a space — the
                            // same as Cmd+Shift+C's copy with headers.
                            crate::db::export::tsv_field(&text)
                        })
                        .collect::<Vec<String>>()
                        .join("\t")
                })
                .collect::<Vec<String>>()
                .join("\n");
            let (text, capped_bytes) = cap_bytes(text);
            let message = if rows < total {
                format!("Copied first {rows} of {total} rows")
            } else if capped_bytes {
                format!("Copied the first {} MB", MAX_COPY_BYTES / (1024 * 1024))
            } else {
                format!("Copied {}", count_of(rows, "row", "rows"))
            };
            self.put_on_clipboard(text, with_skipped(message, skipped), cx);
            return;
        }

        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
            return;
        };
        self.copy_cell(row_ix, col_ix, cx);
    }

    /// Copy one cell's value, a stand-in going out as the `NULL` it would be
    /// in any `Copy as` shape.
    fn copy_cell(&mut self, row_ix: usize, col_ix: usize, cx: &mut Context<Self>) {
        let (text, read_back) = cell_text(self.table.read(cx).delegate().cell(row_ix, col_ix));
        let message = if read_back {
            "Copied value".to_string()
        } else {
            "Value not read back, copied as NULL".to_string()
        };
        self.put_on_clipboard(text, message, cx);
    }

    /// Copy what a `Copy as` menu item asked for, in the shape it named.
    pub fn copy_as(&mut self, what: CopyAs, cx: &mut Context<Self>) {
        self.commit_editor(cx);

        match what {
            CopyAs::Rows(format) => {
                let (snapshot, total) = self.snapshot_capped(self.row_scope(cx), MAX_COPY_ROWS, cx);
                self.copy_snapshot(format, snapshot, total, cx);
            }
            CopyAs::ColumnValues(column) => {
                let snapshot = self.snapshot(Scope::Column(column), cx);
                let cells = column_cells(&snapshot);
                let rendered = export::values(&cells);
                let message = with_skipped(
                    format!("Copied {}", count_of(cells.len(), "value", "values")),
                    rendered.skipped,
                );
                self.put_on_clipboard(rendered.text, message, cx);
            }
            CopyAs::ColumnInList(column) => {
                let snapshot = self.snapshot(Scope::Column(column), cx);
                let cells = column_cells(&snapshot);
                let type_name = snapshot.types.first().cloned().unwrap_or_default();
                let rendered = export::in_list(self.engine(cx), &type_name, &cells);
                let copied = cells
                    .iter()
                    .filter(|cell| cell.is_some() && !query::is_placeholder(cell))
                    .count();
                let message = if copied == 0 {
                    "No values to copy".to_string()
                } else {
                    with_skipped(
                        format!(
                            "Copied {} as an IN list",
                            count_of(copied, "value", "values")
                        ),
                        rendered.skipped,
                    )
                };
                self.put_on_clipboard(rendered.text, message, cx);
            }
        }
    }

    /// Lay a snapshot out in `format` and copy it, capping a result too large
    /// to put on the clipboard whole.
    fn copy_snapshot(
        &mut self,
        format: Format,
        snapshot: Snapshot,
        total: usize,
        cx: &mut Context<Self>,
    ) {
        let rows = snapshot.rows.len();

        let engine = self.engine(cx);
        let table = self.table_name(cx).unwrap_or_default();

        // Laying the rows out is O(rows×cols) string work; it runs on the
        // background executor so a big copy does not stall the UI.
        cx.spawn(async move |this, cx| {
            let rendered = cx
                .background_spawn(async move {
                    export::render(
                        format,
                        engine,
                        &table,
                        &snapshot.columns,
                        &snapshot.types,
                        &snapshot.rows,
                    )
                })
                .await;
            let (text, capped_bytes) = cap_bytes(rendered.text);

            let message = if rows < total {
                format!("Copied first {rows} of {total} rows as {}", format.label())
            } else if capped_bytes {
                format!(
                    "Copied the first {} MB as {}",
                    MAX_COPY_BYTES / (1024 * 1024),
                    format.label()
                )
            } else {
                format!(
                    "Copied {} as {}",
                    count_of(rows, "row", "rows"),
                    format.label()
                )
            };

            this.update_in(cx, |this, _window, cx| {
                this.put_on_clipboard(text, with_skipped(message, rendered.skipped), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Put `text` on the clipboard and tell the owner what it was.
    fn put_on_clipboard(&mut self, text: String, message: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.emit(Copied { message });
    }

    /// The rows a copy acts on: the picked ones when any are picked, every row
    /// shown otherwise.
    fn row_scope(&self, cx: &App) -> Scope {
        if self.table.read(cx).delegate().rows_selected.is_empty() {
            Scope::All
        } else {
            Scope::Picked
        }
    }

    /// The engine a copy quotes its values for.
    pub(super) fn engine(&self, cx: &App) -> Engine {
        self.table.read(cx).delegate().engine
    }

    /// The table a SQL copy writes an `INSERT` against, if the owner named
    /// one.
    pub(super) fn table_name(&self, cx: &App) -> Option<String> {
        self.table.read(cx).delegate().table.clone()
    }

    /// The rows a [`Scope`] names, with the columns and driver types they were
    /// read under and every cell as it stands.
    ///
    /// This is the one place that decides what a copy or an export acts on:
    /// cells come through the delegate's own `cell`, so a staged edit goes out
    /// as it stands, and a row being built by hand is a row like any other.
    pub fn snapshot(&self, scope: Scope, cx: &App) -> Snapshot {
        self.snapshot_capped(scope, usize::MAX, cx).0
    }

    /// [`Self::snapshot`], taking at most `limit` rows, plus how many rows the
    /// scope held. Rows past the cap are never cloned, so a copy of a huge
    /// result costs the rows it copies rather than the whole result.
    fn snapshot_capped(&self, scope: Scope, limit: usize, cx: &App) -> (Snapshot, usize) {
        let delegate = self.table.read(cx).delegate();

        let mut row_ixs: Vec<usize> = match scope {
            Scope::All => (0..delegate.rows()).collect(),
            Scope::Picked => delegate.rows_selected.iter().copied().collect(),
            // A column is copied whole: the picked rows when any are picked,
            // and every row otherwise.
            Scope::Column(_) if delegate.rows_selected.is_empty() => (0..delegate.rows()).collect(),
            Scope::Column(_) => delegate.rows_selected.iter().copied().collect(),
        };
        row_ixs.sort_unstable();
        let total = row_ixs.len();
        row_ixs.truncate(limit);

        let (columns, types): (Vec<String>, Vec<String>) = match scope {
            Scope::Column(column) => (
                delegate
                    .result
                    .columns
                    .get(column)
                    .cloned()
                    .into_iter()
                    .collect(),
                delegate
                    .result
                    .column_types
                    .get(column)
                    .cloned()
                    .into_iter()
                    .collect(),
            ),
            _ => (
                delegate.result.columns.clone(),
                delegate.result.column_types.clone(),
            ),
        };
        let col_ixs: Vec<usize> = match scope {
            Scope::Column(column) => vec![column; columns.len()],
            _ => (0..columns.len()).collect(),
        };

        let rows = row_ixs
            .iter()
            .map(|row_ix| {
                col_ixs
                    .iter()
                    .map(|col_ix| delegate.cell(*row_ix, *col_ix).clone())
                    .collect()
            })
            .collect();

        (
            Snapshot {
                columns,
                types,
                rows,
            },
            total,
        )
    }
}

/// The most rows one copy puts on the clipboard. The payload is built on the
/// UI thread, so a result past this is cut short — before its rows are cloned
/// — and the notice says which part went.
const MAX_COPY_ROWS: usize = 100_000;

/// The most text one copy puts on the clipboard, for a result that is few rows
/// but each a very large value.
const MAX_COPY_BYTES: usize = 50 * 1024 * 1024;

/// The single column of a [`Scope::Column`] snapshot, as cells.
fn column_cells(snapshot: &Snapshot) -> Vec<Cell> {
    snapshot
        .rows
        .iter()
        .map(|row| row.first().cloned().unwrap_or(None))
        .collect()
}

/// One cell as `Cmd+C` writes it, and whether it was a real value: a `NULL`
/// copies as nothing, and so does a stand-in for a value that was never read
/// back (`query::is_placeholder`), the way the TSV `Copy as` writes both —
/// the description is not the value, and pasting it somewhere would pass it
/// off as one.
fn cell_text(cell: &Cell) -> (String, bool) {
    if query::is_placeholder(cell) {
        return (String::new(), false);
    }
    (cell.clone().unwrap_or_default(), true)
}

/// `1 row` / `3 rows`, for a notice.
fn count_of(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Add how many cells could not be read back, when any could not.
fn with_skipped(mut message: String, skipped: usize) -> String {
    if skipped > 0 {
        message.push_str(&format!(
            " ({} not read back, copied as NULL)",
            count_of(skipped, "value", "values")
        ));
    }
    message
}

/// Keep a payload under [`MAX_COPY_BYTES`], on a character boundary. A
/// structured format cut this way is no longer valid, which is why the notice
/// says so rather than pretending the copy is whole.
fn cap_bytes(mut text: String) -> (String, bool) {
    if text.len() <= MAX_COPY_BYTES {
        return (text, false);
    }
    let mut end = MAX_COPY_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}
