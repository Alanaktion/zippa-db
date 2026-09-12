//! A table opened from the sidebar: the grid, paged, with no editor.
//!
//! TODO.md section 2 ("Configurable row limit & offset pagination"), the
//! sorting half of "Multi-column sorting", and the buffer model for inline
//! edits: the grid stages what is typed into it and this view turns a row's
//! staged cells into an `UPDATE`, on leaving the row or on demand.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::table::ColumnSort;
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Window, actions, div, px};

use crate::db::query::{Cell, QueryResult};
use crate::db::{
    Connection, DatabaseObject, ObjectKind, RowKey, SafetyMode, quote_identifier, runtime,
    typed_placeholder,
};
use crate::settings::{self, Settings};
use crate::ui::data_grid::{DataGrid, GridEdit, SortRequested, Sorting, StagedRow};

actions!(
    zippa_db,
    [ApplyEdits, DiscardEdits, EditCell, SetNull, CancelEdit]
);

/// Something that would replace the rows in the grid, held back because the
/// rows in hand have edits that have not been written.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    Reload,
    Page(usize),
    Sort(Option<(String, ColumnSort)>),
    Limit(usize),
}

impl Pending {
    /// What this would do to the page, for the question in the footer.
    fn label(&self) -> &'static str {
        match self {
            Pending::Reload => "Refreshing",
            Pending::Page(_) => "Turning the page",
            Pending::Sort(_) => "Sorting",
            Pending::Limit(_) => "Changing the row limit",
        }
    }
}

pub struct TableView {
    connection: Arc<Connection>,
    object: DatabaseObject,
    grid: Entity<DataGrid>,
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
}

impl TableView {
    pub fn new(
        connection: Arc<Connection>,
        object: DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| DataGrid::with_sorting(Sorting::Delegated, window, cx));
        cx.subscribe_in(&grid, window, Self::on_sort_requested)
            .detach();
        cx.subscribe_in(&grid, window, Self::on_grid_edit).detach();

        // A table takes the page size the settings had when it was opened;
        // changing the setting later leaves open tables where they are.
        let limit = Settings::global(cx)
            .page_size
            .clamp(1, settings::MAX_PAGE_SIZE);

        let limit_input = cx.new(|cx| InputState::new(window, cx).default_value(limit.to_string()));
        cx.subscribe_in(&limit_input, window, Self::on_limit_event)
            .detach();

        let mut view = Self {
            connection,
            object,
            grid,
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
            key_values: Vec::new(),
            key_type: None,
            committing: false,
            notice: None,
            pending: None,
        };
        view.load_row_key(cx);
        view
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
        self.load_row_key(cx);
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

    /// Whether rows of this table can be written back.
    fn is_editable(&self) -> bool {
        self.object.kind == ObjectKind::Table
            && matches!(&self.row_key, Some(key) if key.is_available())
    }

    /// Why the table is read-only, when it is and the user should know.
    fn read_only_reason(&self) -> Option<&'static str> {
        match &self.row_key {
            Some(RowKey::Unavailable(reason)) => Some(reason),
            _ => None,
        }
    }

    /// The table this view reads and writes, qualified and quoted.
    fn target(&self) -> String {
        let engine = self.connection.config.engine;
        match &self.object.schema {
            Some(schema) => format!(
                "{}.{}",
                quote_identifier(schema, engine),
                quote_identifier(&self.object.name, engine)
            ),
            None => quote_identifier(&self.object.name, engine),
        }
    }

    /// The statement this view runs for its current page and sort.
    pub fn query(&self) -> String {
        let engine = self.connection.config.engine;
        let target = self.target();

        // A table with no primary key is addressed by the engine's own row
        // identifier, which `select *` leaves out, so it is asked for by name
        // and taken back out of the result before the grid sees it.
        let mut sql = match &self.row_key {
            Some(RowKey::RowId(id)) => format!("select {id}, * from {target}"),
            _ => format!("select * from {target}"),
        };
        if let Some((column, sort)) = &self.sort {
            let direction = match sort {
                ColumnSort::Descending => "desc",
                _ => "asc",
            };
            sql.push_str(&format!(
                " order by {} {direction}",
                quote_identifier(column, engine)
            ));
        }
        sql.push_str(&format!(
            " limit {} offset {}",
            self.limit,
            self.page * self.limit
        ));
        sql
    }

    /// Rebuild the paging SQL and re-run it. Also the table half of the
    /// session's refresh action.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let sql = self.query();
        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.run_query(&sql).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(mut query_result)) => {
                        this.take_key_column(&mut query_result);
                        this.loaded_rows = query_result.row_count();
                        this.columns = query_result.columns.clone();
                        this.column_types = query_result.column_types.clone();
                        // The rows come back already ordered, so the grid is
                        // told what order they are in: it rebuilds its headers
                        // from scratch and would otherwise show the column as
                        // unsorted and restart its cycle on the next click.
                        let sort = this.sort.clone();
                        this.grid.update(cx, |grid, cx| {
                            grid.set_sorted_result(query_result, sort, cx)
                        });
                    }
                    Ok(Err(error)) => {
                        this.loaded_rows = 0;
                        this.columns.clear();
                        this.column_types.clear();
                        this.error = Some(format!("{error:#}"));
                        this.grid.update(cx, |grid, cx| grid.clear(cx));
                    }
                    Err(_) => this.error = Some("loading the table was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Take the row identifier column back out of a freshly loaded page.
    ///
    /// It was asked for so rows could be addressed, not so it could be shown,
    /// so its values are kept here and the grid is handed the table's own
    /// columns.
    fn take_key_column(&mut self, result: &mut QueryResult) {
        self.key_values.clear();
        self.key_type = None;

        if !matches!(self.row_key, Some(RowKey::RowId(_))) || result.columns.is_empty() {
            return;
        }

        result.columns.remove(0);
        if !result.column_types.is_empty() {
            self.key_type = Some(result.column_types.remove(0));
        }
        for row in &mut result.rows {
            if row.is_empty() {
                self.key_values.push(None);
                continue;
            }
            self.key_values.push(row.remove(0));
        }
    }

    /// How a row is addressed in a write: the left-hand side, the type to cast
    /// the parameter to, and the value itself.
    fn key_for(&self, row: usize, cx: &gpui_kit::App) -> Option<Vec<(String, String, Cell)>> {
        let engine = self.connection.config.engine;
        match self.row_key.as_ref()? {
            RowKey::Columns(columns) => {
                let loaded = self.grid.read(cx).baseline_row(row, cx)?;
                columns
                    .iter()
                    .map(|column| {
                        let index = self.columns.iter().position(|name| name == column)?;
                        let value = loaded.get(index)?.clone();
                        // A NULL key would never match with `=`, and a key
                        // column should not be NULL in the first place.
                        value.as_ref()?;
                        Some((
                            quote_identifier(column, engine),
                            self.column_types.get(index).cloned().unwrap_or_default(),
                            value,
                        ))
                    })
                    .collect()
            }
            RowKey::RowId(id) => {
                let value = self.key_values.get(row)?.clone();
                value.as_ref()?;
                Some(vec![(
                    id.to_string(),
                    self.key_type.clone().unwrap_or_default(),
                    value,
                )])
            }
            RowKey::Unavailable(_) => None,
        }
    }

    /// The `UPDATE` for one row's staged cells, and the values to bind to it.
    fn update_statement(
        &self,
        staged: &StagedRow,
        cx: &gpui_kit::App,
    ) -> Option<(String, Vec<Cell>)> {
        if staged.cells.is_empty() {
            return None;
        }
        let engine = self.connection.config.engine;
        let key = self.key_for(staged.row, cx)?;

        let mut params: Vec<Cell> = Vec::with_capacity(staged.cells.len() + key.len());
        let mut index = 0;

        let assignments: Vec<String> = staged
            .cells
            .iter()
            .map(|(column, value)| {
                index += 1;
                params.push(value.clone());
                let name = self.columns.get(*column).cloned().unwrap_or_default();
                let type_name = self.column_types.get(*column).cloned().unwrap_or_default();
                format!(
                    "{} = {}",
                    quote_identifier(&name, engine),
                    typed_placeholder(engine, index, &type_name)
                )
            })
            .collect();

        let conditions: Vec<String> = key
            .into_iter()
            .map(|(left, type_name, value)| {
                index += 1;
                params.push(value);
                format!("{left} = {}", typed_placeholder(engine, index, &type_name))
            })
            .collect();

        Some((
            format!(
                "update {} set {} where {}",
                self.target(),
                assignments.join(", "),
                conditions.join(" and ")
            ),
            params,
        ))
    }

    /// Hold `action` back when the page has edits it would throw away.
    ///
    /// Returns true when it was held: the footer then asks what to do with the
    /// edits, and the answer either runs the action or drops it.
    fn hold(&mut self, action: Pending, cx: &mut Context<Self>) -> bool {
        // What is half-typed counts as an edit too, so it is folded in before
        // the question is asked.
        self.grid.update(cx, |grid, cx| grid.commit_editor(cx));
        if self.grid.read(cx).staged(cx).is_empty() {
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

        let staged: Vec<StagedRow> = self
            .grid
            .read(cx)
            .staged(cx)
            .into_iter()
            .filter(|staged| rows.is_none_or(|rows| rows.contains(&staged.row)))
            .collect();
        if staged.is_empty() {
            return;
        }

        let mut statements = Vec::with_capacity(staged.len());
        for row in &staged {
            let Some((sql, params)) = self.update_statement(row, cx) else {
                self.error = Some("this row cannot be addressed, so it was not written".into());
                cx.notify();
                return;
            };
            statements.push((sql, params));
        }

        let written: Vec<usize> = staged.iter().map(|staged| staged.row).collect();
        // Editing a key column changes what addresses the row, and Postgres
        // moves a row's `ctid` when it rewrites it, so either way the page in
        // hand is stale and has to be read again.
        let stale = matches!(self.row_key, Some(RowKey::RowId("ctid")))
            || staged.iter().any(|staged| self.touches_key(staged));

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
            this.update(cx, |this, cx| {
                this.committing = false;
                match result {
                    Ok(Ok(())) => {
                        let rows = written.len();
                        let unit = if rows == 1 { "row" } else { "rows" };
                        this.notice = Some(format!("Wrote {rows} {unit}"));
                        if stale {
                            this.reload(cx);
                        } else {
                            this.grid
                                .update(cx, |grid, cx| grid.apply_staged(&written, cx));
                        }
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => this.error = Some("the write was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether a staged row changes a column the write addresses it by.
    fn touches_key(&self, staged: &StagedRow) -> bool {
        let Some(RowKey::Columns(key)) = &self.row_key else {
            return false;
        };
        staged.cells.iter().any(|(column, _)| {
            self.columns
                .get(*column)
                .is_some_and(|name| key.contains(name))
        })
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
                if self.connection.config.safety == SafetyMode::AutoApply {
                    self.commit_rows(Some(&[*row]), cx);
                } else {
                    cx.notify();
                }
            }
            GridEdit::Staged => cx.notify(),
        }
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

    fn on_set_null(&mut self, _: &SetNull, _window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| grid.set_null(cx));
    }

    fn on_cancel_edit(&mut self, _: &CancelEdit, _window: &mut Window, cx: &mut Context<Self>) {
        self.grid.update(cx, |grid, cx| grid.cancel_editor(cx));
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

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_row = self.page * self.limit;
        let range = if self.loaded_rows == 0 {
            "No rows".to_string()
        } else {
            format!("Rows {}–{}", first_row + 1, first_row + self.loaded_rows)
        };

        let staged = self.grid.read(cx).staged(cx).len();
        let unit = if staged == 1 { "row" } else { "rows" };
        let changed = match staged {
            0 => None,
            rows => Some(format!("{rows} {unit} changed")),
        };

        let message = match (&self.error, self.loading, self.committing) {
            (Some(error), _, _) => (error.clone(), cx.theme().danger),
            (None, _, true) => ("Writing…".to_string(), cx.theme().muted_foreground),
            (None, true, _) => ("Loading…".to_string(), cx.theme().muted_foreground),
            // The question comes first: it is the one thing here waiting on
            // an answer.
            (None, false, _) => match (&self.pending, &changed, &self.notice) {
                (Some(action), _, _) => (
                    format!("{} discards {staged} changed {unit}", action.label()),
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
                    .when(staged > 0 && self.pending.is_none(), |this| {
                        this.child(
                            Button::new("discard-edits")
                                .ghost()
                                .xsmall()
                                .label("Discard")
                                .tooltip("Throw away the staged edits")
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
                                .tooltip("Write the staged edits")
                                .disabled(self.committing)
                                .on_click(cx.listener(|this, _, _window, cx| this.commit(cx))),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Limit"),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .child(Input::new(&self.limit_input).id("limit").xsmall()),
                    ),
            )
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
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
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

    pub(crate) fn row_key_for_test(&self) -> Option<RowKey> {
        self.row_key.clone()
    }

    pub(crate) fn is_editable_for_test(&self) -> bool {
        self.is_editable()
    }

    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
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
