//! Result grid.
//!
//! TODO.md section 2. The table virtualizes rows and columns, and cells can be
//! typed into when the owner allows it: edits are staged in an overlay over the
//! result and handed back for the owner to write. Foreign key jumps and
//! specialized cell renderers come later.
//!
//! The work is split three ways: [`layout`] sizes the columns, [`format`] lays
//! one value out for reading, and [`delegate`] holds the result together with
//! everything staged on top of it. What is left here is the view — the events
//! it emits, and the commands its owner drives it with.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
#[cfg(test)]
use gpui_kit::component::table::TableDelegate;
use gpui_kit::component::table::{ColumnSort, DataTable, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Window, actions, div};

use crate::db::query::{self, Cell, QueryResult};
use crate::settings::{self, Settings};
use crate::ui::value_dialog::{self, ValueRequest};

mod delegate;
mod format;
mod layout;

use delegate::ResultDelegate;
use layout::measure_columns;

pub use format::{format_value, needs_a_window};

actions!(zippa_db, [ViewCell]);

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

/// Tells the grid's owner that the staged changes moved.
type ChangeReporter = Rc<dyn Fn(&mut App)>;

/// Opens the value viewer on a cell, from the menu the delegate built.
type ViewReporter = Rc<dyn Fn(usize, usize, &mut App)>;

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

        let report_change: ChangeReporter = Rc::new({
            let grid = cx.weak_entity();
            move |cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |_, cx| cx.emit(GridEdit::Staged));
            }
        });

        let report_view: ViewReporter = Rc::new({
            let grid = cx.weak_entity();
            move |row_ix: usize, col_ix: usize, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |grid, cx| grid.view_cell(row_ix, col_ix, cx));
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
                    rows_selected: HashSet::new(),
                    anchor: None,
                    menu_row: None,
                    menu_cell: None,
                    deletions: HashSet::new(),
                    report_change,
                    report_view,
                    drafts: Vec::new(),
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
            // The stripe setting is read while the grid renders, so a change
            // to it only shows once the grid is drawn again.
            cx.notify();
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
            let row_ix = *row_ix;
            // The table counts the checkbox column; everything below counts
            // the result's own columns.
            let Some(col_ix) = table.read(cx).delegate().data_column(*col_ix) else {
                return;
            };
            // A value a grid cell cannot show is opened in full instead of
            // being typed into through a one-line box.
            let delegate = table.read(cx).delegate();
            let type_name = delegate
                .result
                .column_types
                .get(col_ix)
                .cloned()
                .unwrap_or_default();
            let big = needs_a_window(delegate.cell(row_ix, col_ix), &type_name);

            if big {
                self.view_cell(row_ix, col_ix, cx);
            } else {
                self.begin_edit(row_ix, col_ix, window, cx);
            }
            return;
        }

        if matches!(event, TableEvent::ClearSelection) {
            self.clear_row_selection(cx);
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
                delegate.drafts.clear();
                delegate.deletions.clear();
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

    /// The selected cell, counted in the result's own columns.
    fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        let table = self.table.read(cx);
        let (row_ix, col_ix) = table.selected_cell()?;
        Some((row_ix, table.delegate().data_column(col_ix)?))
    }

    /// Open the editor on the selected cell.
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((row_ix, col_ix)) = self.selected_cell(cx) {
            self.begin_edit(row_ix, col_ix, window, cx);
        }
    }

    /// Stage SQL `NULL` on the selected cell, whatever is in the editor.
    pub fn set_null(&mut self, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
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

    /// Start a row the user fills in by hand, below the ones on screen.
    pub fn add_draft(&mut self, cx: &mut Context<Self>) {
        self.commit_editor(cx);
        self.table.update(cx, |table, cx| {
            if !table.delegate().editable {
                return;
            }
            table.delegate_mut().drafts.push(HashMap::new());
            table.refresh(cx);
        });
        cx.emit(GridEdit::Staged);
    }

    /// The rows being built by hand, each one the cells typed into it.
    ///
    /// A column nobody typed into is absent, so the `INSERT` can leave it out
    /// and let the server put its own default there.
    pub fn drafts(&self, cx: &App) -> Vec<Vec<(usize, Cell)>> {
        self.table
            .read(cx)
            .delegate()
            .drafts
            .iter()
            .map(|draft| {
                let mut cells: Vec<(usize, Cell)> = draft
                    .iter()
                    .map(|(col, value)| (*col, value.clone()))
                    .collect();
                cells.sort_by_key(|(col, _)| *col);
                cells
            })
            .collect()
    }

    /// How many rows are waiting to be written: edited ones and new ones.
    pub fn pending(&self, cx: &App) -> usize {
        let delegate = self.table.read(cx).delegate();
        let edited = self
            .staged(cx)
            .into_iter()
            .filter(|staged| !delegate.deletions.contains(&staged.row))
            .count();
        edited + delegate.drafts.len() + delegate.deletions.len()
    }

    /// The changes waiting to be written, counted the way they are shown:
    /// edited rows, new rows, deleted rows.
    pub fn pending_counts(&self, cx: &App) -> (usize, usize, usize) {
        let delegate = self.table.read(cx).delegate();
        let edited = self
            .staged(cx)
            .into_iter()
            .filter(|staged| !delegate.deletions.contains(&staged.row))
            .count();
        (edited, delegate.drafts.len(), delegate.deletions.len())
    }

    /// The menu a right click anywhere in the grid opens.
    ///
    /// Built when the menu opens rather than now, so it is about the row the
    /// click landed on and knows how many rows the sweep holds by then.
    fn row_menu(
        &self,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let table = self.table.downgrade();

        move |menu, _window, cx| {
            let Some(table) = table.upgrade() else {
                return menu;
            };

            // A click that landed on no row — the header, or the space below
            // the last one — has nothing to offer.
            let Some(row_ix) = table.read(cx).delegate().menu_row else {
                return menu;
            };

            table.update(cx, |table, cx| {
                table.delegate_mut().row_menu_items(row_ix, menu, cx)
            })
        }
    }

    fn on_view_cell(&mut self, _: &ViewCell, _window: &mut Window, cx: &mut Context<Self>) {
        self.view_selected(cx);
    }

    /// Show a cell's whole value in a window of its own.
    pub fn view_cell(&mut self, row_ix: usize, col_ix: usize, cx: &mut Context<Self>) {
        self.commit_editor(cx);

        let delegate = self.table.read(cx).delegate();
        let Some(column) = delegate.result.columns.get(col_ix).cloned() else {
            return;
        };
        let type_name = delegate
            .result
            .column_types
            .get(col_ix)
            .cloned()
            .unwrap_or_default();
        let value = delegate.cell(row_ix, col_ix).clone();

        // The stand-ins the driver hands back describe a value rather than
        // holding it, so there is nothing to edit and the window says why.
        let note = if query::is_placeholder(&value) {
            Some("This value was not read back from the server.")
        } else if !delegate.is_editable(row_ix, col_ix) {
            Some("This value cannot be edited here.")
        } else {
            None
        };

        let save: Option<value_dialog::Save> = delegate.is_editable(row_ix, col_ix).then(|| {
            let grid = cx.weak_entity();
            Rc::new(move |text: String, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |grid, cx| grid.stage_value(row_ix, col_ix, text, cx));
            }) as value_dialog::Save
        });

        value_dialog::open(
            ValueRequest {
                column: column.into(),
                text: format_value(&value, &type_name),
                note: note.map(Into::into),
                save,
            },
            cx,
        );
    }

    /// Show the selected cell's value.
    pub fn view_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((row_ix, col_ix)) = self.selected_cell(cx) {
            self.view_cell(row_ix, col_ix, cx);
        }
    }

    /// Stage what the value window was saved with.
    fn stage_value(&mut self, row_ix: usize, col_ix: usize, value: String, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if !table.delegate().is_editable(row_ix, col_ix) {
                return;
            }
            table.delegate_mut().stage(row_ix, col_ix, Some(value));
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Mark the swept rows for deletion, the way the row menu does.
    pub fn delete_selected(&mut self, cx: &mut Context<Self>) {
        self.set_selected_deleted(true, cx);
    }

    /// Take the deletion mark off the swept rows.
    pub fn restore_selected(&mut self, cx: &mut Context<Self>) {
        self.set_selected_deleted(false, cx);
    }

    fn set_selected_deleted(&mut self, deleted: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if !table.delegate().editable {
                return;
            }
            table.delegate_mut().set_deleted(deleted);
            // Deleting can drop a row being built, so the table is rebuilt
            // rather than redrawn: it caches how many rows there are.
            table.refresh(cx);
        });
        cx.emit(GridEdit::Staged);
    }

    /// Rows marked for deletion, as indices into the result's own rows.
    pub fn deletions(&self, cx: &App) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .table
            .read(cx)
            .delegate()
            .deletions
            .iter()
            .copied()
            .collect();
        rows.sort_unstable();
        rows
    }

    /// Forget which rows were picked out.
    pub fn clear_row_selection(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.rows_selected.is_empty() {
                return;
            }
            delegate.rows_selected.clear();
            delegate.anchor = None;
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

    /// Put the header's sort marker back where the rows actually are.
    ///
    /// The table moves the marker itself when a header is clicked, so an owner
    /// that refuses the sort has to move it back.
    pub fn set_sort_marker(&mut self, sort: Option<(String, ColumnSort)>, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if table.delegate().sorted_by == sort {
                return;
            }
            table.delegate_mut().sorted_by = sort;
            cx.notify();
        });
    }

    /// Throw every staged edit and every row being built away.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.edits.is_empty()
                && delegate.editing.is_none()
                && delegate.drafts.is_empty()
                && delegate.deletions.is_empty()
            {
                return;
            }
            delegate.edits.clear();
            delegate.editing = None;
            delegate.drafts.clear();
            delegate.deletions.clear();
            table.refresh(cx);
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
            delegate.drafts.clear();
            delegate.deletions.clear();
            delegate.rows_selected.clear();
            delegate.anchor = None;
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
            delegate.drafts.clear();
            delegate.deletions.clear();
            delegate.rows_selected.clear();
            delegate.anchor = None;
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

    /// Drop a row being built, the way its menu item does.
    #[cfg(test)]
    pub(crate) fn discard_draft_for_test(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let Some(draft) = table.delegate().draft(row_ix) else {
                return;
            };
            table.delegate_mut().discard_draft(draft);
            table.refresh(cx);
        });
        cx.emit(GridEdit::Staged);
    }

    /// How many rows the grid is showing, drafts included.
    #[cfg(test)]
    pub(crate) fn row_count_for_test(&self, cx: &App) -> usize {
        let delegate = self.table.read(cx).delegate();
        delegate.order.len() + delegate.drafts.len()
    }

    /// The rows a sweep has picked out, in display order.
    #[cfg(test)]
    pub(crate) fn rows_selected_for_test(&self, cx: &App) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .table
            .read(cx)
            .delegate()
            .rows_selected
            .iter()
            .copied()
            .collect();
        rows.sort_unstable();
        rows
    }

    /// Select a cell the way clicking one does.
    #[cfg(test)]
    pub(crate) fn select_cell_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<Self>,
    ) {
        let col_ix = ResultDelegate::shown_column(col_ix);
        self.table
            .update(cx, |table, cx| table.set_selected_cell(row_ix, col_ix, cx));
    }

    /// The row currently highlighted, and the cell the selection sits on.
    #[cfg(test)]
    pub(crate) fn selection_for_test(&self, cx: &App) -> (Option<usize>, Option<(usize, usize)>) {
        (
            self.table.read(cx).delegate().selected_row,
            self.selected_cell(cx),
        )
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
        let col_ix = ResultDelegate::shown_column(col_ix);
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
    pub(crate) fn column_widths_for_test(&self, cx: &App) -> Vec<gpui_kit::Pixels> {
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

        // One menu for the whole grid, rather than one per cell: the row it is
        // about is the one the click landed on, noted by the row itself.
        h_flex()
            .size_full()
            .id("grid")
            .relative()
            .key_context("DataGrid")
            .on_action(cx.listener(Self::on_view_cell))
            .context_menu(self.row_menu())
            .child(
                DataTable::new(&self.table)
                    .xsmall()
                    .stripe(Settings::global(cx).stripe_rows)
                    .bordered(false),
            )
            .into_any_element()
    }
}
