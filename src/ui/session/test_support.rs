//! Reaching into a session from a test.
//!
//! These stand in for the parts of a session a test cannot drive through the
//! window — a live server, a file dialog — and read back what the user would
//! see. Kept beside the session rather than in it so the view itself reads as
//! what the app does.

use gpui_kit::component::dock::DockArea;
use gpui_kit::{App, Context, Entity, SharedString, Window};

use crate::db::DatabaseObject;
use crate::ui::console::ConsoleView;
use crate::ui::create_database_dialog::CreateDatabaseView;
use crate::ui::data_grid::DataGrid;
use crate::ui::erd::ErdView;
use crate::ui::import_dialog::ImportView;
use crate::ui::plan_view::PlanView;
use crate::ui::process_list::ProcessListView;
use crate::ui::query_digest::QueryDigestView;
use crate::ui::query_editor::QueryEditor;
use crate::ui::schema_view::SchemaView;
use crate::ui::server_variables::ServerVariablesView;
use crate::ui::sqlite_maintenance::SqliteMaintenanceView;
use crate::ui::table_view::TableView;
use crate::ui::user_management::UserManagementView;

use super::Session;
use super::panel::{CloseScope, SessionPanel, SessionPanelEvent};
use super::sidebar::compile_filter;
use super::tab::Status;
use super::tool::ToolView;

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

    /// What the title bar's database picker shows as chosen, and the list it
    /// offers.
    pub(crate) fn database_picker_for_test(&self, cx: &App) -> (Option<String>, usize) {
        let state = self.database_select.read(cx);
        (
            state.selected_value().map(|value| value.to_string()),
            self.database_select_items.len(),
        )
    }

    /// Pick which result the active query tab shows, as its result bar's
    /// segments do.
    pub(crate) fn pick_result_for_test(&mut self, which: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.show_result(which, cx));
        }
    }

    /// Switch the active query tab between its result and its plan, as the
    /// Results/Plan segments do.
    pub(crate) fn pick_plan_for_test(&mut self, plan: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.set_show_plan(plan, cx));
        }
    }

    #[cfg(test)]
    pub(crate) fn tab_titles(&self, cx: &App) -> Vec<String> {
        self.panels
            .iter()
            .map(|panel| panel.read(cx).title().to_string())
            .collect()
    }

    /// The kind icon of every tab, in creation order. A data tab and the
    /// structure tab of the same table share a title, so this is what tells
    /// them apart on screen.
    #[cfg(test)]
    pub(crate) fn tab_icons(&self, cx: &App) -> Vec<gpui_kit::assets::IconName> {
        self.panels
            .iter()
            .map(|panel| panel.read(cx).icon())
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

    /// The tool view in the active tab, if this is a tool tab.
    fn active_tool_view<'a>(&self, cx: &'a App) -> Option<&'a ToolView> {
        self.active_panel_for_test()?.read(cx).tool_view()
    }

    /// The view in the active tab, if this is the console tab.
    pub(crate) fn active_console_view(&self, cx: &App) -> Option<Entity<ConsoleView>> {
        match self.active_tool_view(cx)? {
            ToolView::Console(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the process list tab.
    pub(crate) fn active_process_list_view(&self, cx: &App) -> Option<Entity<ProcessListView>> {
        match self.active_tool_view(cx)? {
            ToolView::Processes(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the user management tab.
    pub(crate) fn active_user_management_view(
        &self,
        cx: &App,
    ) -> Option<Entity<UserManagementView>> {
        match self.active_tool_view(cx)? {
            ToolView::Users(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the ER diagram tab.
    pub(crate) fn active_erd_view(&self, cx: &App) -> Option<Entity<ErdView>> {
        match self.active_tool_view(cx)? {
            ToolView::Erd(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the server variables tab.
    pub(crate) fn active_server_variables_view(
        &self,
        cx: &App,
    ) -> Option<Entity<ServerVariablesView>> {
        match self.active_tool_view(cx)? {
            ToolView::Variables(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the query digest tab.
    pub(crate) fn active_query_digest_view(&self, cx: &App) -> Option<Entity<QueryDigestView>> {
        match self.active_tool_view(cx)? {
            ToolView::Digest(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// The view in the active tab, if this is the maintenance tab.
    pub(crate) fn active_maintenance_view(
        &self,
        cx: &App,
    ) -> Option<Entity<SqliteMaintenanceView>> {
        match self.active_tool_view(cx)? {
            ToolView::Maintenance(view) => Some(view.clone()),
            _ => None,
        }
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

    /// Open a new-table structure tab, the way the sidebar's New table
    /// button does.
    #[cfg(test)]
    pub(crate) fn open_new_table_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_table(window, cx);
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

    /// Drive one of a tab's menu commands the way its own item does — emit the
    /// event the item emits.
    ///
    /// The menu itself cannot be opened in a test: a `PopupMenu` keeps itself
    /// alive through the subscription its context menu registers, so a test
    /// that opens one ends in the harness's leaked-entity panic (see
    /// `ui/tests/mod.rs`).
    #[cfg(test)]
    pub(crate) fn close_scope_for_test(
        &mut self,
        index: usize,
        scope: CloseScope,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panels[index].clone();
        panel.update(cx, |_, cx| {
            cx.emit(SessionPanelEvent::CloseScopeRequested(scope))
        });
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

    /// Put the session in the sidebar's Management tab, the way clicking its
    /// top tab does.
    #[cfg(test)]
    pub(crate) fn show_sidebar_tab_for_test(&mut self, management: bool, cx: &mut Context<Self>) {
        self.sidebar_tab = if management {
            super::sidebar::SidebarTab::Management
        } else {
            super::sidebar::SidebarTab::Schema
        };
        cx.notify();
    }

    /// Put the session in the sidebar's Saved tab, the way clicking its
    /// top tab does.
    #[cfg(test)]
    pub(crate) fn show_saved_tab_for_test(&mut self, cx: &mut Context<Self>) {
        self.sidebar_tab = super::sidebar::SidebarTab::Saved;
        cx.notify();
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

    /// Point the active tab at this session's connection again, the way a
    /// database switch repoints it. The tab's stored results belong to
    /// the connection being left, so they must not survive the repoint —
    /// which connection it is pointed at does not change that.
    #[cfg(test)]
    pub(crate) fn repoint_active_tab_for_test(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.active_panel_for_test() else {
            return;
        };
        let connection = self.connection.clone();
        panel.update(cx, |panel, cx| panel.set_connection(connection, cx));
    }

    /// The plan viewer in the active tab, if one has been built.
    #[cfg(test)]
    pub(crate) fn active_plan_view_for_test(&self, cx: &App) -> Option<Entity<PlanView>> {
        self.active_panel_for_test()?.read(cx).plan_view()
    }

    /// Show the chart pane or the result grid in the active tab.
    #[cfg(test)]
    pub(crate) fn pick_chart_for_test(&mut self, show: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.set_show_chart(show, cx));
        }
    }

    /// Whether the active tab is showing its chart rather than its grid.
    #[cfg(test)]
    pub(crate) fn chart_showing_for_test(&self, cx: &App) -> bool {
        self.active_panel_for_test()
            .is_some_and(|panel| panel.read(cx).showing_chart())
    }

    /// Put finished results in the active tab, the way a run does — grid,
    /// status, and which result is showing included — rather than only
    /// handing the grid a result.
    #[cfg(test)]
    pub(crate) fn show_query_results_for_test(
        &mut self,
        results: Vec<crate::db::query::QueryResult>,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.show_results(results, cx));
        }
    }

    /// Show the messages pane or the result grid in the active tab, as its
    /// Messages segment does.
    #[cfg(test)]
    pub(crate) fn pick_messages_for_test(&mut self, show: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.set_show_messages(show, cx));
        }
    }

    /// Whether the active tab is showing its messages rather than its grid.
    #[cfg(test)]
    pub(crate) fn messages_showing_for_test(&self, cx: &App) -> bool {
        self.active_panel_for_test()
            .is_some_and(|panel| panel.read(cx).showing_messages())
    }

    /// The messages the active tab's shown result carries, as the messages
    /// pane would list them — empty when it carries none.
    #[cfg(test)]
    pub(crate) fn shown_messages_for_test(&self, cx: &App) -> Vec<String> {
        self.active_panel_for_test()
            .map(|panel| panel.read(cx).shown_messages())
            .unwrap_or_default()
    }

    /// The chart the active tab would draw, for tests.
    #[cfg(test)]
    pub(crate) fn chart_for_test(&self, cx: &App) -> Option<super::chart::ChartSummary> {
        let panel = self.active_panel_for_test()?;
        panel.read(cx).chart_summary_for_test()
    }

    /// The active tab's dashboard widgets, for tests: one summary per
    /// widget, in order, or nothing when the active tab is not a dashboard.
    #[cfg(test)]
    pub(crate) fn dashboard_for_test(
        &self,
        cx: &App,
    ) -> Option<Vec<super::dashboard::WidgetSummary>> {
        let panel = self.active_panel_for_test()?;
        panel
            .read(cx)
            .dashboard_view()
            .map(|view| view.read(cx).widget_summaries())
    }

    /// The active tab's dashboard variables dialog, when one is open.
    #[cfg(test)]
    pub(crate) fn dashboard_variables_dialog_for_test(
        &self,
        cx: &App,
    ) -> Option<Entity<crate::ui::variables_dialog::VariablesView>> {
        let panel = self.active_panel_for_test()?;
        panel
            .read(cx)
            .dashboard_view()
            .and_then(|view| view.read(cx).variables_dialog_for_test())
    }

    /// Mark the active tab's dashboard as mid-refresh, for tests: true
    /// when the active tab is a dashboard, so a test that needs the
    /// in-flight state fails loudly instead of passing vacuously.
    #[cfg(test)]
    pub(crate) fn mark_dashboard_refreshing_for_test(&self, cx: &mut App) -> bool {
        let Some(panel) = self.active_panel_for_test() else {
            return false;
        };
        let Some(view) = panel.read(cx).dashboard_view() else {
            return false;
        };
        view.update(cx, |view, _| view.mark_refreshing_for_test());
        true
    }

    /// Toggle a column in the active tab's chart y set, the way its Y
    /// picker's checkable items do.
    #[cfg(test)]
    pub(crate) fn toggle_chart_y_for_test(&mut self, column: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.toggle_chart_y(column, cx));
        }
    }

    /// Set the active tab's chart kind, the way its kind switch does.
    #[cfg(test)]
    pub(crate) fn set_chart_kind_for_test(
        &mut self,
        kind: super::chart::ChartKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel_for_test() {
            panel.update(cx, |panel, cx| panel.set_chart_kind(kind, cx));
        }
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

    /// The error the active tab's result pane is showing, if its last run
    /// failed outright.
    #[cfg(test)]
    pub(crate) fn active_pane_error_for_test(&self, cx: &App) -> Option<String> {
        self.active_panel_for_test()
            .and_then(|panel| panel.read(cx).pane_error().map(str::to_owned))
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

    /// The create-database dialog, while one is open.
    #[cfg(test)]
    pub(crate) fn create_database_dialog_for_test(&self) -> Option<Entity<CreateDatabaseView>> {
        self.create_database.clone()
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

    /// Whether the active tab has a transaction open on its connection.
    #[cfg(test)]
    pub(crate) fn active_transaction_for_test(&self, cx: &App) -> crate::db::TxnState {
        self.active_panel()
            .map(|panel| panel.read(cx).transaction())
            .unwrap_or_default()
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
