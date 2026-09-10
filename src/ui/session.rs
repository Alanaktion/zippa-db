//! An open connection: object sidebar, query editor, and result grid, split by
//! draggable panes.

use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, ResizableState, Sizable, h_flex, h_resizable,
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, MouseButton, SharedString, Window, actions, div, px,
};

use regex::{Regex, RegexBuilder};

use crate::db::{Connection, DatabaseObject, ObjectKind, runtime};
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::table_view::TableView;

/// Starting pane sizes; the user drags from here.
const SIDEBAR_WIDTH: f32 = 260.;
const EDITOR_HEIGHT: f32 = 220.;

actions!(zippa_db, [NewTab, CloseTab]);

pub enum SessionEvent {
    /// The user closed the session; the workspace returns to the connection
    /// manager and drains the pool.
    Disconnected,
}

enum Status {
    Idle,
    Running,
    Done(String),
    Error(String),
}

/// What a tab holds: a query editor with its result, or a table opened from
/// the sidebar.
enum TabContent {
    Query {
        editor: Entity<QueryEditor>,
        grid: Entity<DataGrid>,
        status: Status,
    },
    Table {
        view: Entity<TableView>,
    },
}

struct SessionTab {
    title: SharedString,
    content: TabContent,
}

impl SessionTab {
    /// The table this tab shows, if it is a table tab.
    fn object(&self, cx: &gpui_kit::App) -> Option<DatabaseObject> {
        match &self.content {
            TabContent::Table { view } => Some(view.read(cx).object().clone()),
            TabContent::Query { .. } => None,
        }
    }
}

pub struct Session {
    connection: Arc<Connection>,
    tabs: Vec<SessionTab>,
    active: usize,
    /// Numbers the untitled tabs, so closing one does not reuse its name.
    opened: usize,
    /// Sidebar beside the editor and grid.
    columns: Entity<ResizableState>,
    /// Editor above the grid, shared by every tab so the split stays put.
    panes: Entity<ResizableState>,
    databases: Vec<String>,
    objects: Vec<DatabaseObject>,
    /// Text filter over the object list.
    filter: Entity<InputState>,
    /// Compiled form of the filter; `None` means "show everything".
    matcher: Option<Regex>,
    /// Set when the schema could not be read; queries still work.
    metadata_error: Option<String>,
    switching: bool,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables (regex)"));
        cx.subscribe_in(&filter, window, Self::on_filter_event)
            .detach();

        let mut session = Self {
            connection,
            tabs: Vec::new(),
            active: 0,
            opened: 0,
            columns: cx.new(|_| ResizableState::default()),
            panes: cx.new(|_| ResizableState::default()),
            databases: Vec::new(),
            objects: Vec::new(),
            filter,
            matcher: None,
            metadata_error: None,
            switching: false,
        };
        session.open_tab(None, String::new(), false, window, cx);
        session.reload_metadata(cx);
        session
    }

    /// Add a tab and make it active.
    ///
    /// `title` defaults to a running "Query N"; `run` executes `sql` right
    /// away, which is how the sidebar opens a table.
    fn open_tab(
        &mut self,
        title: Option<String>,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.opened += 1;
        let title = title.unwrap_or_else(|| format!("Query {}", self.opened));

        let editor = cx.new(|cx| QueryEditor::with_text(sql.clone(), window, cx));
        cx.subscribe_in(&editor, window, Self::on_editor_event)
            .detach();

        self.tabs.push(SessionTab {
            title: title.into(),
            content: TabContent::Query {
                editor,
                grid: cx.new(|cx| DataGrid::new(window, cx)),
                status: Status::Idle,
            },
        });
        self.active = self.tabs.len() - 1;

        if run && !sql.trim().is_empty() {
            self.run(self.active, sql, cx);
        }
        cx.notify();
    }

    /// Open a table or view from the sidebar in its own tab.
    /// Open a table or view from the sidebar in its own tab.
    ///
    /// A table already open is brought forward rather than opened twice.
    fn open_object(
        &mut self,
        object: &DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.object(cx).as_ref() == Some(object))
        {
            self.activate_tab(index, cx);
            return;
        }

        let view = cx.new(|cx| TableView::new(self.connection.clone(), object.clone(), window, cx));

        self.tabs.push(SessionTab {
            title: object.label().into(),
            content: TabContent::Table { view },
        });
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        self.tabs.remove(index);
        if self.tabs.is_empty() {
            // The session always shows one editor.
            self.open_tab(None, String::new(), false, window, cx);
            return;
        }

        self.active = self.active.min(self.tabs.len() - 1);
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            cx.notify();
        }
    }

    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(None, String::new(), false, window, cx);
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active, window, cx);
    }

    pub fn connection(&self) -> Arc<Connection> {
        self.connection.clone()
    }

    /// Read the database list and the current database's tables and views.
    fn reload_metadata(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let task =
            runtime::spawn(
                async move { (connection.databases().await, connection.objects().await) },
            );

        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                this.metadata_error = None;
                match loaded {
                    Ok((databases, objects)) => {
                        match databases {
                            Ok(databases) => this.databases = databases,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        match objects {
                            Ok(objects) => this.objects = objects,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                    }
                    Err(_) => this.metadata_error = Some("reading the schema was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reopen the pool against `database` and reload the object list.
    ///
    /// No engine can move an open pool to another database, so this replaces
    /// the connection and drains the old one in the background.
    pub(crate) fn switch_database(&mut self, database: String, cx: &mut Context<Self>) {
        // A file-based engine has a single database; "switching" would try to
        // open a file named after it.
        if self.connection.config.engine.is_file_based() {
            return;
        }
        if self.switching || database == self.connection.database() {
            return;
        }

        self.switching = true;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.with_database(&database).await });

        cx.spawn(async move |this, cx| {
            let opened = task.await;
            this.update(cx, |this, cx| {
                this.switching = false;
                match opened {
                    Ok(Ok(connection)) => {
                        let previous =
                            std::mem::replace(&mut this.connection, Arc::new(connection));
                        runtime::spawn(async move { previous.close().await });

                        this.objects.clear();
                        let connection = this.connection.clone();
                        for index in 0..this.tabs.len() {
                            match &this.tabs[index].content {
                                TabContent::Query { grid, .. } => {
                                    let grid = grid.clone();
                                    grid.update(cx, |grid, cx| grid.clear(cx));
                                    this.set_status(index, Status::Idle);
                                }
                                // A table tab points at a table in the database
                                // it was opened from; re-read it in the new one.
                                TabContent::Table { view } => {
                                    let view = view.clone();
                                    let connection = connection.clone();
                                    view.update(cx, |view, cx| view.set_connection(connection, cx));
                                }
                            }
                        }
                        this.reload_metadata(cx);
                    }
                    Ok(Err(error)) => {
                        this.set_status(this.active, Status::Error(format!("{error:#}")))
                    }
                    Err(_) => this.set_status(
                        this.active,
                        Status::Error("switching database was cancelled".into()),
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_filter_event(
        &mut self,
        filter: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }

        let pattern = filter.read(cx).value().trim().to_string();
        self.matcher = compile_filter(&pattern);
        cx.notify();
    }

    /// The objects the filter lets through, in sidebar order.
    fn visible_objects(&self) -> Vec<&DatabaseObject> {
        self.objects
            .iter()
            .filter(|object| match &self.matcher {
                Some(matcher) => matcher.is_match(&object.label()),
                None => true,
            })
            .collect()
    }

    fn on_editor_event(
        &mut self,
        editor: &Entity<QueryEditor>,
        event: &QueryEditorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let QueryEditorEvent::Run(sql) = event;
        let Some(index) = self.query_tab_of(editor) else {
            return;
        };
        self.run(index, sql.clone(), cx);
    }

    /// Index of the query tab owning `editor`.
    fn query_tab_of(&self, editor: &Entity<QueryEditor>) -> Option<usize> {
        self.tabs.iter().position(|tab| match &tab.content {
            TabContent::Query { editor: owned, .. } => owned == editor,
            TabContent::Table { .. } => false,
        })
    }

    fn set_status(&mut self, index: usize, status: Status) {
        if let Some(TabContent::Query { status: slot, .. }) =
            self.tabs.get_mut(index).map(|tab| &mut tab.content)
        {
            *slot = status;
        }
    }

    /// Run `sql` for the tab at `index`; its own grid and status follow it.
    fn run(&mut self, index: usize, sql: String, cx: &mut Context<Self>) {
        let Some(TabContent::Query { editor, grid, .. }) =
            self.tabs.get(index).map(|tab| &tab.content)
        else {
            return;
        };
        let (editor, grid) = (editor.clone(), grid.clone());

        self.set_status(index, Status::Running);
        editor.update(cx, |editor, cx| editor.set_running(true, cx));
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.run_query(&sql).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // The tab may have been closed or moved while the query ran.
                let Some(index) = this.query_tab_of(&editor) else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));

                match result {
                    Ok(Ok(query_result)) => {
                        this.set_status(index, Status::Done(query_result.summary()));
                        grid.update(cx, |grid, cx| grid.set_result(query_result, cx));
                    }
                    Ok(Err(error)) => {
                        this.set_status(index, Status::Error(format!("{error:#}")));
                        grid.update(cx, |grid, cx| grid.clear(cx));
                    }
                    Err(_) => {
                        this.set_status(index, Status::Error("the query was cancelled".into()))
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

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

    #[cfg(test)]
    pub(crate) fn active_sql(&self, cx: &gpui_kit::App) -> String {
        match &self.tabs[self.active].content {
            TabContent::Query { editor, .. } => editor.read(cx).sql(cx),
            TabContent::Table { view } => view.read(cx).query(),
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

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = cx.entity().downgrade();

        TabBar::new("query-tabs")
            .selected_index(self.active)
            .on_click(cx.listener(|this, index: &usize, _window, cx| this.activate_tab(*index, cx)))
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let session = session.clone();

                let middle_click = session.clone();

                Tab::new()
                    .label(tab.title.clone())
                    // Middle-click closes, the way it does in a browser.
                    .on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        if let Some(session) = middle_click.upgrade() {
                            session.update(cx, |session, cx| session.close_tab(index, window, cx));
                        }
                    })
                    .suffix(
                        Button::new(SharedString::from(format!("close-tab-{index}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close tab")
                            .on_click(move |_, window, cx| {
                                if let Some(session) = session.upgrade() {
                                    session.update(cx, |session, cx| {
                                        session.close_tab(index, window, cx)
                                    });
                                }
                            }),
                    )
            }))
            .suffix(
                Button::new("new-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New query tab")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_tab(None, String::new(), false, window, cx)
                    })),
            )
    }

    fn render_database_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.connection.database().to_string();

        // A SQLite connection is one file: there is nothing to switch to, and
        // its "databases" (main, plus attachments) are not separate files.
        if self.connection.config.engine.is_file_based() {
            let file = std::path::Path::new(&current)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or(current);

            return Button::new("database")
                .outline()
                .small()
                .w_full()
                .label(file)
                .disabled(true)
                .into_any_element();
        }

        let databases = self.databases.clone();
        let session = cx.entity().downgrade();

        let label = if self.switching {
            "Switching…".to_string()
        } else if current.is_empty() {
            "No database".to_string()
        } else {
            current.clone()
        };

        Button::new("database")
            .outline()
            .small()
            .w_full()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _window, _cx| {
                if databases.is_empty() {
                    return menu.label("No databases");
                }

                for database in &databases {
                    let name = database.clone();
                    let session = session.clone();

                    menu = menu.item(
                        PopupMenuItem::new(database.clone())
                            .checked(*database == current)
                            .on_click(move |_, _window, cx| {
                                let name = name.clone();
                                if let Some(session) = session.upgrade() {
                                    session.update(cx, |session, cx| {
                                        session.switch_database(name, cx)
                                    });
                                }
                            }),
                    );
                }

                menu.scrollable(true).max_h(px(420.))
            })
            .into_any_element()
    }

    fn render_objects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let objects = self.visible_objects();
        let filtering = self.matcher.is_some();

        let notice = match (
            &self.metadata_error,
            self.objects.is_empty(),
            objects.is_empty(),
        ) {
            (Some(error), _, _) => Some((error.clone(), cx.theme().danger)),
            (None, true, _) => Some((
                "No tables or views".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, true) => Some((
                "Nothing matches the filter".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, false) => None,
        };

        let heading = if filtering {
            format!("TABLES & VIEWS ({}/{})", objects.len(), self.objects.len())
        } else {
            "TABLES & VIEWS".to_string()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_1()
            .child(
                Input::new(&self.filter)
                    .id("object-filter")
                    .small()
                    .cleanable(true),
            )
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .when_some(notice, |this, (message, color)| {
                this.child(div().px_1().text_xs().text_color(color).child(message))
            })
            .child(
                div()
                    .id("objects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .children(objects.into_iter().map(|object| {
                                let label = object.label();
                                let kind = object.kind;
                                let object = object.clone();

                                h_flex()
                                    .id(SharedString::from(format!("object-{label}")))
                                    .test_support()
                                    .w_full()
                                    .px_1()
                                    .py_0p5()
                                    .gap_2()
                                    .justify_between()
                                    .rounded(cx.theme().radius)
                                    .cursor_pointer()
                                    .hover(|style| style.bg(cx.theme().sidebar_accent))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_object(&object, window, cx)
                                    }))
                                    .child(div().text_sm().truncate().child(label))
                                    .when(kind == ObjectKind::View, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("view"),
                                        )
                                    })
                            })),
                    ),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = &self.connection.config;

        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .p_3()
            .gap_3()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().truncate().child(config.display_name()))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(config.display_target()),
                    ),
            )
            .child(self.render_database_picker(cx))
            .child(self.render_objects(cx))
            .child(
                Button::new("disconnect")
                    .outline()
                    .small()
                    .w_full()
                    .label("Disconnect")
                    .on_click(cx.listener(|_this, _, _window, cx| {
                        cx.emit(SessionEvent::Disconnected);
                    })),
            )
    }

    fn render_status_bar(&self, status: &Status, cx: &mut Context<Self>) -> impl IntoElement {
        let (message, color) = match status {
            Status::Idle => ("Ready".to_string(), cx.theme().muted_foreground),
            Status::Running => ("Running…".to_string(), cx.theme().muted_foreground),
            Status::Done(summary) => (summary.clone(), cx.theme().muted_foreground),
            Status::Error(error) => (error.clone(), cx.theme().danger),
        };

        h_flex()
            .w_full()
            .px_3()
            .py_1()
            .flex_none()
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

    fn render_panes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.tabs[self.active].content {
            TabContent::Query {
                editor,
                grid,
                status,
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
                            .child(resizable_panel().child(grid.clone())),
                    ),
                )
                .child(self.render_status_bar(status, cx))
                .into_any_element(),
            // The table view carries its own footer, so it fills the pane.
            TabContent::Table { view } => view.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .child(self.render_tab_bar(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Render for Session {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .key_context("Session")
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .child(
                h_resizable("session-columns")
                    .with_state(&self.columns)
                    .child(
                        resizable_panel()
                            .size(px(SIDEBAR_WIDTH))
                            .size_range(px(180.)..px(560.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(resizable_panel().child(self.render_panes(cx))),
            )
    }
}

/// Turn the filter box's text into a matcher.
///
/// The pattern is a case-insensitive regex; while it is still being typed it is
/// often not valid (`user(`), so an unparseable pattern falls back to matching
/// the text literally rather than showing nothing.
fn compile_filter(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }

    let case_insensitive = |pattern: &str| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .ok()
    };

    case_insensitive(pattern).or_else(|| case_insensitive(&regex::escape(pattern)))
}
