//! `SessionPanel`: one dock panel wrapping a query editor+grid, a table view,
//! a structure view, or the console — the same [`TabContent`] the session
//! used to hold in a plain `Vec`, now a dock-managed panel with its own
//! title, toolbar and closing behavior.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent, PanelId, TabGroup};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
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
use crate::db::export::{self, Format};
use crate::db::query::QueryResult;
use crate::db::runtime;
use crate::db::{PinnedConnection, ScriptMode, ScriptRun, TxnState};
use crate::ui::busy::busy_label;
use crate::ui::completion::SharedCatalog;
use crate::ui::data_grid::{Copied, DataGrid};
use crate::ui::plan_view::PlanView;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::schema_view::SchemaView;
use crate::ui::sql_file;
use crate::ui::table_view::TableView;
use crate::workspace_state::PanelState;

use super::chart::{ChartConfig, chartable};
use super::dashboard::DashboardView;
use super::limit::QueryLimit;
use super::tab::{ObjectViewMode, Status, TabContent};
use super::tool::{Tool, ToolView};
use super::{CloseTab, NewTab};

/// Starting height of the result pane below the editor, as a share of the
/// window's height; the SQL is the point of a query tab, so the editor takes
/// the rest and the user drags from here. Per-panel: a split puts two query
/// panels on screen at once, and a single shared height would fight between
/// them.
const RESULTS_SHARE: f32 = 0.25;

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
    RunScriptIgnoringErrors(String),
    /// The status bar's Commit (`true`) or Roll Back (`false`), for the
    /// tab's open transaction.
    EndTransaction(bool),
    /// The user asked for a statement's plan.
    Explain {
        sql: String,
        analyze: bool,
    },
    OpenFile,
    /// Write the buffer to its file; `as_new` asks for a path first.
    Save {
        as_new: bool,
    },
    /// Keep the buffer in the sidebar's Saved tab: update the saved query
    /// the tab belongs to, or — with `as_new`, or when it belongs to none —
    /// ask for a name.
    SaveQuery {
        as_new: bool,
    },
    /// The user asked to edit this tab's `:name` query variables.
    EditVariables,
}

pub(crate) struct SessionPanel {
    /// Stable identity for element ids and for tests: assigned in creation
    /// order and never reused, so a close button keeps its id when the user
    /// drags the tabs about.
    key: usize,
    title: SharedString,
    pub(super) content: TabContent,
    focus: FocusHandle,
    /// The tab group displaying this panel, for bringing it forward. `None`
    /// until the dock has taken it.
    group: Option<WeakEntity<TabGroup>>,
    /// Editor above grid. One state per panel, not one per session: a split
    /// puts two query panels on screen at once, and a single
    /// `ResizableState` cannot drive two elements.
    panes: Entity<ResizableState>,
    /// The saved query this tab's buffer belongs to — the one it was opened
    /// from in the sidebar's Saved tab, or last saved as — so saving again
    /// updates it in place instead of asking for a name.
    saved_as: Option<String>,
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
        catalog: SharedCatalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| QueryEditor::with_text(sql.clone(), engine, catalog, window, cx));
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
                error: None,
                engine,
                running: None,
                script: None,
                pinned: None,
                baseline: sql,
                plan: None,
                show_plan: false,
                show_chart: false,
                show_messages: false,
                chart: ChartConfig::default(),
                limit: QueryLimit::new(window, cx),
            },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
            saved_as: None,
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
            saved_as: None,
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
            saved_as: None,
        }
    }

    /// A tool tab over an already-built view: one per session, brought
    /// forward rather than duplicated.
    pub(crate) fn tool(key: usize, view: ToolView, cx: &mut Context<Self>) -> Self {
        Self {
            key,
            title: view.tool().title().into(),
            content: TabContent::Tool(view),
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
            saved_as: None,
        }
    }

    /// A dashboard tab over an already-built [`DashboardView`]. The view
    /// owns its dashboard snapshot; the panel only renders it.
    pub(crate) fn dashboard(
        key: usize,
        title: impl Into<SharedString>,
        view: Entity<DashboardView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            content: TabContent::Dashboard { view },
            focus: cx.focus_handle(),
            group: None,
            panes: cx.new(|_| ResizableState::default()),
            saved_as: None,
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
            QueryEditorEvent::RunScriptIgnoringErrors(sql) => {
                cx.emit(SessionPanelEvent::RunScriptIgnoringErrors(sql.clone()))
            }
            QueryEditorEvent::Explain { sql, analyze } => cx.emit(SessionPanelEvent::Explain {
                sql: sql.clone(),
                analyze: *analyze,
            }),
            QueryEditorEvent::Open => cx.emit(SessionPanelEvent::OpenFile),
            QueryEditorEvent::Save => cx.emit(SessionPanelEvent::Save { as_new: false }),
            QueryEditorEvent::SaveAs => cx.emit(SessionPanelEvent::Save { as_new: true }),
            QueryEditorEvent::SaveQuery => cx.emit(SessionPanelEvent::SaveQuery { as_new: false }),
            QueryEditorEvent::SaveQueryAs => cx.emit(SessionPanelEvent::SaveQuery { as_new: true }),
            QueryEditorEvent::EditVariables => cx.emit(SessionPanelEvent::EditVariables),
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

    /// Rename the tab, as when a new-table tab becomes its table's structure
    /// tab.
    pub(crate) fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    pub(crate) fn is_query(&self) -> bool {
        matches!(self.content, TabContent::Query { .. })
    }

    /// Which tool this tab is, if it is one.
    pub(crate) fn tool_kind(&self) -> Option<Tool> {
        match &self.content {
            TabContent::Tool(view) => Some(view.tool()),
            _ => None,
        }
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
            TabContent::Tool(view) => view.tool().icon(),
            TabContent::Dashboard { .. } => IconName::LayoutDashboard,
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
            TabContent::Tool(view) => view.tool().kind(),
            TabContent::Dashboard { .. } => "dashboard",
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
            TabContent::Query { .. } | TabContent::Tool(_) | TabContent::Dashboard { .. } => None,
        }
    }

    /// Which view mode this tab is showing its object in, if it is showing one.
    pub(crate) fn mode(&self) -> Option<ObjectViewMode> {
        match &self.content {
            TabContent::Table { .. } => Some(ObjectViewMode::Data),
            TabContent::Schema { .. } => Some(ObjectViewMode::Schema),
            TabContent::Query { .. } | TabContent::Tool(_) | TabContent::Dashboard { .. } => None,
        }
    }

    /// This tab as it would be restored: a query keeps its title, its buffer,
    /// and its file; a table or structure tab keeps the object it shows. A
    /// new-table tab has no object to restore — its half-designed form is
    /// not serialized — so it is left out, and the quit-time dirty check is
    /// what protects unapplied work instead.
    pub(crate) fn snapshot(&self, cx: &App) -> Option<PanelState> {
        let state = match &self.content {
            TabContent::Query {
                editor,
                path,
                limit,
                ..
            } => {
                let (limit_enabled, limit_rows) = limit.overrides();
                PanelState::Query {
                    title: self.title.to_string(),
                    sql: editor.read(cx).sql(cx),
                    file: path.clone(),
                    variables: editor.read(cx).variables(),
                    saved_as: self.saved_as.clone(),
                    limit_enabled,
                    limit_rows,
                }
            }
            TabContent::Table { view } => PanelState::Table {
                object: view.read(cx).object().clone(),
                filters: view.read(cx).filters(cx),
            },
            TabContent::Schema { view } if view.read(cx).is_new_table() => return None,
            TabContent::Schema { view } => PanelState::Schema {
                object: view.read(cx).object().clone(),
            },
            TabContent::Tool(view) => view.tool().state(),
            TabContent::Dashboard { view } => PanelState::Dashboard {
                name: view.read(cx).name().to_string(),
            },
        };
        Some(state)
    }

    /// Whether a query tab's buffer has changed since it was opened or last
    /// saved. A table tab counts the edits staged in its grid; a structure tab
    /// its unapplied column edits. An empty buffer is never dirty: a stray
    /// newline or spaces are not work a close would lose, so a fresh tab
    /// stays clean and closing it never asks.
    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        match &self.content {
            TabContent::Query {
                editor, baseline, ..
            } => {
                let sql = editor.read(cx).sql(cx);
                if sql.trim().is_empty() && baseline.trim().is_empty() {
                    false
                } else {
                    sql != *baseline
                }
            }
            TabContent::Table { view } => view.read(cx).has_staged_edits(cx),
            TabContent::Schema { view } => view.read(cx).is_dirty(cx),
            TabContent::Tool(_) | TabContent::Dashboard { .. } => false,
        }
    }

    /// Whether this tab holds changes that only exist on screen — staged
    /// rows in a table, a structure edit not yet applied — which re-reading
    /// it from a new connection throws away. A query buffer is not one: it
    /// survives a database switch.
    pub(crate) fn has_unapplied_changes(&self, cx: &App) -> bool {
        match &self.content {
            TabContent::Table { view } => view.read(cx).has_staged_edits(cx),
            TabContent::Schema { view } => view.read(cx).is_dirty(cx),
            _ => false,
        }
    }

    pub(crate) fn table_view(&self) -> Option<Entity<TableView>> {
        match &self.content {
            TabContent::Table { view } => Some(view.clone()),
            _ => None,
        }
    }

    pub(crate) fn schema_view(&self) -> Option<Entity<SchemaView>> {
        match &self.content {
            TabContent::Schema { view } => Some(view.clone()),
            _ => None,
        }
    }

    /// The tool view this tab holds, if it is a tool tab.
    #[cfg(test)]
    pub(crate) fn tool_view(&self) -> Option<&ToolView> {
        match &self.content {
            TabContent::Tool(view) => Some(view),
            _ => None,
        }
    }

    /// The dashboard's name, for bringing an already-open dashboard
    /// forward instead of opening a second tab for it.
    pub(crate) fn dashboard_name(&self, cx: &App) -> Option<String> {
        match &self.content {
            TabContent::Dashboard { view } => Some(view.read(cx).name().to_string()),
            _ => None,
        }
    }

    /// The dashboard tab's view, for the session's refresh path and tests.
    pub(crate) fn dashboard_view(&self) -> Option<Entity<DashboardView>> {
        match &self.content {
            TabContent::Dashboard { view } => Some(view.clone()),
            _ => None,
        }
    }

    /// Run the dashboard tab's own refresh: the session's `Cmd+R` never
    /// does this, since re-running could repeat a write and there is no
    /// window for a confirmation dialog there.
    pub(crate) fn dashboard_refresh(&self, cx: &mut Context<Self>) {
        if let TabContent::Dashboard { view } = &self.content {
            let view = view.clone();
            view.update(cx, |view, cx| view.refresh(cx));
        }
    }

    pub(crate) fn editor(&self) -> Option<Entity<QueryEditor>> {
        match &self.content {
            TabContent::Query { editor, .. } => Some(editor.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn grid(&self) -> Option<Entity<DataGrid>> {
        match &self.content {
            TabContent::Query { grid, .. } => Some(grid.clone()),
            _ => None,
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

    /// Show the plan pane or the result grid. The chart, the plan, and the
    /// messages are views of the same pane, so showing one hides the others.
    pub(crate) fn set_show_plan(&mut self, show: bool, cx: &mut Context<Self>) {
        if let TabContent::Query {
            show_plan,
            show_chart,
            show_messages,
            ..
        } = &mut self.content
        {
            *show_plan = show;
            *show_chart = false;
            *show_messages = false;
            cx.notify();
        }
    }

    /// Whether the pane is showing the shown result's messages rather than
    /// the result grid.
    pub(crate) fn showing_messages(&self) -> bool {
        matches!(
            &self.content,
            TabContent::Query {
                show_messages: true,
                ..
            }
        )
    }

    /// Show the messages pane or the result grid. Like the plan and the
    /// chart, it is a view of the same pane, so showing it hides them —
    /// and it only ever shows for a result that has messages, which the
    /// switch itself already guarantees.
    pub(crate) fn set_show_messages(&mut self, show: bool, cx: &mut Context<Self>) {
        if let TabContent::Query {
            show_plan,
            show_chart,
            show_messages,
            ..
        } = &mut self.content
        {
            *show_messages = show;
            *show_plan = false;
            *show_chart = false;
            cx.notify();
        }
    }

    /// The shown result's messages, if it has any — what the messages pane
    /// lists, and what the view bar's Messages segment appears for.
    pub(crate) fn shown_messages(&self) -> Vec<String> {
        match &self.content {
            TabContent::Query {
                results, result, ..
            } => results
                .get(*result)
                .map(|shown| {
                    shown
                        .messages
                        .iter()
                        .map(|message| message.display())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Put `plan` in this tab's viewer and show it, building the viewer the
    /// first time one is needed.
    pub(crate) fn set_plan(&mut self, plan: Plan, cx: &mut Context<Self>) {
        let summary = plan_summary(&plan);
        let TabContent::Query {
            plan: slot,
            show_plan,
            show_chart,
            show_messages,
            status,
            error,
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
        // The plan is what the pane shows now, not an earlier run's chart
        // or messages.
        *show_chart = false;
        *show_messages = false;
        // The plan is what the pane shows now, not an earlier run's error.
        *error = None;
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
            | TabContent::Tool(_)
            | TabContent::Dashboard { .. } => String::new(),
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
            | TabContent::Tool(_)
            | TabContent::Dashboard { .. } => (0, 0),
        }
    }

    /// Say a run was cancelled, unless whatever cancelled it has already
    /// said more: the sender is dropped when the run is given up on, so this
    /// arrives after `cancel_running` has said what was rolled back.
    pub(crate) fn note_cancelled(&mut self) {
        if let TabContent::Query { status, .. } = &mut self.content
            && matches!(status, Status::Running)
        {
            *status = Status::Done("Cancelled".into());
        }
    }

    pub(crate) fn set_status(&mut self, status: Status) {
        if let TabContent::Query {
            status: slot,
            error,
            show_messages,
            ..
        } = &mut self.content
        {
            // A fresh run replaces whatever the pane was showing, error
            // included; its outcome decides what the pane says next. The
            // messages go with it: until the new run lands, the pane is
            // the grid, not the previous run's messages.
            if matches!(status, Status::Running) {
                *error = None;
                *show_messages = false;
            }
            *slot = status;
        }
    }

    /// The error the result pane is showing, if the last run failed outright.
    pub(crate) fn pane_error(&self) -> Option<&str> {
        match &self.content {
            TabContent::Query { error, .. } => error.as_deref(),
            _ => None,
        }
    }

    /// Put a failure in the result pane: the run produced nothing, so the
    /// pane gives up the last run's rows and says what happened instead.
    /// The status keeps naming the error too; the bar shows the short form
    /// while the pane carries the whole message (see `render_status_bar`).
    pub(crate) fn show_pane_error(&mut self, message: String) {
        if let TabContent::Query {
            error,
            results,
            result,
            ..
        } = &mut self.content
        {
            *error = Some(message);
            results.clear();
            *result = 0;
        }
    }

    pub(crate) fn set_running(&mut self, handle: Option<tokio::task::AbortHandle>) {
        if let TabContent::Query { running, .. } = &mut self.content {
            *running = handle;
        }
    }

    /// Whether a run is in flight, holding the abort handle a second send
    /// would otherwise overwrite — or a script is paused on a failure,
    /// holding its connection until it is answered.
    pub(crate) fn has_running_task(&self) -> bool {
        matches!(
            &self.content,
            TabContent::Query {
                running: Some(_),
                ..
            } | TabContent::Query {
                script: Some(_),
                ..
            }
        )
    }

    /// The connection this query tab runs on, checked out of `connection`'s
    /// pool on first use. `None` for a tab that is not a query tab.
    ///
    /// When `shared` is `Some`, every query tab runs on that one connection
    /// instead of its own. A pin left over from another connection — the
    /// session switched database — is let go and replaced, and so is a pin
    /// from the other mode (`retired` names the session's old shared pin when
    /// the setting is off). A tab with an open transaction is the exception:
    /// it stays where it is until the transaction ends, since moving it would
    /// roll the transaction back.
    pub(crate) fn pin(
        &mut self,
        connection: &Arc<Connection>,
        shared: Option<&PinnedConnection>,
        retired: Option<&PinnedConnection>,
    ) -> Option<PinnedConnection> {
        let TabContent::Query { pinned, .. } = &mut self.content else {
            return None;
        };
        if let Some(wanted) = shared {
            let same = pinned.as_ref().is_some_and(|pin| pin.is_clone_of(wanted));
            let bound = !same && pinned.as_ref().is_some_and(|pin| pin.state().is_open());
            if !same && !bound {
                *pinned = Some(wanted.clone());
            }
        } else {
            let stale = pinned
                .as_ref()
                .is_none_or(|pin| !Arc::ptr_eq(pin.connection(), connection));
            let retired_clone =
                retired.is_some_and(|old| pinned.as_ref().is_some_and(|pin| pin.is_clone_of(old)));
            // A tab with an open transaction stays where it is until the
            // transaction ends, since moving it would roll the transaction
            // back — the same rule as the shared branch above.
            let bound = pinned.as_ref().is_some_and(|pin| pin.state().is_open());
            if stale || (retired_clone && !bound) {
                *pinned = Some(PinnedConnection::new(connection.clone()));
            }
        }
        pinned.clone()
    }

    /// This tab's connection, if the tab has run anything yet.
    pub(crate) fn pinned_clone(&self) -> Option<PinnedConnection> {
        match &self.content {
            TabContent::Query { pinned, .. } => pinned.clone(),
            _ => None,
        }
    }

    /// Whether this tab's connection is a clone of `pin` — the same server
    /// session — rather than a pin of its own.
    pub(crate) fn pinned_is(&self, pin: &PinnedConnection) -> bool {
        self.pinned_clone()
            .is_some_and(|current| current.is_clone_of(pin))
    }

    /// Whether this tab has a transaction open on its connection.
    pub(crate) fn transaction(&self) -> TxnState {
        match &self.content {
            TabContent::Query {
                pinned: Some(pin), ..
            } => pin.state(),
            _ => TxnState::Idle,
        }
    }

    /// Hold a script paused on a failure until the user answers.
    pub(crate) fn park_script(&mut self, run: Box<ScriptRun>) {
        if let TabContent::Query { script, .. } = &mut self.content {
            *script = Some(run);
        }
    }

    /// The paused script, taken to be answered — or `None` when it has
    /// already been answered, cancelled, or closed with its tab.
    pub(crate) fn take_script(&mut self) -> Option<Box<ScriptRun>> {
        match &mut self.content {
            TabContent::Query { script, .. } => script.take(),
            _ => None,
        }
    }

    /// Put a finished run's results in this tab.
    pub(crate) fn show_results(&mut self, results: Vec<QueryResult>, cx: &mut Context<Self>) {
        let TabContent::Query {
            grid,
            results: slot,
            result,
            error,
            show_plan,
            show_chart,
            show_messages,
            ..
        } = &mut self.content
        else {
            return;
        };

        let grid = grid.clone();
        *slot = results;
        *result = 0;
        // The run produced rows, so the pane is the grid again, not the
        // error an earlier run left there.
        *error = None;
        // A new run is what the user asked to see, so the grid comes back even
        // if a plan, a chart, or messages were showing.
        *show_plan = false;
        *show_chart = false;
        *show_messages = false;

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
            show_chart,
            show_messages,
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
        // The chart follows the result being shown, unless the new one has
        // nothing to chart — then the grid comes back instead of a chart
        // pane with no points. Messages follow the same rule: a result
        // with none leaves no messages pane to show.
        if !chartable(&chosen) {
            *show_chart = false;
        }
        if chosen.messages.is_empty() {
            *show_messages = false;
        }

        grid.update(cx, |grid, cx| grid.set_result(chosen, cx));
        let summary = self.result_summary();
        self.set_status(Status::Done(summary));
        cx.notify();
    }

    /// Write the shown result to a file, in `format`.
    ///
    /// The export covers the rows the last run loaded — what the grid is
    /// showing — rather than re-reading them from the server.
    pub(crate) fn export_result(&mut self, format: Format, cx: &mut Context<Self>) {
        let TabContent::Query {
            results,
            result,
            engine,
            ..
        } = &self.content
        else {
            return;
        };
        let Some(shown) = results.get(*result) else {
            return;
        };
        if shown.columns.is_empty() {
            return;
        }

        let engine = *engine;
        let columns = shown.columns.clone();
        let types = shown.column_types.clone();
        let rows = shown.rows.clone();
        let prompt = sql_file::prompt_for_save(None, "query-results", format.extension(), cx);

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            // Laying the rows out is O(rows×cols) string work; it runs on the
            // background executor so a big export does not stall the UI.
            let row_count = rows.len();
            let rendered = cx
                .background_spawn(async move {
                    export::render(format, engine, "", &columns, &types, &rows)
                })
                .await;
            let export::Rendered { text, skipped } = rendered;

            let written = cx
                .background_spawn(sql_file::write(path.clone(), text))
                .await;
            this.update_in(cx, |this, window, cx| {
                match written {
                    Ok(()) => {
                        this.set_status(Status::Done(export_notice(row_count, &path, skipped)))
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        this.set_status(Status::Error(message.clone()));
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put an export failure where the status bar and the toasts can see it.
    fn fail_export(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_status(Status::Error(message.clone()));
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
        cx.notify();
    }

    /// Whether the tab is showing a result worth exporting: a result with
    /// columns, i.e. rows the server returned rather than a write summary.
    fn exportable(&self) -> bool {
        matches!(&self.content, TabContent::Query { results, result, .. }
            if results.get(*result).is_some_and(|shown| !shown.columns.is_empty()))
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

        // A run that stopped at the tab's limit says so, or a page of rows
        // would read as the whole table.
        let summary = match self.limit_note(shown.row_count()) {
            Some(note) => format!("{} · {note}", shown.summary()),
            None => shown.summary(),
        };
        // Messages are easy to miss behind the grid, so the status bar
        // names them too — the count is what sends the user to the pane.
        let summary = match shown.messages.len() {
            0 => summary,
            1 => format!("{summary} · 1 message"),
            count => format!("{summary} · {count} messages"),
        };
        if results.len() == 1 {
            return summary;
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
            summary
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

    /// Whether this tab still holds exactly `sql` as a proposal nobody has
    /// edited or run: the buffer matches, and no run has left results, an
    /// error, a flight, a paused script, or a pinned connection behind.
    /// A run that left nothing else behind still leaves its pin, so a
    /// cancelled run counts as run.
    pub(crate) fn is_pristine_proposal(&self, sql: &str, cx: &App) -> bool {
        match &self.content {
            TabContent::Query {
                editor,
                results,
                error,
                running,
                script,
                pinned,
                status,
                ..
            } => {
                editor.read(cx).sql(cx) == sql
                    && results.is_empty()
                    && error.is_none()
                    && running.is_none()
                    && script.is_none()
                    && pinned.is_none()
                    && matches!(status, Status::Idle)
            }
            _ => false,
        }
    }

    /// Replace a query tab's buffer and its baseline together, as reusing a
    /// pristine proposal does: the tab still reads as unedited afterwards,
    /// so closing it never asks about the proposal it holds.
    pub(crate) fn set_query_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let TabContent::Query {
            editor, baseline, ..
        } = &mut self.content
        {
            editor.update(cx, |editor, cx| editor.set_buffer(sql, window, cx));
            *baseline = sql.to_string();
        }
    }

    /// Stop the run in flight, if there is one. Answers whether there
    /// was something to cancel.
    pub(crate) fn cancel_running(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.transaction().is_open();
        let TabContent::Query {
            editor,
            running,
            status,
            pinned,
            ..
        } = &mut self.content
        else {
            return false;
        };

        if !matches!(status, Status::Running) {
            return false;
        }

        let editor = editor.clone();
        *status = Status::Done("Cancelled".into());
        if let Some(running) = running.take() {
            // A run on the tab's own connection (not a plan, which is read
            // from the pool) leaves it in a state nothing can vouch for, so
            // the tab lets it go: the server is asked to stop the statement,
            // and the connection closes once the run is dropped, rolling back
            // whatever was open. The next run checks out a fresh one.
            if pinned.as_ref().is_some_and(PinnedConnection::is_busy)
                && let Some(pin) = pinned.take()
            {
                pin.cancel();
                if open {
                    *status = Status::Done(
                        "Cancelled; the tab's open transaction was rolled back".into(),
                    );
                }
            }
            running.abort();
        }

        if let Some(run) = self.take_script() {
            let message = match run.mode() {
                ScriptMode::Transaction => "Cancelled; the script's transaction was rolled back",
                ScriptMode::Autocommit if run.joined() => {
                    "Cancelled; the statements before the error are in the tab's open transaction"
                }
                ScriptMode::Autocommit => "Cancelled; the statements before the error were applied",
            };
            self.set_status(Status::Done(message.into()));
            close_script(run);
        }
        editor.update(cx, |editor, cx| editor.set_running(false, cx));
        cx.notify();
        true
    }

    /// Stop the run in flight without touching the editor or status —
    /// the panel is on its way out of the dock, or its connection is closing,
    /// either way.
    ///
    /// The server is asked to stop the statement too: dropping the socket
    /// alone does not, and an `UPDATE` left running would still commit after
    /// the user had closed the tab it was started from.
    pub(crate) fn abort_running(&mut self, cx: &mut Context<Self>) {
        if let TabContent::Query {
            running, pinned, ..
        } = &mut self.content
            && let Some(running) = running.take()
        {
            if pinned.as_ref().is_some_and(PinnedConnection::is_busy)
                && let Some(pin) = pinned.take()
            {
                pin.cancel();
            }
            running.abort();
        }
        if let Some(run) = self.take_script() {
            close_script(run);
        }
        if let TabContent::Dashboard { view } = &self.content {
            let view = view.clone();
            view.update(cx, |view, cx| view.abort_running(cx));
        }
    }

    /// Point this tab at `connection`: a query tab clears its grid, a table
    /// or structure tab re-reads from the new connection.
    pub(crate) fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        // A run still going belongs to the connection being left: stop it
        // (on the server too) and give the Run button back, or the tab would
        // say Idle while refusing every run until the old one ended — which a
        // statement on a dead socket may never do. A paused script goes the
        // same way.
        self.cancel_running(cx);
        if let Some(run) = self.take_script() {
            close_script(run);
            if let TabContent::Query { editor, .. } = &self.content {
                editor.update(cx, |editor, cx| editor.set_running(false, cx));
            }
        }
        match &mut self.content {
            TabContent::Query {
                grid,
                plan,
                results,
                result,
                show_plan,
                show_chart,
                show_messages,
                pinned,
                ..
            } => {
                // The tab's connection was to the database being left; the
                // next run checks one out of the new pool.
                *pinned = None;
                let grid = grid.clone();
                grid.update(cx, |grid, cx| grid.clear(cx));
                // A plan belongs to the connection it was read from.
                *plan = None;
                // So do the results: the view bar offers Messages for
                // whatever the shown result holds, and the old
                // connection's warnings must not survive the switch.
                results.clear();
                *result = 0;
                *show_plan = false;
                *show_chart = false;
                *show_messages = false;
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
            TabContent::Dashboard { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.set_connection(connection, cx));
            }
            TabContent::Tool(view) => view.set_connection(connection, cx),
        }
    }

    /// Reread a table tab's rows, or what a tool tab shows. A query tab's
    /// buffer is the user's own SQL and is left alone; a structure tab has
    /// nothing to refresh this way; a dashboard tab is left alone too —
    /// its own Refresh button is its run path, since re-running could
    /// repeat a write and there is no window for a confirmation dialog
    /// here.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        match &self.content {
            TabContent::Table { view } => {
                let view = view.clone();
                view.update(cx, |view, cx| view.refresh(cx));
            }
            TabContent::Tool(view) => view.refresh(cx),
            TabContent::Query { .. } | TabContent::Schema { .. } | TabContent::Dashboard { .. } => {
            }
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

    /// The saved query this tab belongs to, if any.
    pub(crate) fn saved_as(&self) -> Option<&str> {
        self.saved_as.as_deref()
    }

    /// Tie this tab to the saved query `name` (or to none), and let the
    /// editor's menu say which one Save will update.
    pub(crate) fn set_saved_as(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        if let TabContent::Query { editor, .. } = &self.content {
            let label = name.clone().map(SharedString::from);
            editor.update(cx, |editor, cx| editor.set_saved_name(label, cx));
        }
        self.saved_as = name;
        cx.notify();
    }

    /// What saving this tab to the sidebar's Saved tab would keep: its SQL
    /// and the name the dialog suggests. Only a query tab has SQL worth
    /// keeping, and an empty buffer has nothing to name.
    pub(crate) fn query_to_save(&self, cx: &App) -> Option<(String, String)> {
        let TabContent::Query { editor, .. } = &self.content else {
            return None;
        };
        let sql = editor.read(cx).sql(cx);
        if sql.trim().is_empty() {
            return None;
        }
        Some((sql, default_save_name(&self.title)))
    }

    /// The result pane, given over to the shown result's messages: one line
    /// per notice or warning, in the order the server reported them. The
    /// level is said in words as well as in colour, so nothing here is
    /// told by colour alone.
    fn render_messages(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let messages = self.shown_messages();
        let theme = cx.theme();
        div()
            .id("query-messages")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .children(messages.iter().map(|message| {
                div()
                    .text_sm()
                    .when(message.starts_with("Error"), |this| {
                        this.text_color(theme.danger)
                    })
                    .when(message.starts_with("Warning"), |this| {
                        this.text_color(theme.warning)
                    })
                    .child(message.clone())
            }))
            .into_any_element()
    }

    /// The result pane, given over to a failure: the whole message, in the
    /// space the rows would have filled, scrollable when it is long.
    fn render_pane_error(&self, error: &str, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        div()
            .id("query-error")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(format!("Error: {error}")),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, status: &Status, cx: &mut Context<Self>) -> impl IntoElement {
        let color = match status {
            Status::Error(_) => cx.theme().danger,
            _ => cx.theme().muted_foreground,
        };
        // While the pane carries a failure, the bar only names the state:
        // the whole message is in the pane, and repeating it here would
        // just cap and scroll it in a 72px strip.
        let message = if self.pane_error().is_some() && matches!(status, Status::Error(_)) {
            "Error".to_string()
        } else {
            status.message()
        };
        let transaction = self.transaction();
        let running = matches!(status, Status::Running);

        h_flex()
            .px_3()
            .py_1()
            .flex_none()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .children(self.render_limit(cx))
            // Errors can be long; cap the bar so it never eats the grid.
            .child(
                div()
                    .id("status")
                    .w_full()
                    .max_h(px(72.))
                    .overflow_y_scroll()
                    .text_xs()
                    .text_color(color)
                    .child(if matches!(status, Status::Running) {
                        busy_label(message, color).into_any_element()
                    } else {
                        message.into_any_element()
                    }),
            )
            .when(transaction.is_open(), |this| {
                this.child(self.render_transaction(transaction, running, cx))
            })
            .child({
                let panel = cx.entity().downgrade();
                let exportable = self.exportable();
                Button::new("export-result")
                    .ghost()
                    .xsmall()
                    .label("Export")
                    .dropdown_caret(true)
                    .tooltip("Write the shown result to a file")
                    .disabled(running || !exportable)
                    .dropdown_menu(move |mut menu, _window, _cx| {
                        for format in Format::RESULT_FILE {
                            let panel = panel.clone();
                            menu = menu.item(
                                PopupMenuItem::new(format!("Export as {}…", format.label()))
                                    .on_click(move |_, _window, cx| {
                                        if let Some(panel) = panel.upgrade() {
                                            panel.update(cx, |panel, cx| {
                                                panel.export_result(format, cx)
                                            });
                                        }
                                    }),
                            );
                        }
                        menu
                    })
            })
    }

    /// The open transaction, said in words beside the status — never by
    /// colour alone — with the two ways out of it.
    ///
    /// A failed transaction (Postgres) offers only Roll Back: the server
    /// refuses everything else, and a `COMMIT` there only rolls back anyway.
    fn render_transaction(
        &self,
        transaction: TxnState,
        running: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let failed = transaction == TxnState::Failed;
        let (label, color) = if failed {
            ("Transaction failed: roll back", cx.theme().danger)
        } else {
            ("Transaction open", cx.theme().info)
        };

        h_flex()
            .flex_none()
            .gap_1()
            .child(
                div()
                    .id("transaction-state")
                    .text_xs()
                    .text_color(color)
                    .child(label)
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(
                            "Changes since BEGIN are not visible to the sidebar, table \
                             views, or other tabs until they are committed.",
                        )
                        .build(window, cx)
                    }),
            )
            .when(!failed, |this| {
                this.child(
                    Button::new("transaction-commit")
                        .xsmall()
                        .label("Commit")
                        .tooltip("Commit the open transaction")
                        .disabled(running)
                        .on_click(cx.listener(|_this, _, _window, cx| {
                            cx.emit(SessionPanelEvent::EndTransaction(true));
                        })),
                )
            })
            .child(
                Button::new("transaction-rollback")
                    .xsmall()
                    .label("Roll back")
                    .tooltip("Roll back the open transaction")
                    .disabled(running)
                    .on_click(cx.listener(|_this, _, _window, cx| {
                        cx.emit(SessionPanelEvent::EndTransaction(false));
                    })),
            )
    }

    /// The bar above the result area: one segment per result the last run
    /// produced, and the Results/Chart/Plan switch for what the pane shows.
    /// Both are choices of what the pane shows, so the chosen one is marked
    /// by its segment the way a tab is, not by a primary button. It reads
    /// the tab's own state rather than taking it as arguments, so the
    /// signature stays short.
    fn render_view_bar(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let TabContent::Query {
            results: all,
            result,
            plan,
            ..
        } = &self.content
        else {
            return div().into_any_element();
        };
        let count = all.len();
        let shown = *result;
        let has_plan = plan.is_some();
        let showing_plan = self.showing_plan() && has_plan;
        let chartable = all.get(shown).is_some_and(chartable);
        let has_messages = all
            .get(shown)
            .is_some_and(|shown| !shown.messages.is_empty());
        // The chart, the plan, and the messages are views of the same pane;
        // the plan wins when both are asked for, and messages only ever
        // show for a result that has them.
        let showing_chart = self.showing_chart() && chartable && !showing_plan;
        let showing_messages =
            self.showing_messages() && has_messages && !showing_plan && !showing_chart;
        // Segments in order: Results, then Chart when the shown result has
        // numbers in it, then Plan when one has been read, then Messages
        // when the shown result has any.
        let chart_index = chartable.then_some(1);
        let plan_index = has_plan.then_some(1 + usize::from(chartable));
        let messages_index =
            has_messages.then_some(1 + usize::from(chartable) + usize::from(has_plan));
        let selected = if showing_chart {
            chart_index
        } else if showing_plan {
            plan_index
        } else if showing_messages {
            messages_index
        } else {
            Some(0)
        }
        .unwrap_or(0);

        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            // While the plan is showing there is no result to pick between.
            .when(count > 1 && !showing_plan, |this| {
                this.child(
                    TabBar::new("result-tabs")
                        .segmented()
                        .xsmall()
                        .selected_index(shown)
                        .on_click(cx.listener(|this, index: &usize, _window, cx| {
                            this.show_result(*index, cx)
                        }))
                        .children((0..count).map(|index| {
                            let label = format!("Result {}", index + 1);
                            Tab::new().aria_label(label.clone()).label(label)
                        })),
                )
            })
            .when(chartable || has_plan || has_messages, |this| {
                this.child(
                    h_flex().flex_none().ml_auto().child(
                        TabBar::new("view-tabs")
                            .segmented()
                            .xsmall()
                            .selected_index(selected)
                            .on_click(cx.listener(move |this, index: &usize, window, cx| {
                                if Some(*index) == chart_index {
                                    this.set_show_chart(true, cx);
                                } else if Some(*index) == plan_index {
                                    this.set_show_plan(true, cx);
                                    if let Some(plan) = this.plan_view() {
                                        plan.update(cx, |plan, cx| plan.focus(window, cx));
                                    }
                                } else if Some(*index) == messages_index {
                                    this.set_show_messages(true, cx);
                                } else {
                                    this.set_show_plan(false, cx);
                                }
                            }))
                            .child(Tab::new().aria_label("Results").label("Results"))
                            .when(chartable, |this| {
                                this.child(Tab::new().aria_label("Chart").label("Chart"))
                            })
                            .when(has_plan, |this| {
                                this.child(Tab::new().aria_label("Plan").label("Plan"))
                            })
                            .when(has_messages, |this| {
                                this.child(Tab::new().aria_label("Messages").label("Messages"))
                            }),
                    ),
                )
            })
            .into_any_element()
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
            // A tool's grid or canvas, for the same reason.
            TabContent::Tool(view) => view.focus_handle(cx),
            // The dashboard's own handle: its buttons and widget dialogs
            // are what the keyboard is for there.
            TabContent::Dashboard { view } => view.read(cx).focus_handle(),
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
        self.abort_running(cx);
        cx.emit(SessionPanelEvent::Removed);
    }
}

/// The name the save-query dialog prefills: the tab's title, unless it is
/// still the default `Query N` the session hands out, which names nothing.
fn default_save_name(title: &SharedString) -> String {
    let title = title.as_ref();
    let is_default = title
        .strip_prefix("Query ")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
    if is_default {
        String::new()
    } else {
        title.to_string()
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
        let transaction = self.transaction().is_open();

        h_flex()
            .id(("session-panel-title", key))
            .test_support()
            .gap_1()
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
            // A transaction left open in a tab out of sight is easy to
            // forget, and closing the tab would roll it back.
            .when(transaction, |this| {
                this.child(
                    div()
                        .id(("open-transaction", key))
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(cx.theme().info)
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Open transaction")
                                .build(window, cx)
                        }),
                )
            })
            .child(
                Button::new(SharedString::from(format!("close-tab-{key}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .accessibility_label(match (dirty, transaction) {
                        (true, true) => format!(
                            "Close the {kind} tab {title} (unsaved changes, open transaction)"
                        ),
                        (true, false) => format!("Close the {kind} tab {title} (unsaved changes)"),
                        (false, true) => format!("Close the {kind} tab {title} (open transaction)"),
                        (false, false) => format!("Close the {kind} tab {title}"),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = self.key;
        let body = match &self.content {
            TabContent::Query {
                editor,
                grid,
                status,
                results,
                result,
                error,
                plan,
                show_plan,
                ..
            } => {
                let has_plan = plan.is_some();
                let showing_plan = *show_plan && has_plan;
                let shown = results.get(*result);
                let chartable = shown.is_some_and(chartable);
                let has_messages = shown.is_some_and(|shown| !shown.messages.is_empty());
                // The chart, the plan, and the messages are views of the
                // same pane; the plan wins when both are asked for, and
                // the chart and the messages only show for a result that
                // has numbers in it, or messages, respectively.
                let showing_chart = self.showing_chart() && chartable && !showing_plan;
                let showing_messages =
                    self.showing_messages() && has_messages && !showing_plan && !showing_chart;
                // A run that failed outright owns the pane: the error is
                // shown where its rows would have been, in this tab, for as
                // long as the tab is kept.
                let content: gpui_kit::AnyElement =
                    match (error, showing_plan, showing_chart, showing_messages, plan) {
                        (Some(error), _, _, _, _) => self.render_pane_error(error, cx),
                        (_, true, _, _, Some(plan)) => plan.clone().into_any_element(),
                        (_, _, true, _, _) => match shown {
                            Some(result) => self.render_chart(result, cx),
                            None => grid.clone().into_any_element(),
                        },
                        (_, _, _, true, _) => self.render_messages(cx),
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
                                        .size_range(px(120.)..px(10_000.))
                                        .child(editor.clone()),
                                )
                                .child(
                                    resizable_panel()
                                        .size(window.viewport_size().height * RESULTS_SHARE)
                                        .size_range(px(80.)..px(10_000.))
                                        .child(
                                            v_flex()
                                                .size_full()
                                                // A script leaves one result per
                                                // statement, a plan leaves the switch,
                                                // a chartable result leaves the chart
                                                // switch, and a result with messages
                                                // leaves the messages switch; a lone
                                                // result alone would make the bar say
                                                // nothing. While the pane shows an
                                                // error there is nothing to switch
                                                // between.
                                                .when(
                                                    error.is_none()
                                                        && (results.len() > 1
                                                            || has_plan
                                                            || chartable
                                                            || has_messages),
                                                    |this| this.child(self.render_view_bar(cx)),
                                                )
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
            TabContent::Dashboard { view } => view.clone().into_any_element(),
            TabContent::Tool(view) => view.element(),
        };

        div()
            .id(("session-panel", key))
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(body)
    }
}

/// Give up on a paused script on the database runtime, where its connection
/// has to be closed — closing it is what rolls an open transaction back.
pub(crate) fn close_script(run: Box<ScriptRun>) {
    // Nothing to wait for: the connection goes either way.
    drop(runtime::spawn(run.close()));
}

/// What the status bar says after an export: where it went, and — when the
/// driver had only a description of some values — how many were written as
/// `NULL`.
fn export_notice(rows: usize, path: &Path, skipped: usize) -> String {
    let unit = if rows == 1 { "row" } else { "rows" };
    let mut notice = format!("Exported {rows} {unit} to {}", path.display());
    match skipped {
        0 => {}
        1 => notice.push_str(" (1 value could not be read back and was written as NULL)"),
        skipped => notice.push_str(&format!(
            " ({skipped} values could not be read back and were written as NULL)"
        )),
    }
    notice
}
