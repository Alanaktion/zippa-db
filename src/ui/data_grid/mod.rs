//! Result grid.
//!
//! The table virtualizes rows and columns, and cells can be typed into when
//! the owner allows it: edits are staged in an overlay over the result and
//! handed back for the owner to write. Specialized cell renderers (a date
//! picker, say) are still ahead (TODO.md section 2).
//!
//! The work is split by concern: [`layout`] sizes the columns, [`format`] lays
//! one value out for reading, [`delegate`] holds the result together with
//! everything staged on top of it, and [`clipboard`] decides what a copy takes.
//! What is left here is the view — the events it emits, and the commands its
//! owner drives it with.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::table::{ColumnSort, DataTable, TableEvent, TableState};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseUpEvent, Window, actions, div,
};

use crate::db::Engine;
use crate::db::export::Format;
use crate::db::query::{self, Cell, QueryResult};
use crate::settings::{self, Settings};
use crate::ui::value_dialog::{self, ValueRequest};

mod clipboard;
mod delegate;
mod format;
mod layout;
#[cfg(test)]
mod test_support;

use delegate::ResultDelegate;
use layout::measure_columns;

pub use format::{format_value, needs_a_window};

actions!(
    zippa_db,
    [
        ViewCell,
        /// Copy what is selected: the picked rows, or the selected cell.
        CopyValue,
        /// Copy the rows shown with a header line, tab-separated.
        CopyWithHeaders,
        /// Pick every row out.
        SelectAllRows,
        /// Put every picked row back.
        ClearRowSelection,
        /// Pick the focused row out, or put it back.
        ToggleRow,
        /// Move the selection up a row, taking the rows it passes with it.
        ExtendSelectionUp,
        /// The same, downwards.
        ExtendSelectionDown,
    ]
);

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

/// Reports a foreign key jump on a cell, from the menu the delegate built.
type NavReporter = Rc<dyn Fn(usize, usize, &mut App)>;

/// Copies from that menu: the cell the click landed on, or — with no cell —
/// whatever is selected.
type CopyReporter = Rc<dyn Fn(Option<(usize, usize)>, &mut App)>;

/// Asks the owner to export the picked rows, in the chosen format, from the
/// menu the delegate built.
type ExportReporter = Rc<dyn Fn(Format, &mut App)>;

/// Asks the grid to copy what the menu is about, in a chosen shape, from the
/// menu the delegate built.
type CopyAsReporter = Rc<dyn Fn(CopyAs, &mut App)>;

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
    /// The keyboard's row changed, or was reset by a new page. `None` when no
    /// row has it.
    RowFocused(Option<usize>),
}

/// What a row shown in the grid is, for an owner that lays one out apart from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Loaded,
    Draft,
    Deleted,
}

/// A foreign key jump was asked for from the row menu.
pub struct GridNavigate {
    pub row: usize,
    pub col: usize,
}

/// A binary cell whose value the driver could only describe (`<N bytes>`) was
/// asked to be viewed. The dialog has already opened on that stand-in; an
/// owner that can re-read the row — a table view, never a bare query result —
/// may answer with the real bytes and replace it. `row` is the result's own
/// row index, the same as [`StagedRow::row`], not the display order.
pub struct BinaryPreviewRequested {
    pub row: usize,
    pub column: String,
}

/// The picked rows were asked to be exported, in `format`.
pub struct ExportRequested {
    pub format: Format,
}

/// What a `Copy as` menu item asks the grid to put on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyAs {
    /// The rows a copy acts on, laid out in [`Format`].
    Rows(Format),
    /// One column, one value per line.
    ColumnValues(usize),
    /// One column as a SQL `IN (...)` list.
    ColumnInList(usize),
}

/// Which rows — and how many columns — a [`DataGrid::snapshot`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The rows picked out with the checkboxes, in display order. Empty when
    /// nothing is picked.
    Picked,
    /// Every row the grid is showing, picked or not.
    All,
    /// One column of the rows a copy acts on: the picked ones when any are
    /// picked, every row shown otherwise.
    Column(usize),
}

/// A result laid out for copying or exporting: the columns and driver types it
/// was read under, and one row of cells per row a [`Scope`] named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub columns: Vec<String>,
    pub types: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
}

/// What a copy put on the clipboard, for the owner's status line.
pub struct Copied {
    pub message: String,
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

impl Focusable for DataGrid {
    /// The table is what answers the keyboard — the arrows, the row commands,
    /// and the keys the grid binds on its own context all reach it from
    /// there — so whoever hands the grid the focus lands on it rather than on
    /// an element above it that would swallow every keystroke.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.table.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<SortRequested> for DataGrid {}
impl EventEmitter<GridEdit> for DataGrid {}
impl EventEmitter<GridNavigate> for DataGrid {}
impl EventEmitter<BinaryPreviewRequested> for DataGrid {}
impl EventEmitter<ExportRequested> for DataGrid {}
impl EventEmitter<Copied> for DataGrid {}

impl DataGrid {
    pub fn new(engine: Engine, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_sorting(Sorting::InPlace, engine, window, cx)
    }

    pub fn with_sorting(
        sorting: Sorting,
        engine: Engine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

        let report_navigate: NavReporter = Rc::new({
            let grid = cx.weak_entity();
            move |row_ix: usize, col_ix: usize, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |_, cx| {
                    cx.emit(GridNavigate {
                        row: row_ix,
                        col: col_ix,
                    })
                });
            }
        });

        let report_copy: CopyReporter = Rc::new({
            let grid = cx.weak_entity();
            move |cell: Option<(usize, usize)>, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |grid, cx| grid.copy(cell, cx));
            }
        });

        let report_export: ExportReporter = Rc::new({
            let grid = cx.weak_entity();
            move |format: Format, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |_, cx| cx.emit(ExportRequested { format }));
            }
        });

        let report_copy_as: CopyAsReporter = Rc::new({
            let grid = cx.weak_entity();
            move |what: CopyAs, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                grid.update(cx, |grid, cx| grid.copy_as(what, cx));
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
                    focused_row: None,
                    font,
                    sorting,
                    sorted_by: None,
                    report_sort,
                    edits: HashMap::new(),
                    editable: false,
                    rows_selected: HashSet::new(),
                    anchor: None,
                    sweeping: None,
                    menu_row: None,
                    menu_cell: None,
                    deletions: HashSet::new(),
                    report_change,
                    report_view,
                    report_navigate,
                    report_copy,
                    report_export,
                    report_copy_as,
                    engine,
                    table: None,
                    foreign_keys: HashSet::new(),
                    drafts: Vec::new(),
                    editing: None,
                    editor: editor.clone(),
                },
                window,
                cx,
            )
            .cell_selectable(true)
            // Rows are picked out with the checkbox column, so the table's own
            // row header strip and its row selection mode are turned off: two
            // ways to select a row that mean different things is what made the
            // highlight inconsistent, and the table's mode also left the
            // selected cell — and with it edit, copy, and view value —
            // unset.
            .row_header(false)
            .row_selectable(false)
            // The end of the rows is the end of the rows: an arrow that
            // reappears at the other edge loses the user's place in a result
            // that is thousands of rows long.
            .loop_selection(false)
            // A dragged header would reorder only the table's own column
            // list: every cell, edit, copy, and write here is addressed by the
            // result's column index, so the grid keeps the result's order.
            .col_movable(false)
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
        // The checkbox column is not a value, so the selection never rests on
        // it: an arrow, Home, or a click on a box moves it to the first column
        // that holds one. Without this the selected cell has nothing to copy,
        // edit, or view, and the first left arrow out of a row looks like
        // nothing happened.
        if let TableEvent::SelectCell(row_ix, col_ix) = event
            && table.read(cx).delegate().data_column(*col_ix).is_none()
        {
            let row_ix = *row_ix;
            if !self.is_empty(cx) {
                let first = ResultDelegate::shown_column(0);
                table.update(cx, |table, cx| table.set_selected_cell(row_ix, first, cx));
            }
            return;
        }

        // The table falls back to row selection when a key arrives with
        // nothing selected yet. The grid only works in cells, so that becomes
        // a cell on the same row.
        if let TableEvent::SelectRow(row_ix) = event {
            let row_ix = *row_ix;
            if !self.is_empty(cx) {
                let first = ResultDelegate::shown_column(0);
                table.update(cx, |table, cx| table.set_selected_cell(row_ix, first, cx));
            }
            return;
        }

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
            TableEvent::ClearSelection => None,
            _ => return,
        };

        // Fold the open editor in first: the click that moves the selection
        // also blurs the input, and the order the two arrive in is not ours.
        self.commit_editor(cx);

        let previous = table.read(cx).delegate().focused_row;
        if previous == row {
            return;
        }

        table.update(cx, |table, cx| {
            table.delegate_mut().focused_row = row;
            cx.notify();
        });
        cx.emit(GridEdit::RowFocused(row));

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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Enter finishes the cell; losing focus does the same, so clicking
        // away never drops what was typed.
        if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
            return;
        }

        let finished = matches!(event, InputEvent::PressEnter { .. });
        self.commit_editor(cx);

        // Finishing with the keyboard hands the keyboard back to the rows, so
        // the next arrow moves the selection instead of going nowhere. A blur
        // is the user putting the focus somewhere themselves; taking it back
        // would fight them for it.
        if finished {
            self.focus_table(window, cx);
        }
    }

    /// Put the keyboard back on the table, the way clicking a cell does.
    fn focus_table(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle(cx).focus(window, cx);
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

    /// Say which columns hold a single-column foreign key, by index, so the
    /// row menu can offer to jump to the referenced row. Only the table view
    /// knows this, since it is the one that reads the table's own schema.
    pub fn set_foreign_keys(&mut self, columns: HashSet<usize>, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().foreign_keys = columns;
            cx.notify();
        });
    }

    /// Name the table this result came from, so the row menu can offer a SQL
    /// `INSERT` copy. A query result never gets one, and offers no SQL copy as a
    /// result.
    pub fn set_table(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            if table.delegate().table == name {
                return;
            }
            table.delegate_mut().table = name;
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
    pub fn set_null(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
            return;
        };
        if !self.table.read(cx).delegate().is_editable(row_ix, col_ix) {
            return;
        }

        self.cancel_editor(window, cx);
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
        let value = staged_value(text, Settings::global(cx).coerce_null_literal);

        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.editing = None;
            delegate.stage(row_ix, col_ix, value);
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Close the editor, keeping the cell as it was.
    ///
    /// The keyboard goes back to the table: giving up on a cell should leave
    /// the grid exactly as finishing one does.
    pub fn cancel_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_editor(cx) {
            self.focus_table(window, cx);
        }
    }

    /// Close the editor without touching the focus; says whether one was open.
    fn close_editor(&mut self, cx: &mut Context<Self>) -> bool {
        self.table.update(cx, |table, cx| {
            if table.delegate().editing.is_none() {
                return false;
            }
            table.delegate_mut().editing = None;
            cx.notify();
            true
        })
    }

    /// Start a row the user fills in by hand, below the ones on screen.
    pub fn add_draft(&mut self, cx: &mut Context<Self>) {
        if !self.table.read(cx).delegate().editable {
            return;
        }
        self.commit_editor(cx);
        self.table.update(cx, |table, cx| {
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
        let (edited, inserted, deleted) = self.pending_counts(cx);
        edited + inserted + deleted
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

    /// Whether there is work a close would lose: staged edits, rows being built
    /// by hand, rows marked for deletion, or a cell editor still open on a value
    /// that was changed but not yet folded in.
    pub fn has_unsaved_edits(&self, cx: &App) -> bool {
        if self.pending(cx) > 0 {
            return true;
        }

        let delegate = self.table.read(cx).delegate();
        let Some((row_ix, col_ix)) = delegate.editing else {
            return false;
        };
        let text = delegate.editor.read(cx).value().to_string();
        let value = staged_value(text, Settings::global(cx).coerce_null_literal);
        delegate.cell(row_ix, col_ix) != &value
    }

    /// The menu a right click anywhere in the grid opens.
    ///
    /// Built when the menu opens rather than now, so it is about the row the
    /// click landed on and knows how many rows the sweep holds by then.
    fn row_menu(
        &self,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let table = self.table.downgrade();

        move |menu, window, cx| {
            let Some(table) = table.upgrade() else {
                return menu;
            };

            // A click that landed on no row — the header, or the space below
            // the last one — has nothing to offer. The selected column is the
            // fallback for the menu's column copies when the click was beside
            // the cells rather than on one.
            let (row_ix, selected_column) = {
                let table = table.read(cx);
                (
                    table.delegate().menu_row,
                    table
                        .selected_cell()
                        .and_then(|(_, col)| table.delegate().data_column(col)),
                )
            };
            let Some(row_ix) = row_ix else {
                return menu;
            };

            table.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .row_menu_items(row_ix, menu, selected_column, window, cx)
            })
        }
    }

    fn on_view_cell(&mut self, _: &ViewCell, _window: &mut Window, cx: &mut Context<Self>) {
        self.view_selected(cx);
    }

    fn on_select_all_rows(
        &mut self,
        _: &SelectAllRows,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |table, cx| {
            let rows = table.delegate().rows();
            table.delegate_mut().pick_all(true, rows);
            cx.notify();
        });
    }

    fn on_clear_row_selection(
        &mut self,
        _: &ClearRowSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_row_selection(cx);
    }

    /// Pick the focused row out, or put it back, the way its box does.
    fn on_toggle_row(&mut self, _: &ToggleRow, _window: &mut Window, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let Some(row_ix) = table.delegate().focused_row else {
                return;
            };
            table.delegate_mut().pick(row_ix, false);
            cx.notify();
        });
    }

    fn on_extend_up(&mut self, _: &ExtendSelectionUp, window: &mut Window, cx: &mut Context<Self>) {
        self.extend_selection(-1, window, cx);
    }

    fn on_extend_down(
        &mut self,
        _: &ExtendSelectionDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.extend_selection(1, window, cx);
    }

    /// Move the selection one row and take the rows it passed with it.
    ///
    /// The first shifted arrow takes the row the selection was already on, so
    /// holding shift and pressing down twice leaves three rows picked out.
    fn extend_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_editor(cx);

        let rows = self.table.read(cx).delegate().rows();
        if rows == 0 {
            return;
        }

        // Nothing selected yet: the shifted arrow starts at the top rather
        // than doing nothing.
        let (row_ix, col_ix) = match self.table.read(cx).selected_cell() {
            Some(cell) => cell,
            None => (0, ResultDelegate::shown_column(0)),
        };
        let next = row_ix.saturating_add_signed(step).min(rows - 1);

        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if delegate.anchor.is_none() {
                delegate.anchor_at(row_ix);
            }
            delegate.sweep_to(next, true);
            table.set_selected_cell(next, col_ix, cx);
        });
        self.focus_table(window, cx);
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
        // What the box opens with, and what the save closure compares against:
        // the display form, not the value itself.
        let text = format_value(&value, &type_name);

        // Nothing was read back to show, but an owner that can address this
        // row by its key may be able to fetch the real bytes and replace the
        // dialog with a preview of them.
        let binary_preview_row = (query::is_placeholder(&value)
            && query::is_binary_type(&type_name))
        .then(|| delegate.source(row_ix))
        .flatten();

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
            let opened_with = text.clone();
            Rc::new(move |typed: String, cx: &mut App| {
                let Some(grid) = grid.upgrade() else {
                    return;
                };
                // The box opens with the value laid out for reading — `NULL`,
                // JSON indented — so saving it as it stood is not an edit. This
                // is what keeps an untouched `NULL` a `NULL` rather than the
                // four characters the box spelled it with.
                if typed == opened_with {
                    return;
                }
                grid.update(cx, |grid, cx| {
                    let value = staged_value(typed, Settings::global(cx).coerce_null_literal);
                    grid.stage_cell(row_ix, col_ix, value, cx)
                });
            }) as value_dialog::Save
        });

        value_dialog::open(
            ValueRequest {
                column: column.clone().into(),
                text,
                note: note.map(Into::into),
                save,
                preview: None,
            },
            cx,
        );

        if let Some(row) = binary_preview_row {
            cx.emit(BinaryPreviewRequested { row, column });
        }
    }

    /// Show the selected cell's value.
    pub fn view_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((row_ix, col_ix)) = self.selected_cell(cx) {
            self.view_cell(row_ix, col_ix, cx);
        }
    }

    /// Stage a value on a cell: what the value window was saved with, or what
    /// a side panel typed. `None` stages a NULL.
    pub fn stage_cell(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        value: Cell,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |table, cx| {
            if !table.delegate().is_editable(row_ix, col_ix) {
                return;
            }
            table.delegate_mut().stage(row_ix, col_ix, value);
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

    /// The row the keyboard is on, if any.
    pub fn focused_row(&self, cx: &App) -> Option<usize> {
        self.table.read(cx).delegate().focused_row
    }

    /// The row as it stands, staged edits and draft values included; `None`
    /// when `row_ix` is past the last row shown.
    pub fn row_cells(&self, row_ix: usize, cx: &App) -> Option<Vec<Cell>> {
        let delegate = self.table.read(cx).delegate();
        if row_ix >= delegate.rows() {
            return None;
        }
        Some(
            (0..delegate.result.columns.len())
                .map(|col_ix| delegate.cell(row_ix, col_ix).clone())
                .collect(),
        )
    }

    /// Whether this cell can be typed into.
    pub fn is_field_editable(&self, row_ix: usize, col_ix: usize, cx: &App) -> bool {
        self.table.read(cx).delegate().is_editable(row_ix, col_ix)
    }

    /// Whether this cell holds a staged edit, or is a new row's typed value.
    pub fn is_field_staged(&self, row_ix: usize, col_ix: usize, cx: &App) -> bool {
        self.table.read(cx).delegate().is_staged(row_ix, col_ix)
    }

    /// Take a cell's staged value back: a loaded cell returns to what the
    /// server has, and a new row's cell goes back to the server's default.
    pub fn reset_cell(&mut self, row_ix: usize, col_ix: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            if let Some(draft) = delegate.draft(row_ix) {
                delegate.drafts[draft].remove(&col_ix);
            } else if let Some(source) = delegate.source(row_ix) {
                delegate.edits.remove(&(source, col_ix));
            }
            cx.notify();
        });
        cx.emit(GridEdit::Staged);
    }

    /// Whether the row shown at `row_ix` is loaded, being built, or on its way out.
    pub fn row_status(&self, row_ix: usize, cx: &App) -> Option<RowStatus> {
        let delegate = self.table.read(cx).delegate();
        if row_ix >= delegate.rows() {
            return None;
        }
        Some(if delegate.draft(row_ix).is_some() {
            RowStatus::Draft
        } else if delegate.is_deleted(row_ix) {
            RowStatus::Deleted
        } else {
            RowStatus::Loaded
        })
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

    /// Put the selection on the first cell of the data column named `name`,
    /// and scroll it into view.
    ///
    /// Returns `false` when the grid has no such column — what a caller sees
    /// when it opens a table and asks before the page has loaded, so it can
    /// ask again later.
    pub fn reveal_column(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Some(data_ix) = self
            .table
            .read(cx)
            .delegate()
            .result
            .columns
            .iter()
            .position(|column| column == name)
        else {
            return false;
        };
        let shown = ResultDelegate::shown_column(data_ix);
        let has_row = !self.table.read(cx).delegate().result.rows.is_empty();
        self.table.update(cx, |table, cx| {
            // An empty page still scrolls to the column's header; a cell
            // cannot be selected where there is no row.
            if has_row {
                table.set_selected_cell(0, shown, cx);
            }
            table.scroll_to_col(shown, cx);
        });
        true
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
            delegate.focused_row = None;
            delegate.sorted_by = sort;
            // New rows mean the staged ones are gone: paging, sorting, and
            // refreshing all throw unwritten edits away.
            delegate.edits.clear();
            delegate.editing = None;
            delegate.drafts.clear();
            delegate.deletions.clear();
            delegate.rows_selected.clear();
            delegate.anchor = None;
            delegate.sweeping = None;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.emit(GridEdit::RowFocused(None));
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.has_result = false;
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.result = QueryResult::default();
            delegate.order = Vec::new();
            delegate.widths = Vec::new();
            delegate.focused_row = None;
            delegate.sorted_by = None;
            delegate.edits.clear();
            delegate.editing = None;
            delegate.drafts.clear();
            delegate.deletions.clear();
            delegate.rows_selected.clear();
            delegate.anchor = None;
            delegate.sweeping = None;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.emit(GridEdit::RowFocused(None));
        cx.notify();
    }

    fn is_empty(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().result.columns.is_empty()
    }
}

/// The value a typed string stages: `NULL` in any capitalisation means SQL
/// `NULL` when the setting says so, and the text itself otherwise.
///
/// Shared by the cell editor and the value dialog, so the two agree on what
/// typing `null` into a cell means.
fn staged_value(text: String, coerce: bool) -> Cell {
    if coerce && text.eq_ignore_ascii_case("null") {
        None
    } else {
        Some(text)
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
            .on_action(cx.listener(Self::on_copy_value))
            .on_action(cx.listener(Self::on_copy_with_headers))
            .on_action(cx.listener(Self::on_select_all_rows))
            .on_action(cx.listener(Self::on_clear_row_selection))
            .on_action(cx.listener(Self::on_toggle_row))
            .on_action(cx.listener(Self::on_extend_up))
            .on_action(cx.listener(Self::on_extend_down))
            // Releasing the button ends a sweep across the pick boxes, so a
            // later drag that happens to pass over them picks nothing up.
            // Captured, since the table stops a click on a cell before it
            // reaches this element.
            .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                this.table.update(cx, |table, _cx| {
                    table.delegate_mut().sweeping = None;
                });
            }))
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
