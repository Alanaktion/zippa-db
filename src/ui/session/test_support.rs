//! Reaching into a session from a test.
//!
//! These stand in for the parts of a session a test cannot drive through the
//! window — a live server, a file dialog — and read back what the user would
//! see. Kept beside the session rather than in it so the view itself reads as
//! what the app does.

use gpui_kit::{Context, Entity, Window};

use crate::db::DatabaseObject;
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::QueryEditor;
use crate::ui::table_view::TableView;

use super::Session;
use super::sidebar::compile_filter;
use super::tab::{Status, TabContent};

impl Session {
    /// Feed the grid a result without going through a live query.
    #[cfg(test)]
    pub(crate) fn show_result_for_test(
        &mut self,
        result: crate::db::query::QueryResult,
        cx: &mut Context<Self>,
    ) {
        let index = self.active;
        let Some(grid) = self.active_grid() else {
            return;
        };
        self.set_status(index, Status::Done(result.summary()));
        grid.update(cx, |grid, cx| grid.set_result(result, cx));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn tab_titles(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.title.to_string()).collect()
    }

    /// Whether the tab at `index` has unsaved changes.
    #[cfg(test)]
    pub(crate) fn tab_is_dirty_for_test(&self, index: usize, cx: &gpui_kit::App) -> bool {
        self.tabs[index].is_dirty(cx)
    }

    /// The tab waiting on an answer about its unsaved changes, if any.
    #[cfg(test)]
    pub(crate) fn closing_for_test(&self) -> Option<usize> {
        self.closing
    }

    #[cfg(test)]
    pub(crate) fn active_sql(&self, cx: &gpui_kit::App) -> String {
        match &self.tabs[self.active].content {
            TabContent::Query { editor, .. } => editor.read(cx).sql(cx),
            TabContent::Table { view } => view.read(cx).query(cx),
        }
    }

    /// The table view in the active tab, if this is a table tab.
    #[cfg(test)]
    pub(crate) fn active_table_view(&self) -> Option<Entity<TableView>> {
        match &self.tabs[self.active].content {
            TabContent::Table { view } => Some(view.clone()),
            TabContent::Query { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn open_tab_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(None, String::new(), false, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn close_tab_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab(index, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn activate_tab_for_test(&mut self, index: usize, cx: &mut Context<Self>) {
        self.activate_tab(index, cx);
    }

    /// Set the filter text the way typing in the box does.
    #[cfg(test)]
    pub(crate) fn set_filter_for_test(
        &mut self,
        pattern: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filter = self.filter.clone();
        filter.update(cx, |state, cx| {
            state.set_value(pattern.to_string(), window, cx)
        });
        self.matcher = compile_filter(pattern.trim());
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn visible_object_labels(&self) -> Vec<String> {
        self.visible_objects()
            .into_iter()
            .map(|object| object.label())
            .collect()
    }

    /// Type into the active tab's editor and put focus there.
    #[cfg(test)]
    pub(crate) fn prepare_active_editor_for_test(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TabContent::Query { editor, .. } = &self.tabs[self.active].content else {
            return;
        };
        let editor = editor.clone();
        editor.update(cx, |editor, cx| {
            editor.set_sql(sql, window, cx);
            editor.focus(window, cx);
        });
    }

    /// The editor of the active tab, for a test to drive.
    #[cfg(test)]
    pub(crate) fn active_editor_for_test(&self) -> Option<Entity<QueryEditor>> {
        match &self.tabs[self.active].content {
            TabContent::Query { editor, .. } => Some(editor.clone()),
            TabContent::Table { .. } => None,
        }
    }

    /// How many results the last run left, and which one is showing.
    #[cfg(test)]
    pub(crate) fn results_for_test(&self) -> (usize, usize) {
        match &self.tabs[self.active].content {
            TabContent::Query {
                results, result, ..
            } => (results.len(), *result),
            TabContent::Table { .. } => (0, 0),
        }
    }

    /// Put the active tab into its running state, the way a query in flight
    /// does. Under test a query finishes before anything can be cancelled, so
    /// this is what the cancel path is driven with.
    #[cfg(test)]
    pub(crate) fn mark_running_for_test(&mut self, cx: &mut Context<Self>) {
        let index = self.active;
        let Some(TabContent::Query { editor, .. }) = self.tabs.get(index).map(|tab| &tab.content)
        else {
            return;
        };

        let editor = editor.clone();
        self.set_status(index, Status::Running);
        editor.update(cx, |editor, cx| editor.set_running(true, cx));
        cx.notify();
    }

    /// Reach the active tab's grid from a test.
    #[cfg(test)]
    pub(crate) fn active_grid(&self) -> Option<Entity<DataGrid>> {
        match &self.tabs[self.active].content {
            TabContent::Query { grid, .. } => Some(grid.clone()),
            TabContent::Table { .. } => None,
        }
    }

    /// Whether the active tab has a query in flight.
    #[cfg(test)]
    pub(crate) fn active_is_running(&self) -> bool {
        matches!(
            &self.tabs[self.active].content,
            TabContent::Query {
                status: Status::Running,
                ..
            }
        )
    }

    /// What the status bar says for the active tab, as plain text.
    #[cfg(test)]
    pub(crate) fn active_status_for_test(&self) -> String {
        match &self.tabs[self.active].content {
            TabContent::Query { status, .. } => status.message(),
            TabContent::Table { .. } => String::new(),
        }
    }

    /// Fill the sidebar without waiting on the server.
    #[cfg(test)]
    pub(crate) fn set_metadata_for_test(
        &mut self,
        databases: Vec<String>,
        objects: Vec<DatabaseObject>,
        cx: &mut Context<Self>,
    ) {
        self.databases = databases;
        self.objects = objects;
        self.metadata_error = None;
        cx.notify();
    }
}
