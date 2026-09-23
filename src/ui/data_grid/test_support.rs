//! Test-only reach-ins, for `src/ui/tests/`.

use gpui_kit::component::table::ColumnSort;
#[cfg(test)]
use gpui_kit::component::table::TableDelegate;
use gpui_kit::{App, Context, Focusable, Window};

use crate::db::query::Cell;

use super::delegate::ResultDelegate;

use super::{DataGrid, GridEdit, GridNavigate};

impl DataGrid {
    /// Put the keyboard focus on the grid, the way clicking a cell does.
    pub(crate) fn focus_for_test(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_table(window, cx);
    }

    /// Whether the keyboard is on the grid itself rather than a cell editor.
    pub(crate) fn is_focused_for_test(&self, window: &Window, cx: &App) -> bool {
        self.table.read(cx).focus_handle(cx).is_focused(window)
    }

    pub(crate) fn editing_for_test(&self, cx: &App) -> Option<(usize, usize)> {
        self.table.read(cx).delegate().editing
    }

    pub(crate) fn editable_for_test(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().editable
    }

    /// Open the cell editor the way double-clicking a cell does.
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

    /// Copy one cell, the way "Copy value" in the row menu does.
    pub(crate) fn copy_cell_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<Self>,
    ) {
        self.copy(Some((row_ix, col_ix)), cx);
    }

    /// The value a cell shows, staged edit included.
    pub(crate) fn cell_for_test(&self, row_ix: usize, col_ix: usize, cx: &App) -> Cell {
        self.table.read(cx).delegate().cell(row_ix, col_ix).clone()
    }

    /// Ask to follow a cell's foreign key, the way "Go to referenced row" in
    /// its menu does.
    pub(crate) fn navigate_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<Self>,
    ) {
        cx.emit(GridNavigate {
            row: row_ix,
            col: col_ix,
        });
    }

    /// Whether the row menu would offer a foreign key jump on this column.
    pub(crate) fn is_foreign_key_for_test(&self, col_ix: usize, cx: &App) -> bool {
        self.table
            .read(cx)
            .delegate()
            .foreign_keys
            .contains(&col_ix)
    }

    /// Drop a row being built, the way its menu item does.
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
    pub(crate) fn row_count_for_test(&self, cx: &App) -> usize {
        let delegate = self.table.read(cx).delegate();
        delegate.order.len() + delegate.drafts.len()
    }

    /// The rows a sweep has picked out, in display order.
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
    pub(crate) fn selection_for_test(&self, cx: &App) -> (Option<usize>, Option<(usize, usize)>) {
        (
            self.table.read(cx).delegate().focused_row,
            self.selected_cell(cx),
        )
    }

    /// Sort by a column the way clicking its header does.
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
    pub(crate) fn sorted_for_test(&self, cx: &App) -> Option<(String, ColumnSort)> {
        self.table.read(cx).delegate().sorted_by.clone()
    }

    /// One column of every row, in display order.
    pub(crate) fn column_values_for_test(&self, col_ix: usize, cx: &App) -> Vec<Option<String>> {
        let delegate = self.table.read(cx).delegate();
        (0..delegate.order.len())
            .map(|row_ix| delegate.cell(row_ix, col_ix).clone())
            .collect()
    }

    pub(crate) fn column_widths_for_test(&self, cx: &App) -> Vec<gpui_kit::Pixels> {
        self.table.read(cx).delegate().widths.clone()
    }
}
