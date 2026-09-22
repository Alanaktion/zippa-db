//! A table opened from the sidebar: the grid, paged, with no editor.
//!
//! TODO.md section 2 ("Configurable row limit & offset pagination"), the
//! sorting half of "Multi-column sorting", and the buffer model for inline
//! edits: the grid stages what is typed into it and this view turns a row's
//! staged cells into an `UPDATE`, on leaving the row or on demand.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    InputEvent, InputState, NumberInput, NumberInputEvent, StepAction,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::table::ColumnSort;
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, ResizableState, Sizable, h_flex, h_resizable,
    resizable_panel, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Window, actions, div, px,
};

use std::collections::HashSet;
use std::path::Path;

use crate::db::export::{self, Format};
use crate::db::query::{Cell, QueryResult};
use crate::db::{Connection, DatabaseObject, ForeignKeyDef, ObjectKind, RowKey, runtime};
use crate::settings::{self, Settings};
use crate::ui::data_grid::{
    Copied, DataGrid, ExportRequested, GridEdit, GridNavigate, Scope, SortRequested, Sorting,
    StagedRow,
};
use crate::ui::filter_bar::{FilterBar, FilterSpec, FiltersChanged, Operator};
use crate::ui::sql_file;

mod row_panel;
mod sql;

pub(crate) use row_panel::RowPanel;
use row_panel::RowPanelEvent;

actions!(
    zippa_db,
    [
        ApplyEdits,
        DiscardEdits,
        EditCell,
        SetNull,
        CancelEdit,
        InsertRow,
        DeleteRows,
        RestoreRows,
        ToggleRowPanel
    ]
);

/// Count the changes in hand as the user sees them: new rows, edited rows,
/// deleted rows. Parts with nothing in them are left out.
fn change_summary(edited: usize, inserted: usize, deleted: usize) -> String {
    let mut parts = Vec::new();
    if inserted > 0 {
        parts.push(format!("{inserted} new"));
    }
    if edited > 0 {
        parts.push(format!("{edited} edited"));
    }
    if deleted > 0 {
        parts.push(format!("{deleted} deleted"));
    }

    let rows = inserted + edited + deleted;
    let unit = if rows == 1 { "row" } else { "rows" };
    match parts.len() {
        0 => String::new(),
        1 => format!("{} {unit}", parts[0]),
        _ => format!("{} {unit}", parts.join(", ")),
    }
}

/// The same counts, in the past tense, for the footer after a write.
fn applied_summary(edited: usize, inserted: usize, deleted: usize) -> String {
    let summary = change_summary(edited, inserted, deleted);
    if summary.is_empty() {
        return "Nothing to write".to_string();
    }
    format!("Wrote {summary}")
}

/// What the footer says after an export: where it went, and — when the driver
/// had only a description of some values — how many were written as `NULL`.
fn export_notice(rows: usize, path: &Path, skipped: usize) -> String {
    let unit = if rows == 1 { "row" } else { "rows" };
    let mut notice = format!("Exported {rows} {unit} to {}", path.display());
    match skipped {
        0 => {}
        1 => notice.push_str(" (1 value could not be read back and was written as NULL)"),
        skipped => notice.push_str(&format!(
            " ({skipped} values could not be read back and were written as NULL)"
        )),
    }
    notice
}

/// A write built and waiting for the user to say yes, on a connection that
/// confirms writes.
#[derive(Debug, Clone)]
struct Confirming {
    /// Statements and the values bound to them, in the order they will run.
    statements: Vec<(String, Vec<Cell>)>,
    /// Rows the statements write, for the grid once they have run. Empty when
    /// the page is read again instead.
    rows: Vec<usize>,
    /// Whether the page has to be read again afterwards.
    stale: bool,
    /// What the footer says once the statements have run.
    summary: String,
    /// What the panel asks before they run.
    question: String,
}

/// Something that would replace the rows in the grid, held back because the
/// rows in hand have edits that have not been written.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Reload,
    Page(usize),
    Sort(Option<(String, ColumnSort)>),
    Limit(usize),
    Filter,
}

impl Pending {
    /// What this would do to the page, for the question in the footer.
    fn label(&self) -> &'static str {
        match self {
            Pending::Reload => "Refreshing",
            Pending::Page(_) => "Turning the page",
            Pending::Sort(_) => "Sorting",
            Pending::Limit(_) => "Changing the row limit",
            Pending::Filter => "Filtering",
        }
    }
}

/// What a table view asks its owner to do, since opening or reusing a tab is
/// the session's job, not the view's own.
pub(crate) enum TableViewEvent {
    /// A cell's foreign key was followed; `filter` selects the matching row
    /// in `object` once its tab is open.
    NavigateToForeignKey {
        object: DatabaseObject,
        filter: FilterSpec,
    },
}

impl EventEmitter<TableViewEvent> for TableView {}

pub struct TableView {
    connection: Arc<Connection>,
    filters: Entity<FilterBar>,
    object: DatabaseObject,
    grid: Entity<DataGrid>,
    /// The side panel that lays the focused row out one field per column.
    row_panel: Entity<RowPanel>,
    row_panel_visible: bool,
    columns_pane: Entity<ResizableState>,
    limit_input: Entity<InputState>,
    limit: usize,
    page: usize,
    /// Column and direction of the current `ORDER BY`.
    sort: Option<(String, ColumnSort)>,
    /// Rows the last page returned, used to decide whether there is a next one.
    loaded_rows: usize,
    loading: bool,
    error: Option<String>,
    /// How a row of this table can be addressed by a write; `None` until the
    /// server has been asked.
    row_key: Option<RowKey>,
    /// Column names and driver type names of the page in the grid, kept so a
    /// write can quote its columns and cast its parameters.
    columns: Vec<String>,
    column_types: Vec<String>,
    /// This table's own foreign keys, read once per table opened; `None`
    /// until the server has answered, like `row_key`.
    foreign_keys: Option<Vec<ForeignKeyDef>>,
    /// Row identifiers of the loaded page, when the key is one; indexed the
    /// same way the grid indexes its rows.
    key_values: Vec<Cell>,
    /// Driver type name of that identifier, for the cast Postgres wants.
    key_type: Option<String>,
    committing: bool,
    /// What the last write did, for the footer.
    notice: Option<String>,
    /// An action waiting on an answer about the staged edits it would lose.
    pending: Option<Pending>,
    /// A write waiting on an answer about whether to run at all.
    confirming: Option<Confirming>,
    /// A column to select once a page lands, set by a schema search that opened
    /// this table and cleared once the grid has it.
    reveal: Option<String>,
}

impl TableView {
    pub fn new(
        connection: Arc<Connection>,
        object: DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| {
            DataGrid::with_sorting(Sorting::Delegated, connection.config.engine, window, cx)
        });
        cx.subscribe_in(&grid, window, Self::on_sort_requested)
            .detach();
        cx.subscribe_in(&grid, window, Self::on_grid_edit).detach();
        cx.subscribe_in(&grid, window, Self::on_grid_navigate)
            .detach();
        cx.subscribe_in(&grid, window, Self::on_grid_export)
            .detach();
        cx.subscribe_in(&grid, window, Self::on_grid_copied)
            .detach();
        // The grid quotes copies for this connection's engine, and names this
        // table in a SQL `INSERT` copy; only this view knows either.
        grid.update(cx, |grid, cx| grid.set_table(Some(object.name.clone()), cx));

        let row_panel = cx.new(|cx| RowPanel::new(grid.clone(), window, cx));
        cx.subscribe_in(&row_panel, window, Self::on_row_panel_event)
            .detach();

        let filters = cx.new(|cx| FilterBar::new(window, cx));
        cx.subscribe_in(&filters, window, Self::on_filters_changed)
            .detach();

        // A table takes the page size the settings had when it was opened;
        // changing the setting later leaves open tables where they are.
        let limit = Settings::global(cx)
            .page_size
            .clamp(1, settings::MAX_PAGE_SIZE);

        let limit_input = cx.new(|cx| InputState::new(window, cx).default_value(limit.to_string()));
        cx.subscribe_in(&limit_input, window, Self::on_limit_event)
            .detach();
        // Left unset on the state itself (see `on_limit_step`): with `min`,
        // `max`, or `step` set there, `NumberInput`'s own +/- buttons would
        // update the value and emit `InputEvent::Change` internally, which
        // is what `on_limit_event` ignores to avoid firing a query per
        // keystroke while typing.
        cx.subscribe_in(&limit_input, window, Self::on_limit_step)
            .detach();

        let mut view = Self {
            connection,
            filters,
            object,
            grid,
            row_panel,
            row_panel_visible: true,
            columns_pane: cx.new(|_| ResizableState::default()),
            limit_input,
            limit,
            page: 0,
            sort: None,
            loaded_rows: 0,
            loading: false,
            error: None,
            row_key: None,
            columns: Vec::new(),
            column_types: Vec::new(),
            foreign_keys: None,
            key_values: Vec::new(),
            key_type: None,
            committing: false,
            notice: None,
            pending: None,
            confirming: None,
            reveal: None,
        };
        view.load_row_key(cx);
        view.load_foreign_keys(cx);
        view
    }

    fn on_row_panel_event(
        &mut self,
        _: &Entity<RowPanel>,
        event: &RowPanelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            RowPanelEvent::CloseRequested => {
                self.row_panel_visible = false;
                cx.notify();
            }
        }
    }

    fn on_toggle_row_panel(&mut self, _: &ToggleRowPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.row_panel_visible = !self.row_panel_visible;
        cx.notify();
    }

    pub fn object(&self) -> &DatabaseObject {
        &self.object
    }

    /// Point the view at a new connection, as when the database is switched.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.page = 0;
        self.sort = None;
        // A table of the same name in another database is another table, so
        // the key is read again rather than carried over.
        self.row_key = None;
        self.foreign_keys = None;
        self.load_row_key(cx);
        self.load_foreign_keys(cx);
    }

    /// Ask the server how a row of this table can be addressed, then load it.
    ///
    /// Read once per table rather than per page: the answer only changes when
    /// the table's own definition does.
    fn load_row_key(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        cx.notify();

        let connection = self.connection.clone();
        let object = self.object.clone();
        let task = runtime::spawn(async move { connection.row_key(&object).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // A table whose key could not be read is shown, just not
                // written to; the reason sits in the footer.
                this.row_key = Some(match result {
                    Ok(Ok(key)) => key,
                    Ok(Err(_)) | Err(_) => RowKey::Unavailable("the primary key could not be read"),
                });

                let editable = this.is_editable();
                this.grid
                    .update(cx, |grid, cx| grid.set_editable(editable, cx));
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Ask the server for this table's foreign keys, once per table opened —
    /// the row menu needs them to offer a jump to the referenced row.
    fn load_foreign_keys(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let object = self.object.clone();
        let task = runtime::spawn(async move { connection.foreign_keys(&object).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.foreign_keys = Some(match result {
                    Ok(Ok(keys)) => keys,
                    Ok(Err(_)) | Err(_) => Vec::new(),
                });
                this.sync_foreign_keys(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Tell the grid which of its current columns are backed by a
    /// single-column foreign key, from whichever of the schema read and the
    /// page load has landed last.
    ///
    /// Composite foreign keys are left out: the filter bar only describes one
    /// column per row, so there is no filter a jump could pre-set for them.
    fn sync_foreign_keys(&mut self, cx: &mut Context<Self>) {
        let Some(foreign_keys) = &self.foreign_keys else {
            return;
        };

        let fk_columns: HashSet<usize> = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                foreign_keys
                    .iter()
                    .any(|fk| fk.columns.len() == 1 && fk.columns[0] == *name)
                    .then_some(index)
            })
            .collect();

        self.grid
            .update(cx, |grid, cx| grid.set_foreign_keys(fk_columns, cx));
    }

    /// Whether rows of this table can be written back.
    fn is_editable(&self) -> bool {
        !self.connection.config.safety.is_read_only()
            && self.object.kind == ObjectKind::Table
            && matches!(&self.row_key, Some(key) if key.is_available())
    }

    /// Why the table is read-only, when it is and the user should know.
    fn read_only_reason(&self) -> Option<&'static str> {
        if self.connection.config.safety.is_read_only() {
            return Some("this connection is read-only");
        }
        match &self.row_key {
            Some(RowKey::Unavailable(reason)) => Some(reason),
            _ => None,
        }
    }

    /// Rebuild the paging SQL and re-run it. Also the table half of the
    /// session's refresh action.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let (sql, params) = self.query_with_params(cx);
        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.run_query_with(&sql, params).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(mut query_result)) => {
                        this.take_key_column(&mut query_result);
                        this.loaded_rows = query_result.row_count();
                        this.columns = query_result.columns.clone();
                        this.column_types = query_result.column_types.clone();
                        let columns = this.columns.clone();
                        this.filters
                            .update(cx, |filters, cx| filters.set_columns(&columns, cx));
                        let column_types = this.column_types.clone();
                        this.row_panel.update(cx, |panel, cx| {
                            panel.sync_columns(&columns, &column_types, window, cx)
                        });
                        this.sync_foreign_keys(cx);
                        // The rows come back already ordered, so the grid is
                        // told what order they are in: it rebuilds its headers
                        // from scratch and would otherwise show the column as
                        // unsorted and restart its cycle on the next click.
                        let sort = this.sort.clone();
                        this.grid.update(cx, |grid, cx| {
                            grid.set_sorted_result(query_result, sort, cx)
                        });
                        this.apply_reveal(cx);
                    }
                    Ok(Err(error)) => {
                        this.loaded_rows = 0;
                        this.columns.clear();
                        this.column_types.clear();
                        let message = format!("{error:#}");
                        this.error = Some(message.clone());
                        this.grid.update(cx, |grid, cx| grid.clear(cx));
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                    Err(_) => this.error = Some("loading the table was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Hold `action` back when the page has edits it would throw away.
    ///
    /// Returns true when it was held: the footer then asks what to do with the
    /// edits, and the answer either runs the action or drops it.
    fn hold(&mut self, action: Pending, cx: &mut Context<Self>) -> bool {
        // What is half-typed counts as an edit too, so it is folded in before
        // the question is asked.
        self.grid.update(cx, |grid, cx| grid.commit_editor(cx));
        if self.grid.read(cx).pending(cx) == 0 {
            return false;
        }

        self.pending = Some(action);
        cx.notify();
        true
    }

    /// Throw the staged edits away and do what was held back.
    fn discard_and_continue(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.pending.take() else {
            return;
        };

        self.grid.update(cx, |grid, cx| grid.discard(cx));
        // Nothing is staged now, so these run straight through their own
        // guard rather than asking again.
        match action {
            Pending::Reload => self.reload(cx),
            Pending::Page(page) => self.go(page, cx),
            Pending::Sort(sort) => self.apply_sort(sort, cx),
            Pending::Limit(limit) => self.set_limit(limit, cx),
            Pending::Filter => self.apply_filters(cx),
        }
    }

    /// Drop what was held back and leave the page as it is, edits and all.
    fn keep_edits(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.pending.take() else {
            return;
        };

        // A header click moves the sort marker before the view hears about it,
        // so a refused sort has to put the marker back on the sort the rows
        // are really in.
        if matches!(action, Pending::Sort(_)) {
            let sort = self.sort.clone();
            self.grid
                .update(cx, |grid, cx| grid.set_sort_marker(sort, cx));
        }
        cx.notify();
    }

    /// Reread the page, asking first when that would lose staged edits.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.hold(Pending::Reload, cx) {
            return;
        }
        self.reload(cx);
    }

    /// Whether the grid holds edits that were never written, so a close should
    /// ask before throwing them away.
    pub(crate) fn has_staged_edits(&self, cx: &App) -> bool {
        self.grid.read(cx).has_unsaved_edits(cx)
    }

    /// Select `column` in the grid once the page is there.
    ///
    /// A schema search opens the table and asks straight away; the page is
    /// still loading at that point, so the ask is remembered and applied when
    /// the rows arrive.
    pub(crate) fn reveal_column(&mut self, column: &str, cx: &mut Context<Self>) {
        self.reveal = Some(column.to_string());
        self.apply_reveal(cx);
    }

    /// Apply a remembered column once the grid has it, and forget it then.
    fn apply_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(column) = self.reveal.clone() else {
            return;
        };
        if self
            .grid
            .update(cx, |grid, cx| grid.reveal_column(&column, cx))
        {
            self.reveal = None;
        }
    }

    /// Order the rows by `sort` on the server, from the first page.
    fn apply_sort(&mut self, sort: Option<(String, ColumnSort)>, cx: &mut Context<Self>) {
        if self.hold(Pending::Sort(sort.clone()), cx) {
            return;
        }

        self.sort = sort;
        // Sorting reorders the whole table, so the old page number is
        // meaningless.
        self.page = 0;
        self.reload(cx);
    }

    /// Read `limit` rows a page, from the first page.
    fn set_limit(&mut self, limit: usize, cx: &mut Context<Self>) {
        if self.hold(Pending::Limit(limit), cx) {
            return;
        }

        self.limit = limit;
        self.page = 0;
        self.reload(cx);
    }

    /// Start a row the user fills in by hand.
    fn insert_row(&mut self, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        self.notice = None;
        self.grid.update(cx, |grid, cx| grid.add_draft(cx));
    }

    fn on_insert_row(&mut self, _: &InsertRow, _window: &mut Window, cx: &mut Context<Self>) {
        self.insert_row(cx);
    }

    /// Write every staged row.
    pub(crate) fn commit(&mut self, cx: &mut Context<Self>) {
        self.commit_rows(None, cx);
    }

    /// Write the staged rows, or only `rows` when given.
    fn commit_rows(&mut self, rows: Option<&[usize]>, cx: &mut Context<Self>) {
        if self.committing {
            return;
        }

        // Whatever is half-typed counts as an edit; folding it in first is
        // what makes leaving a cell and leaving the row the same thing.
        self.grid.update(cx, |grid, cx| grid.commit_editor(cx));

        // A row marked for deletion is not edited: whatever was typed into it
        // goes with it.
        let deletions: Vec<usize> = if rows.is_none() {
            self.grid.read(cx).deletions(cx)
        } else {
            Vec::new()
        };

        let staged: Vec<StagedRow> = self
            .grid
            .read(cx)
            .staged(cx)
            .into_iter()
            .filter(|staged| rows.is_none_or(|rows| rows.contains(&staged.row)))
            .filter(|staged| !deletions.contains(&staged.row))
            .collect();

        // A row being built by hand belongs to no row of the page, so it is
        // written only when the whole page is.
        let drafts: Vec<Vec<(usize, Cell)>> = if rows.is_none() {
            self.grid.read(cx).drafts(cx)
        } else {
            Vec::new()
        };

        if staged.is_empty() && drafts.is_empty() && deletions.is_empty() {
            return;
        }

        let mut statements = Vec::with_capacity(staged.len() + drafts.len() + deletions.len());
        for row in &staged {
            let Some((sql, params)) = self.update_statement(row, cx) else {
                self.error = Some("this row cannot be addressed, so it was not written".into());
                cx.notify();
                return;
            };
            statements.push((sql, params));
        }

        let mut inserted = 0;
        for draft in &drafts {
            // A row with nothing typed into it has nothing to say; it stays on
            // screen to be filled in.
            let Some((sql, params)) = self.insert_statement(draft) else {
                continue;
            };
            inserted += 1;
            statements.push((sql, params));
        }

        for row in &deletions {
            let Some((sql, params)) = self.delete_statement(*row, cx) else {
                self.error = Some("this row cannot be addressed, so it was not deleted".into());
                cx.notify();
                return;
            };
            statements.push((sql, params));
        }

        if statements.is_empty() {
            self.error = Some("the new row is empty, so there was nothing to write".into());
            cx.notify();
            return;
        }

        let rows: Vec<usize> = staged.iter().map(|staged| staged.row).collect();
        // Editing a key column changes what addresses the row, and Postgres
        // moves a row's `ctid` when it rewrites it, so either way the page in
        // hand is stale and has to be read again. So is a page that gained or
        // lost a row: only the server knows what it ended up holding.
        let stale = inserted > 0
            || !deletions.is_empty()
            || matches!(self.row_key, Some(RowKey::RowId("ctid")))
            || staged.iter().any(|staged| self.touches_key(staged));

        let write = Confirming {
            summary: applied_summary(rows.len(), inserted, deletions.len()),
            question: change_summary(rows.len(), inserted, deletions.len()),
            statements,
            rows,
            stale,
        };

        // On a connection that confirms writes, the statements go on screen
        // instead of to the server, and wait there for an answer.
        if self.connection.config.safety.confirms_writes() {
            self.error = None;
            self.notice = None;
            self.confirming = Some(write);
            cx.notify();
            return;
        }

        self.run_write(write, cx);
    }

    /// Run a built write and put what it did into the grid.
    fn run_write(&mut self, write: Confirming, cx: &mut Context<Self>) {
        let Confirming {
            statements,
            rows,
            stale,
            summary,
            question: _,
        } = write;

        self.committing = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            // There is no transaction on a connection yet, so a statement that
            // fails leaves the ones before it written; the error says which.
            for (position, (sql, params)) in statements.into_iter().enumerate() {
                let affected = connection.execute(&sql, params).await?;
                if affected != 1 {
                    anyhow::bail!(
                        "row {} matched {affected} rows instead of one; \
                         it may have changed since it was read",
                        position + 1
                    );
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.committing = false;
                match result {
                    Ok(Ok(())) => {
                        this.notice = Some(summary);
                        if stale {
                            this.reload(cx);
                        } else {
                            this.grid
                                .update(cx, |grid, cx| grid.apply_staged(&rows, cx));
                        }
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        this.error = Some(message.clone());
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                    Err(_) => this.error = Some("the write was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Export the whole table, in `format`.
    ///
    /// The filters and the sort are honoured; the page limit is not, since an
    /// export is every row the view is showing, not just the page in hand.
    pub(crate) fn export(&mut self, format: Format, cx: &mut Context<Self>) {
        // The statement is built now — that costs nothing — but the table is
        // only read once a file has been chosen, so a cancelled dialog does not
        // cost a full-table read.
        let (sql, params) = self.select_sql(false, cx);
        let table = self.object.name.clone();
        let connection = self.connection.clone();
        // A table addressed by its engine row id has that id asked for by name
        // (`select rowid, *`), so it has to come back out; a table with a
        // primary key selects only its own columns and needs nothing dropped.
        let by_row_id = matches!(self.row_key, Some(RowKey::RowId(_)));

        let query = async move {
            match runtime::spawn(async move { connection.run_query_with(&sql, params).await }).await
            {
                Ok(Ok(mut result)) => {
                    if by_row_id {
                        sql::take_row_id(&mut result);
                    }
                    Ok(result)
                }
                Ok(Err(error)) => Err(format!("{error:#}")),
                Err(_) => Err("exporting the table was cancelled".to_string()),
            }
        };

        self.export_result(format, table, query, cx);
    }

    /// Export the rows picked out in the grid, in `format`.
    ///
    /// Cells come through the grid, so a staged edit is exported as it stands.
    pub(crate) fn export_picked(&mut self, format: Format, cx: &mut Context<Self>) {
        let snapshot = self.grid.read(cx).snapshot(Scope::Picked, cx);
        if snapshot.rows.is_empty() {
            return;
        }

        let table = self.object.name.clone();
        let result = QueryResult {
            columns: snapshot.columns,
            column_types: snapshot.types,
            rows: snapshot.rows,
            ..QueryResult::default()
        };
        let query = async move { Ok(result) };
        self.export_result(format, table, query, cx);
    }

    /// Ask where to put an export, then run `query`, lay the rows out, and
    /// write them there.
    fn export_result(
        &mut self,
        format: Format,
        table: String,
        query: impl Future<Output = Result<QueryResult, String>> + 'static,
        cx: &mut Context<Self>,
    ) {
        let engine = self.connection.config.engine;
        let prompt = sql_file::prompt_for_save(None, &table, format.extension(), cx);

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            let result = match query.await {
                Ok(result) => result,
                Err(message) => {
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            let rendered = export::render(
                format,
                engine,
                &table,
                &result.columns,
                &result.column_types,
                &result.rows,
            );
            let rows = result.row_count();
            let export::Rendered { text, skipped } = rendered;

            let written = cx
                .background_spawn(sql_file::write(path.clone(), text))
                .await;
            this.update_in(cx, |this, window, cx| {
                match written {
                    Ok(()) => {
                        this.error = None;
                        this.notice = Some(export_notice(rows, &path, skipped));
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        this.error = Some(message.clone());
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put an export failure where the status line and the toasts can see it.
    fn fail_export(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.error = Some(message.clone());
        self.notice = None;
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
        cx.notify();
    }

    /// Mark the rows the grid has picked out for deletion.
    fn delete_rows(&mut self, cx: &mut Context<Self>) {
        if self.committing || !self.is_editable() {
            return;
        }
        self.notice = None;
        self.grid.update(cx, |grid, cx| grid.delete_selected(cx));
    }

    /// Take the deletion mark off the rows the grid has picked out.
    fn restore_rows(&mut self, cx: &mut Context<Self>) {
        if self.committing {
            return;
        }
        self.grid.update(cx, |grid, cx| grid.restore_selected(cx));
    }

    fn on_delete_rows(&mut self, _: &DeleteRows, _window: &mut Window, cx: &mut Context<Self>) {
        self.delete_rows(cx);
    }

    fn on_restore_rows(&mut self, _: &RestoreRows, _window: &mut Window, cx: &mut Context<Self>) {
        self.restore_rows(cx);
    }

    /// Run the write that is waiting to be confirmed.
    fn confirm_write(&mut self, cx: &mut Context<Self>) {
        let Some(write) = self.confirming.take() else {
            return;
        };
        self.run_write(write, cx);
    }

    /// Drop the write that is waiting; the edits stay staged to try again.
    fn cancel_write(&mut self, cx: &mut Context<Self>) {
        if self.confirming.take().is_none() {
            return;
        }
        self.notice = Some("Not written".into());
        cx.notify();
    }

    fn on_grid_edit(
        &mut self,
        _: &Entity<DataGrid>,
        event: &GridEdit,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // On an auto-apply connection, leaving a row is what writes it,
            // the way a form field applies when you tab out of it. A staged
            // connection keeps the edit until it is applied by hand.
            GridEdit::RowLeft { row } => {
                if self.connection.config.safety.auto_applies() {
                    self.commit_rows(Some(&[*row]), cx);
                } else {
                    cx.notify();
                }
            }
            GridEdit::Staged | GridEdit::RowFocused(_) => cx.notify(),
        }
    }

    /// The grid's menu asked for the picked rows to be exported.
    fn on_grid_export(
        &mut self,
        _: &Entity<DataGrid>,
        event: &ExportRequested,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_picked(event.format, cx);
    }

    /// A copy went to the clipboard; the footer says what it was.
    fn on_grid_copied(
        &mut self,
        _: &Entity<DataGrid>,
        event: &Copied,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notice = Some(event.message.clone());
        cx.notify();
    }

    /// A jump to a foreign key's referenced row was asked for from the row
    /// menu. Resolved here, since this is the one place that holds both the
    /// table's own foreign keys and its current column layout; where to open
    /// it is the session's call, so it goes out as an event.
    fn on_grid_navigate(
        &mut self,
        _: &Entity<DataGrid>,
        event: &GridNavigate,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(foreign_keys) = &self.foreign_keys else {
            return;
        };
        let Some(name) = self.columns.get(event.col) else {
            return;
        };
        let Some(fk) = foreign_keys
            .iter()
            .find(|fk| fk.columns.len() == 1 && fk.columns[0] == *name)
        else {
            return;
        };
        let Some(Some(value)) = self
            .grid
            .read(cx)
            .baseline_row(event.row, cx)
            .and_then(|row| row.get(event.col).cloned())
        else {
            return;
        };

        cx.emit(TableViewEvent::NavigateToForeignKey {
            object: DatabaseObject {
                schema: fk.referenced_schema.clone(),
                name: fk.referenced_table.clone(),
                kind: ObjectKind::Table,
            },
            filter: FilterSpec {
                column: fk.referenced_columns[0].clone(),
                operator: Operator::Equals,
                value,
            },
        });
    }

    /// Replace this table's filters with one already filled in, e.g. a
    /// foreign key jump from another table's row menu.
    pub(crate) fn apply_external_filter(
        &mut self,
        filter: FilterSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filters.update(cx, |filters, cx| {
            filters.set_filter(&filter.column, filter.operator, &filter.value, window, cx)
        });
    }

    fn on_apply_edits(&mut self, _: &ApplyEdits, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit(cx);
    }

    fn on_discard_edits(&mut self, _: &DiscardEdits, _window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| grid.discard(cx));
        self.notice = None;
        cx.notify();
    }

    fn on_edit_cell(&mut self, _: &EditCell, window: &mut Window, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.edit_selected(window, cx));
    }

    fn on_set_null(&mut self, _: &SetNull, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| grid.set_null(window, cx));
    }

    fn on_cancel_edit(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.grid
            .update(cx, |grid, cx| grid.cancel_editor(window, cx));
    }

    /// Re-run the page with the filters as they now stand.
    fn on_filters_changed(
        &mut self,
        _: &Entity<FilterBar>,
        _: &FiltersChanged,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.apply_filters(cx);
    }

    /// Read the table again through the filter bar, from the first page.
    fn apply_filters(&mut self, cx: &mut Context<Self>) {
        if self.hold(Pending::Filter, cx) {
            return;
        }

        // A filter changes which rows there are, so the page number it had is
        // about a different set of rows.
        self.page = 0;
        self.reload(cx);
    }

    fn on_sort_requested(
        &mut self,
        _: &Entity<DataGrid>,
        event: &SortRequested,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sort = match event.sort {
            ColumnSort::Default => None,
            sort => Some((event.column.clone(), sort)),
        };
        self.apply_sort(sort, cx);
    }

    fn on_limit_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Applied on Enter rather than per keystroke, so typing "1000" does not
        // fire queries for 1, 10, and 100 on the way.
        if !matches!(event, InputEvent::PressEnter { .. }) {
            return;
        }

        let Ok(limit) = input.read(cx).value().trim().parse::<usize>() else {
            return;
        };
        let limit = limit.clamp(1, settings::MAX_PAGE_SIZE);
        if limit == self.limit {
            return;
        }

        self.set_limit(limit, cx);
    }

    /// A click on the number input's own +/- buttons: unlike typing, one
    /// click is one committed change, so it applies straight away rather
    /// than waiting on `Enter`.
    fn on_limit_step(
        &mut self,
        input: &Entity<InputState>,
        event: &NumberInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NumberInputEvent::Step(action) = event;
        let limit = match action {
            StepAction::Increment => self.limit.saturating_add(1),
            StepAction::Decrement => self.limit.saturating_sub(1),
        }
        .clamp(1, settings::MAX_PAGE_SIZE);
        if limit == self.limit {
            return;
        }

        input.update(cx, |input, cx| {
            input.set_value(limit.to_string(), window, cx);
        });
        self.set_limit(limit, cx);
    }

    fn has_previous(&self) -> bool {
        self.page > 0
    }

    /// A full page suggests there is more; a short one means this is the end.
    fn has_next(&self) -> bool {
        self.loaded_rows >= self.limit
    }

    fn go(&mut self, page: usize, cx: &mut Context<Self>) {
        if page == self.page {
            return;
        }
        if self.hold(Pending::Page(page), cx) {
            return;
        }

        self.page = page;
        self.reload(cx);
    }

    /// The changes waiting for an answer, above the footer.
    ///
    /// The statements themselves are not shown: the rows on screen already
    /// say what will happen to them, in the colours they are drawn in.
    fn render_confirm(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let question = self
            .confirming
            .as_ref()
            .map(|write| write.question.clone())
            .unwrap_or_default();

        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_2()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(format!("Apply {question}?")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cancel-write")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .tooltip("Leave the changes as they are")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_write(cx))),
                    )
                    .child(
                        Button::new("confirm-write")
                            .primary()
                            .xsmall()
                            .label("Apply")
                            .tooltip("Write the changes to the server")
                            .on_click(cx.listener(|this, _, _window, cx| this.confirm_write(cx))),
                    ),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_row = self.page * self.limit;
        let range = if self.loaded_rows == 0 {
            "No rows".to_string()
        } else {
            format!("Rows {}–{}", first_row + 1, first_row + self.loaded_rows)
        };

        let (edited, inserted, deleted) = self.grid.read(cx).pending_counts(cx);
        let staged = edited + inserted + deleted;
        let changed = match staged {
            0 => None,
            _ => Some(change_summary(edited, inserted, deleted)),
        };

        // An error says so in words: the colour it is drawn in is the only
        // other thing telling it apart from the row count beside it.
        let message = match (&self.error, self.loading, self.committing) {
            (Some(error), _, _) => (format!("Error: {error}"), cx.theme().danger),
            (None, _, true) => ("Writing…".to_string(), cx.theme().muted_foreground),
            (None, true, _) => ("Loading…".to_string(), cx.theme().muted_foreground),
            // The question comes first: it is the one thing here waiting on
            // an answer.
            (None, false, _) => match (&self.pending, &changed, &self.notice) {
                (Some(action), _, _) => (
                    format!(
                        "{} discards {}",
                        action.label(),
                        change_summary(edited, inserted, deleted)
                    ),
                    cx.theme().danger,
                ),
                (None, Some(changed), _) => (changed.clone(), cx.theme().warning),
                (None, None, Some(notice)) => (notice.clone(), cx.theme().muted_foreground),
                (None, None, None) => (range, cx.theme().muted_foreground),
            },
        };

        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_1()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("previous-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .accessibility_label("Previous page")
                            .tooltip("Previous page")
                            .disabled(!self.has_previous() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.go(this.page.saturating_sub(1), cx)
                            })),
                    )
                    .child(
                        Button::new("next-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronRight)
                            .accessibility_label("Next page")
                            .tooltip("Next page")
                            .disabled(!self.has_next() || self.loading)
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.go(this.page + 1, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(message.1)
                            .child(message.0.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when_some(self.read_only_reason(), |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Read-only: {reason}")),
                        )
                    })
                    .when_some(self.pending.clone(), |this, _| {
                        this.child(
                            Button::new("keep-edits")
                                .ghost()
                                .xsmall()
                                .label("Keep editing")
                                .tooltip("Leave the page as it is")
                                .on_click(cx.listener(|this, _, _window, cx| this.keep_edits(cx))),
                        )
                        .child(
                            Button::new("discard-and-continue")
                                .danger()
                                .xsmall()
                                .label("Discard")
                                .tooltip("Throw the edits away and carry on")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.discard_and_continue(cx)
                                })),
                        )
                    })
                    .when(
                        self.is_editable() && self.pending.is_none() && self.confirming.is_none(),
                        |this| {
                            this.child(
                                Button::new("insert-row")
                                    .ghost()
                                    .xsmall()
                                    .label("New row")
                                    .tooltip_with_action(
                                        "Add a row to fill in",
                                        &InsertRow,
                                        Some("TableView > DataTable"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(
                                        cx.listener(|this, _, _window, cx| this.insert_row(cx)),
                                    ),
                            )
                        },
                    )
                    // Hidden while a write is waiting to be confirmed: the
                    // confirm banner above already asks about the same
                    // changes, so showing both looks like clicking Apply did
                    // nothing.
                    .when(
                        staged > 0 && self.pending.is_none() && self.confirming.is_none(),
                        |this| {
                            this.child(
                                Button::new("discard-edits")
                                    .ghost()
                                    .xsmall()
                                    .label("Discard")
                                    .tooltip_with_action(
                                        "Throw away the staged edits",
                                        &DiscardEdits,
                                        Some("TableView"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_discard_edits(&DiscardEdits, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("apply-edits")
                                    .primary()
                                    .xsmall()
                                    .label("Apply")
                                    .tooltip_with_action(
                                        "Write the staged edits",
                                        &ApplyEdits,
                                        Some("TableView"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(cx.listener(|this, _, _window, cx| this.commit(cx))),
                            )
                        },
                    )
                    .child({
                        let view = cx.entity().downgrade();
                        Button::new("export-table")
                            .ghost()
                            .xsmall()
                            .label("Export")
                            .dropdown_caret(true)
                            .tooltip("Write the whole table to a file")
                            .disabled(self.loading || self.committing)
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                for format in Format::FILE {
                                    let view = view.clone();
                                    menu =
                                        menu.item(
                                            PopupMenuItem::new(format!(
                                                "Export as {}…",
                                                format.label()
                                            ))
                                            .on_click(move |_, _window, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |view, cx| {
                                                        view.export(format, cx)
                                                    });
                                                }
                                            }),
                                        );
                                }
                                menu
                            })
                    })
                    .child(
                        Button::new("toggle-row-panel")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelRightOpen)
                            .accessibility_label("Toggle row detail panel")
                            .tooltip_with_action(
                                "Show the focused row as fields",
                                &ToggleRowPanel,
                                Some("TableView"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_toggle_row_panel(&ToggleRowPanel, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Limit"),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .child(NumberInput::new(&self.limit_input).xsmall()),
                    ),
            )
    }
}

impl Focusable for TableView {
    /// The rows are where the keyboard belongs: a tab that has just been
    /// opened, or that a dialog has just closed over, answers the arrows and
    /// the row commands without being clicked into first.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for TableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("TableView")
            .on_action(cx.listener(Self::on_apply_edits))
            .on_action(cx.listener(Self::on_discard_edits))
            .on_action(cx.listener(Self::on_edit_cell))
            .on_action(cx.listener(Self::on_set_null))
            .on_action(cx.listener(Self::on_cancel_edit))
            .on_action(cx.listener(Self::on_insert_row))
            .on_action(cx.listener(Self::on_delete_rows))
            .on_action(cx.listener(Self::on_restore_rows))
            .on_action(cx.listener(Self::on_toggle_row_panel))
            .child(self.filters.clone())
            .child(if self.row_panel_visible {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        h_resizable("table-view-columns")
                            .with_state(&self.columns_pane)
                            .child(resizable_panel().child(self.grid.clone()))
                            .child(
                                resizable_panel()
                                    .size(px(280.))
                                    .size_range(px(220.)..px(480.))
                                    .child(self.row_panel.clone()),
                            ),
                    )
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.grid.clone())
                    .into_any_element()
            })
            .when(self.confirming.is_some(), |this| {
                this.child(self.render_confirm(cx))
            })
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
impl TableView {
    pub(crate) fn page_for_test(&self) -> usize {
        self.page
    }

    pub(crate) fn limit_for_test(&self) -> usize {
        self.limit
    }

    pub(crate) fn go_for_test(&mut self, page: usize, cx: &mut Context<Self>) {
        self.go(page, cx);
    }

    pub(crate) fn sort_for_test(&mut self, column: &str, sort: ColumnSort, cx: &mut Context<Self>) {
        let sort = match sort {
            ColumnSort::Default => None,
            sort => Some((column.to_string(), sort)),
        };
        self.apply_sort(sort, cx);
    }

    pub(crate) fn pending_for_test(&self) -> bool {
        self.pending.is_some()
    }

    /// The statements waiting to be confirmed, as the panel shows them.
    pub(crate) fn confirming_for_test(&self) -> Option<String> {
        self.confirming.as_ref().map(|write| write.question.clone())
    }

    pub(crate) fn row_key_for_test(&self) -> Option<RowKey> {
        self.row_key.clone()
    }

    pub(crate) fn is_editable_for_test(&self) -> bool {
        self.is_editable()
    }

    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn notice_for_test(&self) -> Option<String> {
        self.notice.clone()
    }

    pub(crate) fn filters_for_test(&self) -> Entity<FilterBar> {
        self.filters.clone()
    }

    pub(crate) fn row_panel_for_test(&self) -> Entity<RowPanel> {
        self.row_panel.clone()
    }

    pub(crate) fn toggle_row_panel_for_test(&mut self, cx: &mut Context<Self>) {
        self.row_panel_visible = !self.row_panel_visible;
        cx.notify();
    }

    pub(crate) fn row_panel_visible_for_test(&self) -> bool {
        self.row_panel_visible
    }

    pub(crate) fn grid_for_test(&self) -> Entity<DataGrid> {
        self.grid.clone()
    }

    pub(crate) fn set_loaded_rows_for_test(&mut self, rows: usize) {
        self.loaded_rows = rows;
    }

    pub(crate) fn loaded_rows_for_test(&self) -> usize {
        self.loaded_rows
    }

    pub(crate) fn can_page_for_test(&self) -> (bool, bool) {
        (self.has_previous(), self.has_next())
    }
}
