//! `SessionPanel`: one dock panel wrapping a query editor+grid, a table view,
//! or a structure view — the same three-variant [`TabContent`] the session
//! used to hold in a plain `Vec`, now a dock-managed panel with its own
//! title, toolbar and closing behavior.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent, PanelId, TabGroup};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme, IconName, ResizableState, Sizable, h_flex, resizable_panel, v_flex, v_resizable,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    SharedString, WeakEntity, Window, div, px,
};

use crate::db::Connection;
use crate::db::DatabaseObject;
use crate::db::query::QueryResult;
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::schema_view::SchemaView;
use crate::ui::sql_file;
use crate::ui::table_view::TableView;

use super::tab::{ObjectViewMode, Status, TabContent};
use super::{CloseTab, NewTab};

/// Starting height of the editor pane above the grid; the user drags from
/// here. Per-panel now: a split puts two query panels on screen at once, and
/// a single shared height would fight between them.
const EDITOR_HEIGHT: f32 = 220.;

pub(crate) enum SessionPanelEvent {
    /// The user is working in this panel now.
    Focused,
    /// The panel left the dock, however it left.
    Removed,
    /// The tab's ✕, or a middle click on it.
    CloseRequested,
    /// The strip's + — a query tab beside this one.
    NewTabRequested,
    Run(String),
    RunScript(String),
    /// The status bar's Run answered a held-back write.
    ConfirmRun(String),
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| QueryEditor::with_text(sql.clone(), window, cx));
        cx.subscribe_in(&editor, window, Self::on_editor_event)
            .detach();

        Self {
            key,
            title: title.into(),
            content: TabContent::Query {
                editor,
                grid: cx.new(|cx| DataGrid::new(window, cx)),
                status: Status::Idle,
                path: None,
                results: Vec::new(),
                result: 0,
                running: None,
                baseline: sql,
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
            QueryEditorEvent::Open => cx.emit(SessionPanelEvent::OpenFile),
            QueryEditorEvent::Save => cx.emit(SessionPanelEvent::Save),
        }
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

    /// The tab group displaying this panel, if the dock has taken it yet.
    pub(crate) fn group(&self) -> Option<WeakEntity<TabGroup>> {
        self.group.clone()
    }

    /// The table this tab shows, if it is a table or structure tab.
    pub(crate) fn object(&self, cx: &App) -> Option<DatabaseObject> {
        match &self.content {
            TabContent::Table { view } => Some(view.read(cx).object().clone()),
            TabContent::Schema { view } => Some(view.read(cx).object().clone()),
            TabContent::Query { .. } => None,
        }
    }

    /// Which view mode this tab is showing its object in, if it is showing one.
    pub(crate) fn mode(&self) -> Option<ObjectViewMode> {
        match &self.content {
            TabContent::Table { .. } => Some(ObjectViewMode::Data),
            TabContent::Schema { .. } => Some(ObjectViewMode::Schema),
            TabContent::Query { .. } => None,
        }
    }

    /// Whether a query tab's buffer has changed since it was opened or last
    /// saved. A table tab has no buffer, so it is never dirty; a structure
    /// tab is dirty when it has unapplied column edits.
    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        match &self.content {
            TabContent::Query {
                editor, baseline, ..
            } => &editor.read(cx).sql(cx) != baseline,
            TabContent::Table { .. } => false,
            TabContent::Schema { view } => view.read(cx).is_dirty(cx),
        }
    }

    pub(crate) fn table_view(&self) -> Option<Entity<TableView>> {
        match &self.content {
            TabContent::Table { view } => Some(view.clone()),
            TabContent::Query { .. } | TabContent::Schema { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn schema_view(&self) -> Option<Entity<SchemaView>> {
        match &self.content {
            TabContent::Schema { view } => Some(view.clone()),
            TabContent::Query { .. } | TabContent::Table { .. } => None,
        }
    }

    pub(crate) fn editor(&self) -> Option<Entity<QueryEditor>> {
        match &self.content {
            TabContent::Query { editor, .. } => Some(editor.clone()),
            TabContent::Table { .. } | TabContent::Schema { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn grid(&self) -> Option<Entity<DataGrid>> {
        match &self.content {
            TabContent::Query { grid, .. } => Some(grid.clone()),
            TabContent::Table { .. } | TabContent::Schema { .. } => None,
        }
    }

    /// The editor and grid entities a run acts on.
    pub(crate) fn query_parts(&self) -> Option<(Entity<QueryEditor>, Entity<DataGrid>)> {
        match &self.content {
            TabContent::Query { editor, grid, .. } => Some((editor.clone(), grid.clone())),
            _ => None,
        }
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
            TabContent::Table { .. } | TabContent::Schema { .. } => String::new(),
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
            TabContent::Table { .. } | TabContent::Schema { .. } => (0, 0),
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
            ..
        } = &mut self.content
        else {
            return;
        };

        let grid = grid.clone();
        *slot = results;
        *result = 0;

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

    /// Leave the statement waiting on confirmation unrun; the buffer it came
    /// from is untouched either way.
    pub(crate) fn cancel_run(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            &self.content,
            TabContent::Query {
                status: Status::Confirm(_),
                ..
            }
        ) {
            return;
        }

        self.set_status(Status::Done("Not run".into()));
        cx.notify();
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
        match &self.content {
            TabContent::Query { grid, .. } => {
                let grid = grid.clone();
                grid.update(cx, |grid, cx| grid.clear(cx));
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
        }
    }

    /// Reread a table tab's rows. A query tab's buffer is the user's own SQL
    /// and is left alone; a structure tab has nothing to refresh this way.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        if let TabContent::Table { view } = &self.content {
            let view = view.clone();
            view.update(cx, |view, cx| view.refresh(cx));
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
        // early and never reaches `focus_active_panel`.
        panel.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn render_status_bar(&self, status: &Status, cx: &mut Context<Self>) -> impl IntoElement {
        let color = match status {
            Status::Error(_) => cx.theme().danger,
            Status::Confirm(_) => cx.theme().warning,
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
            // The statement itself is in the editor above, so the bar only
            // has to carry the answer.
            .when(matches!(status, Status::Confirm(_)), |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(
                            Button::new("cancel-run")
                                .ghost()
                                .xsmall()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _window, cx| this.cancel_run(cx))),
                        )
                        .child(
                            Button::new("confirm-run")
                                .primary()
                                .xsmall()
                                .label("Run")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    if let TabContent::Query {
                                        status: Status::Confirm(sql),
                                        ..
                                    } = &this.content
                                    {
                                        let sql = sql.clone();
                                        cx.emit(SessionPanelEvent::ConfirmRun(sql));
                                    }
                                })),
                        ),
                )
            })
    }

    /// One button per result the last run produced.
    fn render_result_bar(
        &self,
        results: usize,
        shown: usize,
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
                    .on_click(cx.listener(move |this, _, _window, cx| this.show_result(index, cx)));

                if index == shown {
                    button.primary()
                } else {
                    button.ghost()
                }
            }))
    }
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
            .child(div().child(label))
            .child(
                Button::new(SharedString::from(format!("close-tab-{key}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .accessibility_label(if dirty {
                        format!("Close the tab {title} (unsaved changes)")
                    } else {
                        format!("Close the tab {title}")
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
        if self.is_dirty(cx) {
            menu.item(PopupMenuItem::new("Close (unsaved changes)").disabled(true))
        } else {
            menu
        }
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
                ..
            } => v_flex()
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
                                        // statement; a single query leaves
                                        // one, and the bar for it would say
                                        // nothing.
                                        .when(results.len() > 1, |this| {
                                            this.child(self.render_result_bar(
                                                results.len(),
                                                *result,
                                                cx,
                                            ))
                                        })
                                        .child(div().flex_1().min_h_0().child(grid.clone())),
                                ),
                            ),
                    ),
                )
                .child(self.render_status_bar(status, cx))
                .into_any_element(),
            // The table/structure view carries its own footer, so it fills
            // the pane.
            TabContent::Table { view } => view.clone().into_any_element(),
            TabContent::Schema { view } => view.clone().into_any_element(),
        };

        div()
            .id(("session-panel", key))
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(body)
    }
}
