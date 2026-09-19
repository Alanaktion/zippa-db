//! Reaching into a session from a test.
//!
//! These stand in for the parts of a session a test cannot drive through the
//! window — a live server, a file dialog — and read back what the user would
//! see. Kept beside the session rather than in it so the view itself reads as
//! what the app does.

use gpui_kit::component::dock::DockArea;
use gpui_kit::{App, Context, Entity, SharedString, Window};

use crate::db::DatabaseObject;
use crate::ui::data_grid::DataGrid;
use crate::ui::import_dialog::ImportView;
use crate::ui::plan_view::PlanView;
use crate::ui::query_editor::QueryEditor;
use crate::ui::schema_view::SchemaView;
use crate::ui::table_view::TableView;

use super::Session;
use super::panel::SessionPanel;
use super::sidebar::compile_filter;
use super::tab::Status;

impl Session {
    /// Feed the grid a result without going through a live query.
    #[cfg(test)]
    pub(crate) fn show_result_for_test(
        &mut self,
        result: crate::db::query::QueryResult,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.active_panel_for_test() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Done(result.summary()));
            if let Some(grid) = panel.grid() {
                grid.update(cx, |grid, cx| grid.set_result(result, cx));
            }
            cx.notify();
        });
    }

    #[cfg(test)]
    pub(crate) fn tab_titles(&self, cx: &App) -> Vec<String> {
        self.panels
            .iter()
            .map(|panel| panel.read(cx).title().to_string())
            .collect()
    }

    /// Whether the tab at `index` has unsaved changes.
    #[cfg(test)]
    pub(crate) fn tab_is_dirty_for_test(&self, index: usize, cx: &App) -> bool {
        self.panels[index].read(cx).is_dirty(cx)
    }

    #[cfg(test)]
    pub(crate) fn active_sql(&self, cx: &App) -> String {
        let Some(panel) = self.active_panel_for_test() else {
            return String::new();
        };
        let panel = panel.read(cx);
        if let Some(editor) = panel.editor() {
            return editor.read(cx).sql(cx);
        }
        if let Some(view) = panel.table_view() {
            return view.read(cx).query(cx);
        }
        String::new()
    }

    /// The table view in the active tab, if this is a table tab.
    #[cfg(test)]
    pub(crate) fn active_table_view(&self, cx: &App) -> Option<Entity<TableView>> {
        self.active_panel_for_test()?.read(cx).table_view()
    }

    /// The schema view in the active tab, if this is a structure tab.
    #[cfg(test)]
    pub(crate) fn active_schema_view(&self, cx: &App) -> Option<Entity<SchemaView>> {
        self.active_panel_for_test()?.read(cx).schema_view()
    }

    /// Open `object`'s structure tab, the way the sidebar's row menu does.
    #[cfg(test)]
    pub(crate) fn open_schema_for_test(
        &mut self,
        object: &DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_object(object, super::tab::ObjectViewMode::Schema, window, cx);
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
        let panel = self.panel_for_test(index);
        self.close_tab(&panel, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn activate_tab_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_tab(index, window, cx);
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
        let Some(editor) = self
            .active_panel_for_test()
            .and_then(|panel| panel.read(cx).editor())
        else {
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.set_sql(sql, window, cx);
            editor.focus(window, cx);
        });
    }

    /// The editor of the active tab, for a test to drive.
    #[cfg(test)]
    pub(crate) fn active_editor_for_test(&self, cx: &App) -> Option<Entity<QueryEditor>> {
        self.active_panel_for_test()?.read(cx).editor()
    }

    /// How many results the last run left, and which one is showing.
    #[cfg(test)]
    pub(crate) fn results_for_test(&self, cx: &App) -> (usize, usize) {
        self.active_panel_for_test()
            .map(|panel| panel.read(cx).results())
            .unwrap_or((0, 0))
    }

    /// Put the active tab into its running state, the way a query in flight
    /// does. Under test a query finishes before anything can be cancelled, so
    /// this is what the cancel path is driven with.
    #[cfg(test)]
    pub(crate) fn mark_running_for_test(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.active_panel_for_test() else {
            return;
        };
        let Some(editor) = panel.read(cx).editor() else {
            return;
        };
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));
    }

    /// The plan viewer in the active tab, if one has been built.
    #[cfg(test)]
    pub(crate) fn active_plan_view_for_test(&self, cx: &App) -> Option<Entity<PlanView>> {
        self.active_panel_for_test()?.read(cx).plan_view()
    }

    /// Whether the active tab is showing its plan rather than its grid.
    #[cfg(test)]
    pub(crate) fn active_plan_showing_for_test(&self, cx: &App) -> bool {
        self.active_panel_for_test()
            .is_some_and(|panel| panel.read(cx).showing_plan())
    }

    /// Reach the active tab's grid from a test.
    #[cfg(test)]
    pub(crate) fn active_grid(&self, cx: &App) -> Option<Entity<DataGrid>> {
        self.active_panel_for_test()?.read(cx).grid()
    }

    /// Whether the active tab has a query in flight.
    #[cfg(test)]
    pub(crate) fn active_is_running(&self, cx: &App) -> bool {
        self.active_panel_for_test()
            .is_some_and(|panel| panel.read(cx).is_running())
    }

    /// What the status bar says for the active tab, as plain text.
    #[cfg(test)]
    pub(crate) fn active_status_for_test(&self, cx: &App) -> String {
        self.active_panel_for_test()
            .map(|panel| panel.read(cx).status_message())
            .unwrap_or_default()
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
        self.rebuild_tree(cx);
        cx.notify();
    }

    /// The sidebar tree's highlighted object, if any.
    #[cfg(test)]
    pub(crate) fn selected_object_label_for_test(&self, cx: &App) -> Option<String> {
        self.objects_tree
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string())
    }

    /// The panel the user was last focused in, for a test to drive.
    #[cfg(test)]
    pub(crate) fn active_panel_for_test(&self) -> Option<Entity<SessionPanel>> {
        self.active_panel()
    }

    /// The panel at `index` in creation order, for a test to drive.
    #[cfg(test)]
    pub(crate) fn panel_for_test(&self, index: usize) -> Entity<SessionPanel> {
        self.panels[index].clone()
    }

    /// The import dialog, if one is open in this session.
    #[cfg(test)]
    pub(crate) fn import_view_for_test(&self) -> Option<Entity<ImportView>> {
        self.import.clone()
    }

    /// Open the import dialog for `path` without going through the file picker.
    #[cfg(test)]
    pub(crate) fn open_import_for_test(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_import_dialog(path, window, cx);
    }

    /// The id of the close button on the tab at `index`, which is keyed by
    /// the panel's stable `key` rather than its position.
    #[cfg(test)]
    pub(crate) fn close_button_id_for_test(&self, index: usize, cx: &App) -> SharedString {
        SharedString::from(format!("close-tab-{}", self.panels[index].read(cx).key()))
    }

    /// Exercise the `…`-menu close path, which never reaches
    /// [`Session::close_tab`] — it removes the panel from the dock directly.
    #[cfg(test)]
    pub(crate) fn dock_remove_for_test(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock = self.dock_for_test();
        dock.update(cx, |dock, cx| dock.remove_panel(panel.clone(), window, cx));
    }

    #[cfg(test)]
    fn dock_for_test(&self) -> Entity<DockArea> {
        self.dock.clone()
    }
}
