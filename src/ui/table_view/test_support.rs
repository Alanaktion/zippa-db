//! Test-only reach-ins, for `src/ui/tests/`.

use gpui_kit::component::table::ColumnSort;
use gpui_kit::{Context, Entity};

use crate::db::RowKey;
use crate::ui::data_grid::DataGrid;
use crate::ui::filter_bar::FilterBar;

use super::{RowPanel, TableView};

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
