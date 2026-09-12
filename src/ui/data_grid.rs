//! Result grid.
//!
//! TODO.md section 2. The table virtualizes rows and columns, and cells can be
//! typed into when the owner allows it: edits are staged in an overlay over the
//! result and handed back for the owner to write. Foreign key jumps and
//! specialized cell renderers come later.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, Entity, EventEmitter, Pixels, SharedString, Stateful, Window, div, px,
};

use crate::db::query::{self, Cell, QueryResult};
use crate::settings::{self, Settings};

/// Rows read when sizing a column. Values further down are rare enough that
/// paying for them on every result would cost more than the odd clipped cell.
const SAMPLE_ROWS: usize = 100;

/// Rough advance width of one character in the grid's font, which is monospaced
/// and rendered at `text_xs`. Measuring properly would mean shaping every
/// sampled value; this is within a few pixels and costs nothing.
const CHARACTER_WIDTH: f32 = 7.2;

/// Cell padding and borders that sit either side of the text.
const CELL_PADDING: f32 = 18.;

const MIN_COLUMN_WIDTH: f32 = 56.;
const MAX_COLUMN_WIDTH: f32 = 420.;

/// Width of the placeholder shown for SQL `NULL`.
const NULL_WIDTH: usize = 4;

/// Size every column from its header and the first [`SAMPLE_ROWS`] values.
fn measure_columns(result: &QueryResult) -> Vec<Pixels> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut characters = name.chars().count();

            for row in result.rows.iter().take(SAMPLE_ROWS) {
                let width = match row.get(index) {
                    Some(Some(value)) => value.chars().count(),
                    Some(None) => NULL_WIDTH,
                    None => 0,
                };
                characters = characters.max(width);
            }

            let width = characters as f32 * CHARACTER_WIDTH + CELL_PADDING;
            px(width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH))
        })
        .collect()
}

/// How a click on a column header is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sorting {
    /// Reorder the rows already in hand. Used for ad-hoc query results, where
    /// re-running the statement is not ours to do.
    InPlace,
    /// Report the sort to the owner so it can ask the server for ordered rows.
    /// Used by the table view, which pages through a table.
    Delegated,
}

/// Hands a delegated sort back to the grid's owner.
type SortReporter = Rc<dyn Fn(String, ColumnSort, &mut App)>;

/// Emitted when the user clicks a column header on a [`Sorting::Delegated`]
/// grid.
pub struct SortRequested {
    pub column: String,
    pub sort: ColumnSort,
}

/// What the grid reports about staged edits. The owner decides what a write
/// means; the grid only knows a cell was typed into and when the user moved on.
pub enum GridEdit {
    /// An edit was staged, unstaged, or thrown away.
    Staged,
    /// The selection left `row`, which has edits waiting on it.
    RowLeft { row: usize },
}

/// One row's staged cells, by column index, for the owner to turn into SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRow {
    /// Index into the result's own rows, not the display order.
    pub row: usize,
    pub cells: Vec<(usize, Cell)>,
}

struct ResultDelegate {
    result: QueryResult,
    /// Row indices in display order. Sorting reorders this rather than the
    /// rows themselves, so the order the rows arrived in is never lost.
    order: Vec<usize>,
    widths: Vec<Pixels>,
    /// Row under the selected cell, so the whole row can be highlighted while
    /// the selection itself stays on one column.
    selected_row: Option<usize>,
    font: SharedString,
    sorting: Sorting,
    /// Column the rows are sorted by, for the header arrow. Held by name, so
    /// it survives a result whose columns moved.
    sorted_by: Option<(String, ColumnSort)>,
    report_sort: SortReporter,
    /// Values typed into cells but not written yet, keyed by the row's index
    /// into the result and the column. Absent means unchanged.
    edits: HashMap<(usize, usize), Cell>,
    /// Whether the owner can write this result back at all.
    editable: bool,
    /// Cell the text editor is open on, in display coordinates.
    editing: Option<(usize, usize)>,
    /// The editor itself, shared with the grid so it can be focused and read.
    editor: Entity<InputState>,
}

/// Stands in for a cell a short row does not have.
static MISSING: Cell = None;

/// The cell at `row_ix`/`col_ix`, or `NULL` where the row is short.
fn cell_at(rows: &[Vec<Cell>], row_ix: usize, col_ix: usize) -> &Cell {
    rows.get(row_ix)
        .and_then(|row| row.get(col_ix))
        .unwrap_or(&MISSING)
}

/// Compare two cells: numbers numerically, everything else as text, NULLs last.
fn compare(left: &Cell, right: &Cell) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match (left.parse::<f64>(), right.parse::<f64>()) {
            (Ok(left), Ok(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
            _ => left.cmp(right),
        },
    }
}

impl TableDelegate for ResultDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.result.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.order.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let name = self.result.columns.get(col_ix).cloned().unwrap_or_default();
        let width = self
            .widths
            .get(col_ix)
            .copied()
            .unwrap_or(px(MIN_COLUMN_WIDTH));

        let sort = match &self.sorted_by {
            Some((sorted, sort)) if *sorted == name => *sort,
            _ => ColumnSort::Default,
        };

        Column::new(name.clone(), name)
            .width(width)
            .resizable(true)
            .sortable()
            .sort(sort)
    }

    /// Answer a header click.
    ///
    /// The table cycles a column through descending, ascending, and back to
    /// unsorted, and hands the state it settled on here.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(column) = self.result.columns.get(col_ix).cloned() else {
            return;
        };

        self.sorted_by = match sort {
            ColumnSort::Default => None,
            sort => Some((column.clone(), sort)),
        };

        match self.sorting {
            Sorting::InPlace => {
                self.reorder(col_ix, sort);
                cx.notify();
            }
            Sorting::Delegated => {
                let report = self.report_sort.clone();
                cx.defer(move |cx| report(column, sort, cx));
            }
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div()
            .id(("row", row_ix))
            .when(self.selected_row == Some(row_ix), |this| {
                this.bg(cx.theme().tokens.table_active)
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if self.editing == Some((row_ix, col_ix)) {
            // The table paints its selected-cell tint over this, so the editor
            // carries its own background to stay readable underneath it.
            return div()
                .size_full()
                .bg(cx.theme().background)
                .child(
                    Input::new(&self.editor)
                        .id(("cell-editor", col_ix))
                        .xsmall()
                        .appearance(false)
                        .bordered(false),
                )
                .into_any_element();
        }

        let cell = div().font_family(self.font.clone()).text_xs();
        let staged = self.is_staged(row_ix, col_ix);
        let cell = if staged {
            cell.bg(cx.theme().warning.opacity(0.2))
        } else {
            cell
        };

        match self.cell(row_ix, col_ix) {
            Some(value) => cell.child(value.to_string()).into_any_element(),
            None => cell
                .text_color(cx.theme().muted_foreground)
                .child("NULL")
                .into_any_element(),
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.cell(row_ix, col_ix).clone().unwrap_or_default()
    }
}

/// Stands in for a cell whose row the result does not have.
static ABSENT: Cell = None;

impl ResultDelegate {
    /// Index into the result's rows of the row shown at `row_ix`.
    fn source(&self, row_ix: usize) -> Option<usize> {
        self.order.get(row_ix).copied()
    }

    /// The value as it came from the server.
    fn baseline(&self, row_ix: usize, col_ix: usize) -> &Cell {
        let Some(row_ix) = self.source(row_ix) else {
            return &ABSENT;
        };
        cell_at(&self.result.rows, row_ix, col_ix)
    }

    /// The value as it stands, staged edit included.
    fn cell(&self, row_ix: usize, col_ix: usize) -> &Cell {
        match self
            .source(row_ix)
            .and_then(|row_ix| self.edits.get(&(row_ix, col_ix)))
        {
            Some(staged) => staged,
            None => self.baseline(row_ix, col_ix),
        }
    }

    fn is_staged(&self, row_ix: usize, col_ix: usize) -> bool {
        self.source(row_ix)
            .is_some_and(|row_ix| self.edits.contains_key(&(row_ix, col_ix)))
    }

    /// Whether this cell can be typed into.
    ///
    /// A binary column and a value the driver could only describe (`<3 bytes>`,
    /// `<XML>`) are shown but not held, so writing one back would lose it.
    fn is_editable(&self, row_ix: usize, col_ix: usize) -> bool {
        if !self.editable || self.source(row_ix).is_none() {
            return false;
        }
        let binary = self
            .result
            .column_types
            .get(col_ix)
            .is_some_and(|name| query::is_binary_type(name));
        !binary && !query::is_placeholder(self.baseline(row_ix, col_ix))
    }

    /// Stage `value` on a cell, or drop the edit when it matches the row as
    /// loaded, so typing a value back the way it was leaves nothing to write.
    fn stage(&mut self, row_ix: usize, col_ix: usize, value: Cell) {
        let Some(source) = self.source(row_ix) else {
            return;
        };
        if self.baseline(row_ix, col_ix) == &value {
            self.edits.remove(&(source, col_ix));
            return;
        }
        self.edits.insert((source, col_ix), value);
    }

    /// Put the display order where `sort` asks for; `Default` restores the
    /// order the server sent.
    fn reorder(&mut self, col_ix: usize, sort: ColumnSort) {
        let mut order: Vec<usize> = (0..self.result.rows.len()).collect();

        if sort != ColumnSort::Default {
            let rows = &self.result.rows;
            order.sort_by(|&left, &right| {
                let ordering = compare(cell_at(rows, left, col_ix), cell_at(rows, right, col_ix));
                match sort {
                    ColumnSort::Descending => ordering.reverse(),
                    _ => ordering,
                }
            });
        }

        self.order = order;
    }
}

pub struct DataGrid {
    table: Entity<TableState<ResultDelegate>>,
    has_result: bool,
    /// The cell editor, shared with the delegate that renders it.
    editor: Entity<InputState>,
}

impl EventEmitter<SortRequested> for DataGrid {}
impl EventEmitter<GridEdit> for DataGrid {}

impl DataGrid {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_sorting(Sorting::InPlace, window, cx)
    }

    pub fn with_sorting(sorting: Sorting, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let font = settings::grid_font(cx);

        let grid = cx.entity().downgrade();
        let report_sort: SortReporter = Rc::new(move |column, sort, cx| {
            if let Some(grid) = grid.upgrade() {
                grid.update(cx, |_, cx| cx.emit(SortRequested { column, sort }));
            }
        });

        let editor = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&editor, window, Self::on_editor_event)
            .detach();

        let table = cx.new(|cx| {
            TableState::new(
                ResultDelegate {
                    result: QueryResult::default(),
                    order: Vec::new(),
                    widths: Vec::new(),
                    selected_row: None,
                    font,
                    sorting,
                    sorted_by: None,
                    report_sort,
                    edits: HashMap::new(),
                    editable: false,
                    editing: None,
                    editor: editor.clone(),
                },
                window,
                cx,
            )
            .cell_selectable(true)
            .row_selectable(true)
        });

        // The grid's font is a setting, so it can change under a grid that is
        // already on screen.
        cx.observe_global::<Settings>(|this, cx| {
            let font = settings::grid_font(cx);
            this.table.update(cx, |table, cx| {
                if table.delegate().font != font {
                    table.delegate_mut().font = font;
                    cx.notify();
                }
            });
        })
        .detach();

        cx.subscribe_in(&table, window, Self::on_table_event)
            .detach();

        Self {
            table,
            has_result: false,
            editor,
        }
    }

    /// Follow the selection: highlight the row it sits on, close any editor it
    /// moved away from, and report a row it left with edits waiting on it.
    fn on_table_event(
        &mut self,
        table: &Entity<TableState<ResultDelegate>>,
        event: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let TableEvent::DoubleClickedCell(row_ix, col_ix) = event {
            self.begin_edit(*row_ix, *col_ix, window, cx);
            return;
        }

        let row = match event {
            TableEvent::SelectCell(row_ix, _) => Some(*row_ix),
            TableEvent::SelectRow(row_ix) => Some(*row_ix),
            TableEvent::ClearSelection => None,
            _ => return,
        };

        // Fold the open editor in first: the click that moves the selection
        // also blurs the input, and the order the two arrive in is not ours.
        self.commit_editor(cx);

        let previous = table.read(cx).delegate().selected_row;
        if previous == row {
            return;
        }

        table.update(cx, |table, cx| {
            table.delegate_mut().selected_row = row;
            cx.notify();
        });

        // Leaving a row is what writes it, so the owner hears about it here.
        if let Some(left) = previous
            && let Some(source) = table.read(cx).delegate().source(left)
            && self.row_has_edits(source, cx)
        {
            cx.emit(GridEdit::RowLeft { row: source });
        }
    }

    fn on_editor_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Enter finishes the cell; losing focus does the same, so clicking
        // away never drops what was typed.
        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
            self.commit_editor(cx);
        }
    }

    /// Whether the owner is allowed to write this result back.
    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if table.delegate().editable == editable {
                return;
            }
            let delegate = table.delegate_mut();
            delegate.editable = editable;
            delegate.editing = None;
            if !editable {
                delegate.edits.clear();
            }
            cx.notify();
        });
    }

    /// Open the editor on a cell, seeded with the value as it stands.
    pub fn begin_edit(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_editor(cx);

        if !self.table.read(cx).delegate().is_editable(row_ix, col_ix) {
            return;
        }

        // A NULL opens empty: there is no text to put in front of the cursor,
        // and leaving it empty stores an empty string rather than a NULL.
        let value = self
            .table
            .read(cx)
            .delegate()
            .cell(row_ix, col_ix)
            .clone()
            .unwrap_or_default();

        self.editor
            .update(cx, |editor, cx| editor.set_value(value, window, cx));
        self.table.update(cx, |table, cx| {
            table.delegate_mut().editing = Some((row_ix, col_ix));
            cx.notify();
        });
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
    }

    /// Open the editor on the selected cell.
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((row_ix, col_ix)) = self.table.read(cx).selected_cell() {
            self.begin_edit(row_ix, col_ix, window, cx);
        }
    }

    /// Stage SQL `NULL` on the selected cell, whatever is in the editor.
    pub fn set_null(&mut self, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.table.read(cx).selected_cell() else {
            return;
        };
        if !self.table.read(cx).delegate().is_editable(row_ix, col_ix) {
            return;
        }

        self.cancel_editor(cx);
        self.table.update(cx, |table, cx| {
            table.delegate_mut().stage(row_ix, col_ix, None);
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Fold whatever is in the open editor into the staged edits.
    ///
    /// Safe to call when nothing is being edited, which is what lets both the
    /// blur and the new selection call it without coordinating.
    pub fn commit_editor(&mut self, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.table.read(cx).delegate().editing else {
            return;
        };

        let text = self.editor.read(cx).value().to_string();
        let coerce = Settings::global(cx).coerce_null_literal;
        let value = if coerce && text.eq_ignore_ascii_case("null") {
            None
        } else {
            Some(text)
        };

        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.editing = None;
            delegate.stage(row_ix, col_ix, value);
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Close the editor, keeping the cell as it was.
    pub fn cancel_editor(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if table.delegate().editing.is_none() {
                return;
            }
            table.delegate_mut().editing = None;
            cx.notify();
        });
    }

    /// Edits waiting to be written, by row, in result order.
    pub fn staged(&self, cx: &App) -> Vec<StagedRow> {
        let delegate = self.table.read(cx).delegate();
        let mut rows: HashMap<usize, Vec<(usize, Cell)>> = HashMap::new();
        for ((row, col), value) in &delegate.edits {
            rows.entry(*row).or_default().push((*col, value.clone()));
        }

        let mut staged: Vec<StagedRow> = rows
            .into_iter()
            .map(|(row, mut cells)| {
                cells.sort_by_key(|(col, _)| *col);
                StagedRow { row, cells }
            })
            .collect();
        staged.sort_by_key(|staged| staged.row);
        staged
    }

    fn row_has_edits(&self, row: usize, cx: &App) -> bool {
        self.table
            .read(cx)
            .delegate()
            .edits
            .keys()
            .any(|(staged, _)| *staged == row)
    }

    /// The row as it was loaded, untouched by staged edits.
    ///
    /// What addresses a row in a write has to be the value the server has, not
    /// the one being typed over it — the key itself may be what changed.
    pub fn baseline_row(&self, row: usize, cx: &App) -> Option<Vec<Cell>> {
        self.table.read(cx).delegate().result.rows.get(row).cloned()
    }

    /// Take `rows`' staged edits as written: they become the loaded values.
    pub fn apply_staged(&mut self, rows: &[usize], cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            for row in rows {
                let columns: Vec<usize> = delegate
                    .edits
                    .keys()
                    .filter(|(staged, _)| staged == row)
                    .map(|(_, col)| *col)
                    .collect();

                for col in columns {
                    let Some(value) = delegate.edits.remove(&(*row, col)) else {
                        continue;
                    };
                    if let Some(cell) = delegate
                        .result
                        .rows
                        .get_mut(*row)
                        .and_then(|row| row.get_mut(col))
                    {
                        *cell = value;
                    }
                }
            }
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Throw every staged edit away.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.edits.is_empty() && delegate.editing.is_none() {
                return;
            }
            delegate.edits.clear();
            delegate.editing = None;
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    pub fn set_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        self.set_sorted_result(result, None, cx);
    }

    /// Show `result`, marking its header as sorted by `sort`.
    ///
    /// The table rebuilds its headers from the delegate whenever the rows
    /// change, so an owner that sorts on the server has to hand its sort back.
    /// Without it the header returns to unsorted and the next click on it
    /// starts the cycle over, which makes every click sort descending.
    pub fn set_sorted_result(
        &mut self,
        result: QueryResult,
        sort: Option<(String, ColumnSort)>,
        cx: &mut Context<Self>,
    ) {
        self.has_result = true;
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.widths = measure_columns(&result);
            delegate.order = (0..result.rows.len()).collect();
            delegate.result = result;
            delegate.selected_row = None;
            delegate.sorted_by = sort;
            // New rows mean the staged ones are gone: paging, sorting, and
            // refreshing all throw unwritten edits away.
            delegate.edits.clear();
            delegate.editing = None;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.has_result = false;
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.result = QueryResult::default();
            delegate.order = Vec::new();
            delegate.widths = Vec::new();
            delegate.selected_row = None;
            delegate.sorted_by = None;
            delegate.edits.clear();
            delegate.editing = None;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.notify();
    }

    fn is_empty(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().result.columns.is_empty()
    }

    /// Put the keyboard focus on the grid, the way clicking a cell does.
    #[cfg(test)]
    pub(crate) fn focus_for_test(&self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::Focusable as _;
        let handle = self.table.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    #[cfg(test)]
    pub(crate) fn editing_for_test(&self, cx: &App) -> Option<(usize, usize)> {
        self.table.read(cx).delegate().editing
    }

    #[cfg(test)]
    pub(crate) fn editable_for_test(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().editable
    }

    /// Open the cell editor the way double-clicking a cell does.
    #[cfg(test)]
    pub(crate) fn begin_edit_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_edit(row_ix, col_ix, window, cx);
    }

    /// Put `value` in the open editor, the way typing into it does.
    #[cfg(test)]
    pub(crate) fn set_editor_value_for_test(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.set_value(value.to_string(), window, cx)
        });
    }

    /// The value a cell shows, staged edit included.
    #[cfg(test)]
    pub(crate) fn cell_for_test(&self, row_ix: usize, col_ix: usize, cx: &App) -> Cell {
        self.table.read(cx).delegate().cell(row_ix, col_ix).clone()
    }

    /// Select a cell the way clicking one does.
    #[cfg(test)]
    pub(crate) fn select_cell_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<Self>,
    ) {
        self.table
            .update(cx, |table, cx| table.set_selected_cell(row_ix, col_ix, cx));
    }

    /// The row currently highlighted, and the cell the selection sits on.
    #[cfg(test)]
    pub(crate) fn selection_for_test(&self, cx: &App) -> (Option<usize>, Option<(usize, usize)>) {
        let table = self.table.read(cx);
        (table.delegate().selected_row, table.selected_cell())
    }

    /// Sort by a column the way clicking its header does.
    #[cfg(test)]
    pub(crate) fn sort_for_test(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().perform_sort(col_ix, sort, window, cx)
        });
    }

    /// The column the header is marked as sorted by.
    #[cfg(test)]
    pub(crate) fn sorted_for_test(&self, cx: &App) -> Option<(String, ColumnSort)> {
        self.table.read(cx).delegate().sorted_by.clone()
    }

    /// One column of every row, in display order.
    #[cfg(test)]
    pub(crate) fn column_values_for_test(&self, col_ix: usize, cx: &App) -> Vec<Option<String>> {
        let delegate = self.table.read(cx).delegate();
        (0..delegate.order.len())
            .map(|row_ix| delegate.cell(row_ix, col_ix).clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn column_widths_for_test(&self, cx: &App) -> Vec<Pixels> {
        self.table.read(cx).delegate().widths.clone()
    }
}

impl Render for DataGrid {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_empty(cx) {
            let message = if self.has_result {
                "Statement returned no columns"
            } else {
                "Run a query to see results"
            };

            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .into_any_element();
        }

        h_flex()
            .size_full()
            .child(
                DataTable::new(&self.table)
                    .xsmall()
                    .stripe(true)
                    .bordered(false),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(columns: &[&str], rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|name| name.to_string()).collect(),
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| cell.map(|value| value.to_string()))
                        .collect()
                })
                .collect(),
            ..QueryResult::default()
        }
    }

    #[test]
    fn narrow_columns_get_the_minimum_width() {
        let widths = measure_columns(&result(&["id"], vec![vec![Some("1")], vec![Some("2")]]));
        assert_eq!(widths, [px(MIN_COLUMN_WIDTH)]);
    }

    #[test]
    fn wide_values_are_capped() {
        let long = "x".repeat(500);
        let widths = measure_columns(&result(&["blob"], vec![vec![Some(long.as_str())]]));
        assert_eq!(widths, [px(MAX_COLUMN_WIDTH)]);
    }

    #[test]
    fn width_follows_the_longest_sampled_value() {
        let widths = measure_columns(&result(
            &["name"],
            vec![
                vec![Some("ada")],
                vec![Some("a rather longer value")],
                vec![None],
            ],
        ));

        let expected = "a rather longer value".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn the_header_widens_a_column_of_short_values() {
        let widths = measure_columns(&result(
            &["a_column_with_a_long_name"],
            vec![vec![Some("1")]],
        ));

        let expected = "a_column_with_a_long_name".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn only_the_first_rows_are_sampled() {
        let sampled = "a twenty char value.";
        let long = "x".repeat(300);
        let mut rows: Vec<Vec<Option<&str>>> = vec![vec![Some(sampled)]; SAMPLE_ROWS];
        rows.push(vec![Some(long.as_str())]);

        let widths = measure_columns(&result(&["value"], rows));
        assert_eq!(
            widths,
            [px(sampled.len() as f32 * CHARACTER_WIDTH + CELL_PADDING)],
            "a long value past the sample should not widen the column"
        );
    }
}
