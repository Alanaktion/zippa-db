//! An open connection: object sidebar, query editor, and result grid, split by
//! draggable panes.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, ResizableState, Sizable, h_flex, h_resizable,
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, MouseButton, SharedString, Window, actions, div, px,
};

use regex::Regex;

use crate::db::query::QueryResult;
use crate::db::{Connection, DatabaseObject, runtime, statement};
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::sql_file;
use crate::ui::table_view::TableView;

mod sidebar;
mod tab;
#[cfg(test)]
mod test_support;

use tab::{SessionTab, Status, TabContent};

/// Starting pane sizes; the user drags from here.
const SIDEBAR_WIDTH: f32 = 260.;
const EDITOR_HEIGHT: f32 = 220.;

actions!(
    zippa_db,
    [
        NewTab,
        CloseTab,
        OpenFile,
        SaveFile,
        SaveFileAs,
        Refresh,
        CancelQuery
    ]
);

pub enum SessionEvent {
    /// The user closed the session; the workspace returns to the connection
    /// manager and drains the pool.
    Disconnected,
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
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables"));
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
                path: None,
                results: Vec::new(),
                result: 0,
                running: None,
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

    /// Same as [`Self::on_new_tab`], for the toolbar button which does not
    /// have an `&NewTab` to hand it.
    pub(crate) fn new_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(None, String::new(), false, window, cx);
    }

    fn on_refresh(&mut self, _: &Refresh, _window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    /// Reread the schema, and the rows of the table being looked at.
    ///
    /// A query tab is left alone on purpose: its buffer is the user's own
    /// SQL, run verbatim, and re-running it could repeat a write.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_metadata(cx);
        if let Some(TabContent::Table { view }) = self.tabs.get(self.active).map(|tab| &tab.content)
        {
            let view = view.clone();
            view.update(cx, |view, cx| view.refresh(cx));
        }
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active, window, cx);
    }

    fn on_open_file(&mut self, _: &OpenFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(cx);
    }

    fn on_save_file(&mut self, _: &SaveFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.save(self.active, false, cx);
    }

    fn on_save_file_as(&mut self, _: &SaveFileAs, _window: &mut Window, cx: &mut Context<Self>) {
        self.save(self.active, true, cx);
    }

    /// Ask for SQL files and give each one its own tab.
    fn open_file(&mut self, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_open(cx);

        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Some(paths)) => paths,
                Ok(None) => return,
                Err(error) => {
                    this.update(cx, |this, cx| this.report(error, cx)).ok();
                    return;
                }
            };

            for path in paths {
                match cx.background_spawn(sql_file::read(path.clone())).await {
                    Ok(sql) => {
                        this.update_in(cx, |this, window, cx| {
                            this.open_file_tab(path, sql, window, cx)
                        })
                        .ok();
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| this.report(error, cx)).ok();
                    }
                }
            }
        })
        .detach();
    }

    /// Show `sql` read from `path` in a new tab named after the file.
    fn open_file_tab(
        &mut self,
        path: PathBuf,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_tab(None, sql, false, window, cx);
        let index = self.active;
        self.set_status(index, Status::Done(format!("Opened {}", path.display())));
        self.set_file(index, path, cx);
    }

    /// Save the query tab at `index`.
    ///
    /// A tab with no file yet — or "Save As", with `ask` set — asks the
    /// platform for a path first.
    fn save(&mut self, index: usize, ask: bool, cx: &mut Context<Self>) {
        // A table tab has no buffer to save, so saving it writes its staged
        // edits instead. `TableView` binds the same key itself, which covers
        // the grid having focus; this covers everywhere else in the session.
        if let Some(TabContent::Table { view }) = self.tabs.get(index).map(|tab| &tab.content) {
            let view = view.clone();
            view.update(cx, |view, cx| view.commit(cx));
            return;
        }

        let Some(TabContent::Query { editor, path, .. }) =
            self.tabs.get(index).map(|tab| &tab.content)
        else {
            return;
        };
        let (editor, file) = (editor.clone(), path.clone());
        let sql = editor.read(cx).sql(cx);

        if let Some(path) = file.clone().filter(|_| !ask) {
            self.write(editor, path, sql, cx);
            return;
        }

        let prompt = sql_file::prompt_for_save(file.as_deref(), &self.tabs[index].title, cx);

        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                this.update(cx, |this, cx| this.write(editor, path, sql, cx))
                    .ok();
            }
            Ok(None) => {}
            Err(error) => {
                this.update(cx, |this, cx| this.report(error, cx)).ok();
            }
        })
        .detach();
    }

    /// Write `sql` to `path`, then bind the tab to it.
    fn write(
        &mut self,
        editor: Entity<QueryEditor>,
        path: PathBuf,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_spawn(sql_file::write(path.clone(), sql));

        cx.spawn(async move |this, cx| {
            let written = task.await;
            this.update(cx, |this, cx| {
                // The tab may have been closed or moved while the file was written.
                let Some(index) = this.query_tab_of(&editor) else {
                    return;
                };

                match written {
                    Ok(()) => {
                        this.set_status(index, Status::Done(format!("Saved {}", path.display())));
                        this.set_file(index, path, cx);
                    }
                    Err(error) => this.set_status(index, Status::Error(format!("{error:#}"))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Bind the query tab at `index` to `path`, naming the tab after the file.
    fn set_file(&mut self, index: usize, path: PathBuf, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let TabContent::Query { path: slot, .. } = &mut tab.content else {
            return;
        };

        tab.title = sql_file::label(&path).into();
        *slot = Some(path);
        cx.notify();
    }

    /// Put a file error where the user can see it.
    ///
    /// Only query tabs carry a status bar, so an error raised while a table
    /// tab is in front goes to the query tab nearest it.
    fn report(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        let Some(index) = self.nearest_query_tab() else {
            return;
        };
        self.set_status(index, Status::Error(format!("{error:#}")));
        cx.notify();
    }

    /// The active tab if it holds a query, else the next one that does.
    fn nearest_query_tab(&self) -> Option<usize> {
        (0..self.tabs.len())
            .map(|offset| (self.active + offset) % self.tabs.len())
            .find(|index| matches!(self.tabs[*index].content, TabContent::Query { .. }))
    }

    pub fn connection(&self) -> Arc<Connection> {
        self.connection.clone()
    }

    /// The connection's name, for the titlebar.
    pub(crate) fn display_name(&self) -> String {
        self.connection.config.display_name()
    }

    /// The connection's target (host, or file path), for the titlebar.
    pub(crate) fn display_target(&self) -> String {
        self.connection.config.display_target()
    }

    /// Put the caret in the active tab's editor.
    ///
    /// Called when the session is brought forward, so `Cmd+Enter` runs the
    /// query without having to click into the editor first.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabContent::Query { editor, .. }) =
            self.tabs.get(self.active).map(|tab| &tab.content)
        else {
            return;
        };

        let editor = editor.clone();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
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
    fn on_editor_event(
        &mut self,
        editor: &Entity<QueryEditor>,
        event: &QueryEditorEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A tab renders only while it is active, so its buttons act on it.
        let Some(index) = self.query_tab_of(editor) else {
            return;
        };

        match event {
            QueryEditorEvent::Run(sql) => self.run(index, sql.clone(), cx),
            QueryEditorEvent::RunScript(sql) => self.run_script(index, sql.clone(), cx),
            QueryEditorEvent::Open => self.open_file(cx),
            QueryEditorEvent::Save => self.save(index, false, cx),
        }
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

    /// Run `sql` for the tab at `index`, asking first where the connection
    /// says every write is confirmed.
    fn run(&mut self, index: usize, sql: String, cx: &mut Context<Self>) {
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.set_status(index, Status::Confirm(sql));
            cx.notify();
            return;
        }

        self.run_now(index, sql, cx);
    }

    /// Run every statement in `sql`, one after another.
    fn run_script(&mut self, index: usize, sql: String, cx: &mut Context<Self>) {
        // A script is confirmed as a whole: what the user is being asked about
        // is the buffer they are about to run.
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.set_status(index, Status::Confirm(sql));
            cx.notify();
            return;
        }

        self.send(index, sql, true, cx);
    }

    /// Run the statement that is waiting to be confirmed.
    fn confirm_run(&mut self, cx: &mut Context<Self>) {
        let index = self.active;
        let Some(TabContent::Query {
            status: Status::Confirm(sql),
            ..
        }) = self.tabs.get(index).map(|tab| &tab.content)
        else {
            return;
        };

        let sql = sql.clone();
        self.run_now(index, sql, cx);
    }

    /// Leave the statement unrun; the buffer is untouched either way.
    fn cancel_run(&mut self, cx: &mut Context<Self>) {
        let index = self.active;
        if !matches!(
            self.tabs.get(index).map(|tab| &tab.content),
            Some(TabContent::Query {
                status: Status::Confirm(_),
                ..
            })
        ) {
            return;
        }

        self.set_status(index, Status::Done("Not run".into()));
        cx.notify();
    }

    /// Send one statement for the tab at `index`.
    fn run_now(&mut self, index: usize, sql: String, cx: &mut Context<Self>) {
        self.send(index, sql, false, cx);
    }

    /// Send `sql` for the tab at `index`; its own grid and status follow it.
    ///
    /// A script comes back as one result per statement; a single statement
    /// comes back as one result, so both land in the same place.
    fn send(&mut self, index: usize, sql: String, script: bool, cx: &mut Context<Self>) {
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
        let task = runtime::spawn(async move {
            if script {
                connection.run_script(&sql).await
            } else {
                connection.run_query(&sql).await.map(|result| vec![result])
            }
        });
        self.set_running(index, task.abort_handle());

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // The tab may have been closed or moved while the query ran.
                let Some(index) = this.query_tab_of(&editor) else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));
                this.set_running(index, None);

                match result {
                    Ok(Ok(results)) => this.show_results(index, results, cx),
                    Ok(Err(error)) => {
                        this.set_status(index, Status::Error(format!("{error:#}")));
                        grid.update(cx, |grid, cx| grid.clear(cx));
                    }
                    // The sender is dropped when the run is given up on, which
                    // is what cancelling does.
                    Err(_) => this.set_status(index, Status::Done("Cancelled".into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Remember what is running in the tab at `index`, so it can be cancelled.
    fn set_running(&mut self, index: usize, handle: Option<tokio::task::AbortHandle>) {
        if let Some(TabContent::Query { running, .. }) =
            self.tabs.get_mut(index).map(|tab| &mut tab.content)
        {
            *running = handle;
        }
    }

    /// Put a finished run's results in the tab at `index`.
    fn show_results(&mut self, index: usize, results: Vec<QueryResult>, cx: &mut Context<Self>) {
        let Some(TabContent::Query {
            grid,
            results: slot,
            result,
            ..
        }) = self.tabs.get_mut(index).map(|tab| &mut tab.content)
        else {
            return;
        };

        let grid = grid.clone();
        *slot = results;
        *result = 0;

        let summary = self.result_summary(index);
        self.set_status(index, Status::Done(summary));

        // A statement that returned no rows at all — an `update`, say — leaves
        // the grid empty rather than showing the rows of the run before it.
        let first = match self.tabs.get(index).map(|tab| &tab.content) {
            Some(TabContent::Query { results, .. }) => results.first().cloned(),
            _ => None,
        };
        match first {
            Some(result) => grid.update(cx, |grid, cx| grid.set_result(result, cx)),
            None => grid.update(cx, |grid, cx| grid.clear(cx)),
        }
    }

    /// Show another of the results the last run produced.
    fn show_result(&mut self, index: usize, which: usize, cx: &mut Context<Self>) {
        let Some(TabContent::Query {
            grid,
            results,
            result,
            ..
        }) = self.tabs.get_mut(index).map(|tab| &mut tab.content)
        else {
            return;
        };

        let Some(chosen) = results.get(which).cloned() else {
            return;
        };
        let grid = grid.clone();
        *result = which;

        grid.update(cx, |grid, cx| grid.set_result(chosen, cx));
        let summary = self.result_summary(index);
        self.set_status(index, Status::Done(summary));
        cx.notify();
    }

    /// What the status bar says about the run that just finished.
    fn result_summary(&self, index: usize) -> String {
        let Some(TabContent::Query {
            results, result, ..
        }) = self.tabs.get(index).map(|tab| &tab.content)
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

    /// Give up on the run in the active tab.
    fn cancel_query(&mut self, _: &CancelQuery, _window: &mut Window, cx: &mut Context<Self>) {
        let index = self.active;
        let Some(TabContent::Query {
            editor,
            running,
            status,
            ..
        }) = self.tabs.get_mut(index).map(|tab| &mut tab.content)
        else {
            return;
        };

        if !matches!(status, Status::Running) {
            return;
        }

        let editor = editor.clone();
        if let Some(running) = running.take() {
            running.abort();
        }

        *status = Status::Done("Cancelled".into());
        editor.update(cx, |editor, cx| editor.set_running(false, cx));
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
                    .px_1()
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
                            .accessibility_label(format!("Close the tab {}", tab.title))
                            .tooltip_with_action("Close tab", &CloseTab, Some("Session"))
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
                    .mr_1()
                    .icon(IconName::Plus)
                    .accessibility_label("New query tab")
                    .tooltip_with_action("New query tab", &NewTab, Some("Session"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_tab(None, String::new(), false, window, cx)
                    })),
            )
    }

    /// The database dropdown, rendered by whoever owns the toolbar.
    ///
    /// Sized and styled here rather than by the caller: `dropdown_menu`
    /// wraps the button in a popover that does not itself implement
    /// [`Sizable`]/[`ButtonVariants`].
    pub(crate) fn render_database_picker(
        session: &Entity<Session>,
        cx: &mut gpui_kit::App,
    ) -> impl IntoElement {
        let this = session.read(cx);
        let current = this.connection.database().to_string();

        // A SQLite connection is one file: there is nothing to switch to, and
        // its "databases" (main, plus attachments) are not separate files.
        if this.connection.config.engine.is_file_based() {
            return Button::new("database")
                .outline()
                .xsmall()
                .max_w(px(160.))
                .label(crate::db::file_name(&current))
                .disabled(true)
                .into_any_element();
        }

        let databases = this.databases.clone();
        let weak = session.downgrade();

        let label = if this.switching {
            "Switching…".to_string()
        } else if current.is_empty() {
            "No database".to_string()
        } else {
            current.clone()
        };

        Button::new("database")
            .outline()
            .xsmall()
            .max_w(px(160.))
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _window, _cx| {
                if databases.is_empty() {
                    return menu.label("No databases");
                }

                for database in &databases {
                    let name = database.clone();
                    let weak = weak.clone();

                    menu = menu.item(
                        PopupMenuItem::new(database.clone())
                            .checked(*database == current)
                            .on_click(move |_, _window, cx| {
                                let name = name.clone();
                                if let Some(session) = weak.upgrade() {
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
            // The statement itself is in the editor above, so the bar only has
            // to carry the answer.
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
                                .on_click(cx.listener(|this, _, _window, cx| this.confirm_run(cx))),
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
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let active = this.active;
                        this.show_result(active, index, cx);
                    }));

                if index == shown {
                    button.primary()
                } else {
                    button.ghost()
                }
            }))
    }

    fn render_panes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.tabs[self.active].content {
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
                                        // statement; a single query leaves one,
                                        // and the bar for it would say nothing.
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
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_save_file))
            .on_action(cx.listener(Self::on_save_file_as))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::cancel_query))
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
