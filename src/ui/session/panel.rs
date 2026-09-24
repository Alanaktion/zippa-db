//! `SessionPanel`: one dock panel wrapping a query editor+grid, a table view,
//! a structure view, or the console — the same [`TabContent`] the session
//! used to hold in a plain `Vec`, now a dock-managed panel with its own
//! title, toolbar and closing behavior.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent, PanelId, TabGroup};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, ResizableState, Sizable, h_flex, resizable_panel, v_flex,
    v_resizable,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    SharedString, WeakEntity, Window, div, px,
};

use crate::db::Connection;
use crate::db::DatabaseObject;
use crate::db::Engine;
use crate::db::Plan;
use crate::db::query::QueryResult;
use crate::ui::console::ConsoleView;
use crate::ui::data_grid::{Copied, DataGrid};
use crate::ui::plan_view::PlanView;
use crate::ui::process_list::ProcessListView;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::schema_view::SchemaView;
use crate::ui::server_variables::ServerVariablesView;
use crate::ui::sql_file;
use crate::ui::table_view::TableView;
use crate::workspace_state::PanelState;

use super::tab::{ObjectViewMode, Status, TabContent};
use super::{CloseTab, NewTab};

/// Starting height of the editor pane above the grid; the user drags from
/// here. Per-panel now: a split puts two query panels on screen at once, and
/// a single shared height would fight between them.
const EDITOR_HEIGHT: f32 = 220.;

/// Which tabs a close command takes along with the tab it was opened from.
///
/// The two "beside it" commands work along the strip the tab is shown in — a
/// session split into two groups has two strips, and a menu opened in one is
/// about that one. The two category commands clear the session instead: a
/// table tab is usually opened on the way somewhere rather than kept, so
/// "close the tables, keep my queries" is a command about the connection
/// rather than about one strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseScope {
    /// Every other tab in the same strip.
    Others,
    /// Every tab after this one in the same strip.
    ToTheRight,
    /// Every query tab in the session.
    QueryTabs,
    /// Every table tab in the session, its rows and its structure alike.
    TableTabs,
}

pub(crate) enum SessionPanelEvent {
    /// The user is working in this panel now.
    Focused,
    /// The panel left the dock, however it left.
    Removed,
    /// The tab's ✕, or a middle click on it.
    CloseRequested,
    /// A tab command that closes more than the tab it was opened from.
    CloseScopeRequested(CloseScope),
    /// The strip's + — a query tab beside this one.
    NewTabRequested,
    Run(String),
    RunScript(String),
    /// The user asked for a statement's plan.
    Explain {
        sql: String,
        analyze: bool,
    },
    OpenFile,
    Save,
}

pub(crate) struct SessionPanel {
    /// Stable identity for element ids and for tests: assigned in creation
    /// order and never reused, so a close button keeps its id when the user
    /// drags the tabs about.
    key: usize,
    title: SharedString,
    content: TabContent,
    focus: FocusHandle,
    /// The tab group displaying this panel, for bringing it forward. `None`
    /// until the dock has taken it.
    group: Option<WeakEntity<TabGroup>>,
    /// Editor above grid. One state per panel, not one per session: a split
    /// puts two query panels on screen at once, and a single
    /// `ResizableState` cannot drive two elements.
    panes: Entity<ResizableState>,
}

impl EventEmitter<SessionPanelEvent> for SessionPanel {}
impl EventEmitter<PanelEvent> for SessionPanel {}

impl SessionPanel {
    /// A query tab, already holding `sql`.
    pub(crate) fn query(
        key: usize,
        title: impl Into<SharedString>,
        sql: String,
        engine: Engine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| QueryEditor::with_text(sql.clone(), engine, window, cx));
        cx.subscribe_in(&editor, window, Self::on_editor_event)
            .detach();

        let grid = cx.new(|cx| DataGrid::new(engine, window, cx));
        cx.subscribe_in(&grid, window, Self::on_grid_copied)
            .detach();

        Self {
            key,
            title: title.into(),
            content: TabContent::Query {
                editor,
                grid,
                status: Status::Idle,
                path: None,
                results: Vec::new(),
                result: 0,
                running: None,
                baseline: sql,
                plan: None,
                show_plan: false,
            },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    /// A table tab over an already-built [`TableView`].
    pub(crate) fn table(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<TableView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Table { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    /// A structure tab over an already-built [`SchemaView`].
    pub(crate) fn schema(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<SchemaView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Schema { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    /// The console tab over an already-built [`ConsoleView`]: one per
    /// session, the same as any other singleton tab, brought forward rather
    /// than duplicated.
    pub(crate) fn console(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<ConsoleView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Console { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    /// The process list tab over an already-built [`ProcessListView`]: one
    /// per session, the same as the console.
    pub(crate) fn processes(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<ProcessListView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Processes { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    /// The server variables tab over an already-built [`ServerVariablesView`]:
    /// one per session, the same as the console and the process list.
    pub(crate) fn variables(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<ServerVariablesView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Variables { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
        }
    }

    fn on_editor_event(
        &mut self,
        _: &Entity<QueryEditor>,
        event: &QueryEditorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QueryEditorEvent::Run(sql) => cx.emit(SessionPanelEvent::Run(sql.clone())),
            QueryEditorEvent::RunScript(sql) => cx.emit(SessionPanelEvent::RunScript(sql.clone())),
            QueryEditorEvent::Explain { sql, analyze } => cx.emit(SessionPanelEvent::Explain {
                sql: sql.clone(),
                analyze: *analyze,
            }),
            QueryEditorEvent::Open => cx.emit(SessionPanelEvent::OpenFile),
            QueryEditorEvent::Save => cx.emit(SessionPanelEvent::Save),
        }
    }

    /// A copy went to the clipboard; the status bar says what it was. This is
    /// the query tab's counterpart to the table view's footer notice.
    fn on_grid_copied(
        &mut self,
        _: &Entity<DataGrid>,
        event: &Copied,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_status(Status::Done(event.message.clone()));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn key(&self) -> usize {
        self.key
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub(crate) fn is_query(&self) -> bool {
        matches!(self.content, TabContent::Query { .. })
    }

    pub(crate) fn is_console(&self) -> bool {
        matches!(self.content, TabContent::Console { .. })
    }

    pub(crate) fn is_process_list(&self) -> bool {
        matches!(self.content, TabContent::Processes { .. })
    }

    pub(crate) fn is_server_variables(&self) -> bool {
        matches!(self.content, TabContent::Variables { .. })
    }

    /// The icon that tells this tab's kind apart at a glance.
    ///
    /// A table's data tab and its structure tab carry the same name, so the
    /// icon is what tells them apart; the query icon is the one the quick
    /// switcher already uses for an open query tab.
    pub(crate) fn icon(&self) -> IconName {
        match &self.content {
            TabContent::Query { .. } => IconName::SquareTerminal,
            TabContent::Table { .. } => IconName::Table,
            TabContent::Schema { .. } => IconName::ListTree,
            TabContent::Console { .. } => IconName::ScrollText,
            TabContent::Processes { .. } => IconName::Activity,
            TabContent::Variables { .. } => IconName::SlidersHorizontal,
        }
    }

    /// What this tab shows, in words.
    ///
    /// The icon is what tells a data tab from a structure one, but an icon
    /// names nothing to a screen reader, so the close button says the kind
    /// rather than leaving two identically named tabs to be guessed at.
    pub(crate) fn kind(&self) -> &'static str {
        match &self.content {
            TabContent::Query { .. } => "query",
            TabContent::Table { .. } => "table",
            TabContent::Schema { .. } => "table structure",
            TabContent::Console { .. } => "console",
            TabContent::Processes { .. } => "process list",
            TabContent::Variables { .. } => "server variables",
        }
    }

    /// The tab group displaying this panel, if the dock has taken it yet.
    pub(crate) fn group(&self) -> Option<WeakEntity<TabGroup>> {
        self.group.clone()
    }

    /// The commands a tab offers about itself and its neighbours.
    ///
    /// Shared by the tab's own right-click menu and the dock's `…` menu: both
    /// belong to one tab — the context menu hangs off its title element, and
    /// the `…` menu belongs to the group's displayed panel — so both offer the
    /// same commands about the same tab.
    ///
    /// Nothing here is disabled against how many tabs there are: the panel is
    /// drawn *by* its tab group, so reading the group to count them — the only
    /// place that knows — is a read of the group while it is being updated,
    /// which is a panic. A command with nothing to close does nothing instead,
    /// and the session's half of it decides that against the dock at the
    /// moment it runs.
    fn tab_menu(menu: PopupMenu, panel: WeakEntity<Self>) -> PopupMenu {
        let item = |label: &'static str, event: fn() -> SessionPanelEvent| {
            let panel = panel.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                if let Some(panel) = panel.upgrade() {
                    panel.update(cx, |_, cx| cx.emit(event()));
                }
            })
        };

        menu.item(item("Close", || SessionPanelEvent::CloseRequested))
            .separator()
            .item(item("Close Others", || {
                SessionPanelEvent::CloseScopeRequested(CloseScope::Others)
            }))
            .item(item("Close to the Right", || {
                SessionPanelEvent::CloseScopeRequested(CloseScope::ToTheRight)
            }))
            .separator()
            .item(item("Close All Table Tabs", || {
                SessionPanelEvent::CloseScopeRequested(CloseScope::TableTabs)
            }))
            .item(item("Close All Query Tabs", || {
                SessionPanelEvent::CloseScopeRequested(CloseScope::QueryTabs)
            }))
    }

    /// The table this tab shows, if it is a table or structure tab.
    pub(crate) fn object(&self, cx: &App) -> Option<DatabaseObject> {
        match &self.content {
            TabContent::Table { view } => Some(view.read(cx).object().clone()),
            TabContent::Schema { view } => Some(view.read(cx).object().clone()),
            TabContent::Query { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    /// Which view mode this tab is showing its object in, if it is showing one.
    pub(crate) fn mode(&self) -> Option<ObjectViewMode> {
        match &self.content {
            TabContent::Table { .. } => Some(ObjectViewMode::Data),
            TabContent::Schema { .. } => Some(ObjectViewMode::Schema),
            TabContent::Query { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    /// This tab as it would be restored: a query keeps its title, its buffer,
    /// and its file; a table or structure tab keeps the object it shows.
    pub(crate) fn snapshot(&self, cx: &App) -> PanelState {
        match &self.content {
            TabContent::Query { editor, path, .. } => PanelState::Query {
                title: self.title.to_string(),
                sql: editor.read(cx).sql(cx),
                file: path.clone(),
            },
            TabContent::Table { view } => PanelState::Table {
                object: view.read(cx).object().clone(),
            },
            TabContent::Schema { view } => PanelState::Schema {
                object: view.read(cx).object().clone(),
            },
            TabContent::Console { .. } => PanelState::Console,
            TabContent::Processes { .. } => PanelState::Processes,
            TabContent::Variables { .. } => PanelState::Variables,
        }
    }

    /// Whether a query tab's buffer has changed since it was opened or last
    /// saved. A table tab counts the edits staged in its grid; a structure tab
    /// its unapplied column edits.
    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        match &self.content {
            TabContent::Query {
                editor, baseline, ..
            } => &editor.read(cx).sql(cx) != baseline,
            TabContent::Table { view } => view.read(cx).has_staged_edits(cx),
            TabContent::Schema { view } => view.read(cx).is_dirty(cx),
            TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => false,
        }
    }

    pub(crate) fn table_view(&self) -> Option<Entity<TableView>> {
        match &self.content {
            TabContent::Table { view } => Some(view.clone()),
            TabContent::Query { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn schema_view(&self) -> Option<Entity<SchemaView>> {
        match &self.content {
            TabContent::Schema { view } => Some(view.clone()),
            TabContent::Query { .. }
            | TabContent::Table { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn console_view(&self) -> Option<Entity<ConsoleView>> {
        match &self.content {
            TabContent::Console { view } => Some(view.clone()),
            TabContent::Query { .. }
            | TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn process_list_view(&self) -> Option<Entity<ProcessListView>> {
        match &self.content {
            TabContent::Processes { view } => Some(view.clone()),
            TabContent::Query { .. }
            | TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn server_variables_view(&self) -> Option<Entity<ServerVariablesView>> {
        match &self.content {
            TabContent::Variables { view } => Some(view.clone()),
            TabContent::Query { .. }
            | TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. } => None,
        }
    }

    pub(crate) fn editor(&self) -> Option<Entity<QueryEditor>> {
        match &self.content {
            TabContent::Query { editor, .. } => Some(editor.clone()),
            TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn grid(&self) -> Option<Entity<DataGrid>> {
        match &self.content {
            TabContent::Query { grid, .. } => Some(grid.clone()),
            TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => None,
        }
    }

    /// The editor and grid entities a run acts on.
    pub(crate) fn query_parts(&self) -> Option<(Entity<QueryEditor>, Entity<DataGrid>)> {
        match &self.content {
            TabContent::Query { editor, grid, .. } => Some((editor.clone(), grid.clone())),
            _ => None,
        }
    }

    /// The plan viewer this tab has, once it has read a plan.
    pub(crate) fn plan_view(&self) -> Option<Entity<PlanView>> {
        match &self.content {
            TabContent::Query { plan, .. } => plan.clone(),
            _ => None,
        }
    }

    /// Whether the pane is showing the plan rather than the result grid.
    pub(crate) fn showing_plan(&self) -> bool {
        matches!(
            &self.content,
            TabContent::Query {
                show_plan: true,
                ..
            }
        )
    }

    /// Show the plan pane or the result grid.
    pub(crate) fn set_show_plan(&mut self, show: bool, cx: &mut Context<Self>) {
        if let TabContent::Query { show_plan, .. } = &mut self.content {
            *show_plan = show;
            cx.notify();
        }
    }

    /// Put `plan` in this tab's viewer and show it, building the viewer the
    /// first time one is needed.
    pub(crate) fn set_plan(&mut self, plan: Plan, cx: &mut Context<Self>) {
        let summary = plan_summary(&plan);
        let TabContent::Query {
            plan: slot,
            show_plan,
            status,
            ..
        } = &mut self.content
        else {
            return;
        };

        let view = match slot {
            Some(view) => view.clone(),
            None => {
                let view = cx.new(PlanView::new);
                *slot = Some(view.clone());
                view
            }
        };
        view.update(cx, |view, cx| view.set_plan(plan, cx));
        *show_plan = true;
        *status = Status::Done(summary);
        cx.notify();
    }

    /// The plan viewer while it is the pane being shown, so whoever hands the
    /// tab its keyboard can put it on the plan instead of the editor.
    pub(crate) fn active_plan(&self) -> Option<Entity<PlanView>> {
        if !self.showing_plan() {
            return None;
        }
        self.plan_view()
    }

    /// The file a query tab is bound to, if any.
    pub(crate) fn file_path(&self) -> Option<PathBuf> {
        match &self.content {
            TabContent::Query { path, .. } => path.clone(),
            _ => None,
        }
    }

    pub(crate) fn file_name(&self) -> Option<String> {
        self.file_path()
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    #[cfg(test)]
    pub(crate) fn status_message(&self) -> String {
        match &self.content {
            TabContent::Query { status, .. } => status.message(),
            TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => String::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            &self.content,
            TabContent::Query {
                status: Status::Running,
                ..
            }
        )
    }

    /// How many results the last run left, and which one is showing.
    #[cfg(test)]
    pub(crate) fn results(&self) -> (usize, usize) {
        match &self.content {
            TabContent::Query {
                results, result, ..
            } => (results.len(), *result),
            TabContent::Table { .. }
            | TabContent::Schema { .. }
            | TabContent::Console { .. }
            | TabContent::Processes { .. }
            | TabContent::Variables { .. } => (0, 0),
        }
    }

    pub(crate) fn set_status(&mut self, status: Status) {
        if let TabContent::Query { status: slot, .. } = &mut self.content {
            *slot = status;
        }
    }

    pub(crate) fn set_running(&mut self, handle: Option<tokio::task::AbortHandle>) {
        if let TabContent::Query { running, .. } = &mut self.content {
            *running = handle;
        }
    }

    /// Put a finished run's results in this tab.
    pub(crate) fn show_results(&mut self, results: Vec<QueryResult>, cx: &mut Context<Self>) {
        let TabContent::Query {
            grid,
            results: slot,
            result,
            show_plan,
            ..
        } = &mut self.content
        else {
            return;
        };

        let grid = grid.clone();
        *slot = results;
        *result = 0;
        // A new run is what the user asked to see, so the grid comes back even
        // if a plan was showing.
        *show_plan = false;

        let summary = self.result_summary();
        self.set_status(Status::Done(summary));

        // A statement that returned no rows at all — an `update`, say —
        // leaves the grid empty rather than showing the rows of the run
        // before it.
        let first = match &self.content {
            TabContent::Query { results, .. } => results.first().cloned(),
            _ => None,
        };
        match first {
            Some(result) => grid.update(cx, |grid, cx| grid.set_result(result, cx)),
            None => grid.update(cx, |grid, cx| grid.clear(cx)),
        }
        cx.notify();
    }

    /// Show another of the results the last run produced.
    pub(crate) fn show_result(&mut self, which: usize, cx: &mut Context<Self>) {
        let TabContent::Query {
            grid,
            results,
            result,
            ..
        } = &mut self.content
        else {
            return;
        };

        let Some(chosen) = results.get(which).cloned() else {
            return;
        };
        let grid = grid.clone();
        *result = which;

        grid.update(cx, |grid, cx| grid.set_result(chosen, cx));
        let summary = self.result_summary();
        self.set_status(Status::Done(summary));
        cx.notify();
    }

    /// What the status bar says about the run that just finished.
    fn result_summary(&self) -> String {
        let TabContent::Query {
            results, result, ..
        } = &self.content
        else {
            return String::new();
        };

        let Some(shown) = results.get(*result) else {
            return "Nothing to show".to_string();
        };

        if results.len() == 1 {
            return shown.summary();
        }

        let total: u128 = results
            .iter()
            .map(|result| result.elapsed.as_millis())
            .sum();
        format!(
            "{} statements in {total} ms · result {} of {}: {}",
            results.len(),
            result + 1,
            results.len(),
            shown.summary()
        )
    }

    /// Bind this query tab to `path`, naming the tab after the file.
    pub(crate) fn set_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let TabContent::Query { path: slot, .. } = &mut self.content else {
            return;
        };
        self.title = sql_file::label(&path).into();
        *slot = Some(path);
        cx.notify();
    }

    /// Mark `sql` as this query tab's clean state, as a save does.
    pub(crate) fn set_baseline(&mut self, sql: String) {
        if let TabContent::Query { baseline, .. } = &mut self.content {
            *baseline = sql;
        }
    }

    /// Give up on the run in flight, if there is one. Answers whether there
    /// was something to cancel.
    pub(crate) fn cancel_running(&mut self, cx: &mut Context<Self>) -> bool {
        let TabContent::Query {
            editor,
            running,
            status,
            ..
        } = &mut self.content
        else {
            return false;
        };

        if !matches!(status, Status::Running) {
            return false;
        }

        let editor = editor.clone();
        if let Some(running) = running.take() {
            running.abort();
        }

        *status = Status::Done("Cancelled".into());
        editor.update(cx, |editor, cx| editor.set_running(false, cx));
        cx.notify();
        true
    }

    /// Give up on the run in flight without touching the editor or status —
    /// the panel is on its way out of the dock either way.
    fn abort_running(&mut self) {
        if let TabContent::Query { running, .. } = &mut self.content
            && let Some(running) = running.take()
        {
            running.abort();
        }
    }

    /// Point this tab at `connection`: a query tab clears its grid, a table
    /// or structure tab re-reads from the new connection.
    pub(crate) fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        match &mut self.content {
            TabContent::Query {
                grid,
                plan,
                show_plan,
                ..
            } => {
                let grid = grid.clone();
                grid.update(cx, |grid, cx| grid.clear(cx));
                // A plan belongs to the connection it was read from.
                *plan = None;
                *show_plan = false;
                self.set_status(Status::Idle);
            }
            TabContent::Table { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
            TabContent::Schema { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
            TabContent::Console { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
            TabContent::Processes { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
            TabContent::Variables { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
        }
    }

    /// Reread a table tab's rows, a console tab's log, a process list, or the
    /// server variables. A query tab's buffer is the user's own SQL and is
    /// left alone; a structure tab has nothing to refresh this way.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        match &self.content {
            TabContent::Table { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.refresh(cx));
            }
            TabContent::Console { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.refresh(cx));
            }
            TabContent::Processes { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.refresh(cx));
            }
            TabContent::Variables { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.refresh(cx));
            }
            TabContent::Query { .. } | TabContent::Schema { .. } => {}
        }
    }

    /// Write a table tab's staged edits. `Cmd+S` on a query tab saves its
    /// buffer instead, handled by the session directly.
    pub(crate) fn commit(&mut self, cx: &mut Context<Self>) {
        if let TabContent::Table { view } = &self.content {
            let view = view.clone();
            view.update(cx, |view, cx| view.commit(cx));
        }
    }

    /// Bring `panel` forward in whatever group holds it, and put focus in it.
    ///
    /// A free function rather than a method taken through `panel.update(...)`:
    /// `TabGroup::select_tab` synchronously reads the group's panels back
    /// (`focus_active_panel` asks the displayed one whether it is still
    /// visible), and reading a panel entity while it is mid-`update` is a
    /// hard panic. Going through `panel.read(cx)` for everything here means
    /// the entity is never held exclusively while the dock reads it back.
    pub(crate) fn bring_forward(panel: &Entity<SessionPanel>, window: &mut Window, cx: &mut App) {
        let id = PanelId::from(panel.entity_id());
        if let Some(group) = panel.read(cx).group.as_ref().and_then(|g| g.upgrade()) {
            group.update(cx, |group, cx| {
                if let Some(ix) = group.panels().iter().position(|p| p.panel_id(cx) == id) {
                    group.select_tab(ix, window, cx);
                }
            });
        }
        // Covers the already-displayed case, where `select_tab` returns
        // early and never reaches `focus_active_panel`. The plan, when it is
        // what the tab shows, is where its keys are bound.
        match panel.read(cx).active_plan() {
            Some(plan) => plan.update(cx, |plan, cx| plan.focus(window, cx)),
            None => {
                let handle = panel.read(cx).focus_handle(cx);
                handle.focus(window, cx);
            }
        }
    }

    fn render_status_bar(&self, status: &Status, cx: &mut Context<Self>) -> impl IntoElement {
        let color = match status {
            Status::Error(_) => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        let message = status.message();

        h_flex()
            .w_full()
            .px_3()
            .py_1()
            .flex_none()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            // Errors can be long; cap the bar so it never eats the grid.
            .child(
                div()
                    .id("status")
                    .w_full()
                    .max_h(px(72.))
                    .overflow_y_scroll()
                    .text_xs()
                    .text_color(color)
                    .child(message),
            )
    }

    /// The bar above the result area: one button per result the last run
    /// produced, and the Results/Plan switch once a plan has been read.
    fn render_view_bar(
        &self,
        results: usize,
        shown: usize,
        has_plan: bool,
        show_plan: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .children((0..results).map(|index| {
                let button = Button::new(SharedString::from(format!("result-{index}")))
                    .xsmall()
                    .label(format!("Result {}", index + 1))
                    .disabled(show_plan)
                    .on_click(cx.listener(move |this, _, _window, cx| this.show_result(index, cx)));

                if index == shown && !show_plan {
                    button.primary()
                } else {
                    button.ghost()
                }
            }))
            .child(div().flex_1())
            .when(has_plan, |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .child(
                            Button::new("show-results")
                                .xsmall()
                                .label("Results")
                                .when(show_plan, |this| this.ghost())
                                .when(!show_plan, |this| this.primary())
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.set_show_plan(false, cx)
                                })),
                        )
                        .child(
                            Button::new("show-plan")
                                .xsmall()
                                .label("Plan")
                                .when(show_plan, |this| this.primary())
                                .when(!show_plan, |this| this.ghost())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_show_plan(true, cx);
                                    if let Some(plan) = this.plan_view() {
                                        plan.update(cx, |plan, cx| plan.focus(window, cx));
                                    }
                                })),
                        ),
                )
            })
    }
}

/// The status bar's line after a plan comes back.
fn plan_summary(plan: &Plan) -> String {
    let milliseconds = plan.elapsed.as_millis();
    let analyzed = if plan.analyzed { " · analyzed" } else { "" };
    format!("Plan for 1 statement in {milliseconds} ms{analyzed}")
}

impl Focusable for SessionPanel {
    /// A query panel answers with its editor, so clicking its tab puts the
    /// caret where the user is about to type; the dock focuses this handle
    /// itself whenever it displays the panel.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            TabContent::Query { editor, .. } => editor.read(cx).focus_handle(cx),
            // A table panel answers with its grid, for the same reason: the
            // rows are what the keyboard is for, and the panel's own element
            // is above the contexts the grid's keys are bound on, so focus
            // left there would answer none of them.
            TabContent::Table { view } => view.read(cx).focus_handle(cx),
            TabContent::Schema { .. } => self.focus.clone(),
            // The rows are what the keyboard is for here too, the same
            // reason a table panel answers with its grid.
            TabContent::Console { view } => view.read(cx).focus_handle(cx),
            TabContent::Processes { view } => view.read(cx).focus_handle(cx),
            TabContent::Variables { view } => view.read(cx).focus_handle(cx),
        }
    }
}

impl BasePanel for SessionPanel {
    fn panel_name(&self) -> &'static str {
        "SessionPanel"
    }

    /// Permission for the dock's own Close, which never asks the session
    /// first; the ✕ goes through `DockArea::remove_panel`, which ignores
    /// this.
    fn closable(&self, cx: &App) -> bool {
        !self.is_dirty(cx)
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(SessionPanelEvent::Focused);
        }
    }

    fn on_added_to(
        &mut self,
        group: WeakEntity<TabGroup>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.group = Some(group);
    }

    /// A tab that leaves takes its running query with it.
    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.abort_running();
        cx.emit(SessionPanelEvent::Removed);
    }
}

impl Panel for SessionPanel {
    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = self.is_dirty(cx);
        let label = if dirty {
            format!("\u{25cf} {}", self.title)
        } else {
            self.title.to_string()
        };
        let key = self.key;
        let title = self.title.clone();
        let kind = self.kind();
        let icon = self.icon();

        h_flex()
            .id(("session-panel-title", key))
            .test_support()
            .gap_1()
            .items_center()
            // Middle-click closes, the way it does in a browser. Capture
            // phase for the same reason the grid records its right-clicked
            // row that way: the ✕ button below would otherwise swallow the
            // press first.
            .capture_any_mouse_down(cx.listener(|_this, event: &MouseDownEvent, _window, cx| {
                if event.button == MouseButton::Middle {
                    cx.emit(SessionPanelEvent::CloseRequested);
                }
            }))
            // The title is the only part of a dock tab this app draws, so the
            // tab's right-click menu hangs off it. The same commands sit in
            // the `…` menu, which is reachable without a right click.
            .context_menu({
                let me = cx.weak_entity();
                move |menu, _, _| SessionPanel::tab_menu(menu, me.clone())
            })
            .child(
                Icon::new(icon)
                    .flex_none()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().child(label))
            .child(
                Button::new(SharedString::from(format!("close-tab-{key}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .accessibility_label(if dirty {
                        format!("Close the {kind} tab {title} (unsaved changes)")
                    } else {
                        format!("Close the {kind} tab {title}")
                    })
                    .tooltip_with_action("Close tab", &CloseTab, Some("Session"))
                    .on_click(cx.listener(|_this, _, _window, cx| {
                        cx.emit(SessionPanelEvent::CloseRequested);
                    })),
            )
    }

    /// The `+` beside this panel's tabs, one per group, so a new tab lands
    /// in the group the user is looking at.
    fn toolbar_buttons(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("new-tab")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .accessibility_label("New query tab")
                .tooltip_with_action("New query tab", &NewTab, Some("Session"))
                .on_click(cx.listener(|_this, _, _window, cx| {
                    cx.emit(SessionPanelEvent::NewTabRequested);
                })),
        ])
    }

    /// The `…` menu's own Close vanishes when this tab is dirty (see
    /// `closable`); say why rather than leaving it looking broken.
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let menu = if self.is_dirty(cx) {
            menu.item(PopupMenuItem::new("Close (unsaved changes)").disabled(true))
        } else {
            menu
        };

        // The `…` menu belongs to the group's displayed panel, so the tab
        // commands mean the same thing here as on the tab's own menu.
        Self::tab_menu(menu, cx.weak_entity())
    }

    /// The bodies carry their own footers and fill the pane.
    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    fn zoom_control(&self, _cx: &App) -> Option<PanelControl> {
        Some(PanelControl::Toolbar)
    }
}

impl Render for SessionPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.key;
        let body = match &self.content {
            TabContent::Query {
                editor,
                grid,
                status,
                results,
                result,
                plan,
                show_plan,
                ..
            } => {
                let has_plan = plan.is_some();
                let showing_plan = *show_plan && has_plan;
                let content: gpui_kit::AnyElement = match (showing_plan, plan) {
                    (true, Some(plan)) => plan.clone().into_any_element(),
                    _ => grid.clone().into_any_element(),
                };

                v_flex()
                    .size_full()
                    .child(
                        div().flex_1().min_h_0().child(
                            v_resizable("session-panes")
                                .with_state(&self.panes)
                                .child(
                                    resizable_panel()
                                        .size(px(EDITOR_HEIGHT))
                                        .size_range(px(120.)..px(720.))
                                        .child(editor.clone()),
                                )
                                .child(
                                    resizable_panel().child(
                                        v_flex()
                                            .size_full()
                                            // A script leaves one result per
                                            // statement, and a plan leaves the
                                            // switch; a lone result alone would
                                            // make the bar say nothing.
                                            .when(results.len() > 1 || has_plan, |this| {
                                                this.child(self.render_view_bar(
                                                    results.len(),
                                                    *result,
                                                    has_plan,
                                                    showing_plan,
                                                    cx,
                                                ))
                                            })
                                            .child(div().flex_1().min_h_0().child(content)),
                                    ),
                                ),
                        ),
                    )
                    .child(self.render_status_bar(status, cx))
                    .into_any_element()
            }
            // The table/structure view carries its own footer, so it fills
            // the pane.
            TabContent::Table { view } => view.clone().into_any_element(),
            TabContent::Schema { view } => view.clone().into_any_element(),
            TabContent::Console { view } => view.clone().into_any_element(),
            TabContent::Processes { view } => view.clone().into_any_element(),
            TabContent::Variables { view } => view.clone().into_any_element(),
        };

        div()
            .id(("session-panel", key))
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(body)
    }
}
