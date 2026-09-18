//! An open connection: object sidebar, and a dock of query, table and
//! structure panels, split and reordered by dragging their tabs.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    DockArea, DockLayout, DockPlacement, DockSkin, InsertTarget, NodeId, PanelId, PanelStyle,
    panel_handle,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tree::TreeState;
use gpui_kit::component::{
    ActiveTheme, Disableable, ResizableState, Sizable, h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, Focusable, SharedString, WeakEntity, Window, actions, div,
    px,
};

use regex::Regex;

use crate::db::{Connection, DatabaseObject, runtime, statement};
use crate::ui::filter_bar::FilterSpec;
use crate::ui::schema_view::SchemaView;
use crate::ui::sql_file;
use crate::ui::table_view::{TableView, TableViewEvent};

mod panel;
mod sidebar;
pub(crate) mod tab;
#[cfg(test)]
mod test_support;

use panel::{SessionPanel, SessionPanelEvent};
use tab::{ObjectViewMode, Status};

/// Starting pane sizes; the user drags from here.
const SIDEBAR_WIDTH: f32 = 260.;

actions!(
    zippa_db,
    [
        NewTab,
        CloseTab,
        OpenFile,
        SaveFile,
        SaveFileAs,
        Refresh,
        CancelQuery,
        QuickSwitcher,
    ]
);

pub enum SessionEvent {
    /// The user closed the session; the workspace returns to the connection
    /// manager and drains the pool.
    Disconnected,
}

pub struct Session {
    connection: Arc<Connection>,
    dock: Entity<DockArea>,
    /// Every panel this session owns, in creation order. The dock owns where
    /// they are; this owns which ones exist.
    panels: Vec<Entity<SessionPanel>>,
    /// The panel the user is in — with a split open, several panels are
    /// displayed at once, so "active" means where focus last was.
    active: Option<WeakEntity<SessionPanel>>,
    /// Numbers the untitled tabs, so closing one does not reuse its name.
    opened: usize,
    /// Sidebar beside the editor and grid.
    columns: Entity<ResizableState>,
    databases: Vec<String>,
    objects: Vec<DatabaseObject>,
    /// Text filter over the object list.
    filter: Entity<InputState>,
    /// Compiled form of the filter; `None` means "show everything".
    matcher: Option<Regex>,
    /// The sidebar's object list; its selection follows the active tab's
    /// table, cleared for a query tab.
    objects_tree: Entity<TreeState>,
    /// Set when the schema could not be read; queries still work.
    metadata_error: Option<String>,
    switching: bool,
    /// A tab close waiting on an answer, because the buffer it would lose has
    /// unsaved changes.
    closing: Option<WeakEntity<SessionPanel>>,
    /// The next panel's stable key. See `SessionPanel`'s `key` field.
    next_key: usize,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables"));
        cx.subscribe_in(&filter, window, Self::on_filter_event)
            .detach();

        // A per-connection id: every workspace tab builds its own dock.
        let (dock, skin) = DockSkin::dock_area(
            SharedString::from(format!("session-dock-{}", connection.config.id)),
            None,
            window,
            cx,
        );
        // A lone tab still gets a strip, so its close button and `+` are
        // always there.
        skin.set_panel_style(PanelStyle::TabBar, cx);
        dock.update(cx, |dock, cx| {
            dock.set_center(DockLayout::tabs(), window, cx)
        });

        let mut session = Self {
            connection,
            dock,
            panels: Vec::new(),
            active: None,
            opened: 0,
            columns: cx.new(|_| ResizableState::default()),
            databases: Vec::new(),
            objects: Vec::new(),
            filter,
            matcher: None,
            objects_tree: cx.new(|cx| TreeState::new(cx)),
            metadata_error: None,
            switching: false,
            closing: None,
            next_key: 0,
        };
        session.open_tab(None, String::new(), false, window, cx);
        session.reload_metadata(cx);
        session
    }

    /// The one place a panel joins the session.
    fn install(
        &mut self,
        panel: Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(&panel, window, Self::on_panel_event)
            .detach();

        // `add_panel_view` appends to the *first* group in the region, so
        // with a split open a new tab would land beside the wrong one; the
        // move afterward puts it beside the group the user is looking at.
        let target = self.active_group(cx);
        self.dock.update(cx, |dock, cx| {
            dock.add_panel_view(
                panel_handle(panel.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            if let Some(node) = target {
                dock.move_panel(
                    PanelId::from(panel.entity_id()),
                    InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    window,
                    cx,
                );
            }
        });

        self.panels.push(panel.clone());
        self.active = Some(panel.downgrade());
        self.sync_tree_selection(cx);
        cx.notify();
    }

    /// The node of the tab group holding the active panel — where a new tab
    /// goes.
    fn active_group(&self, cx: &App) -> Option<NodeId> {
        let panel = self.active_panel()?;
        let group = panel.read(cx).group()?.upgrade()?;
        Some(group.read(cx).node())
    }

    /// The panel the user was last focused in, if it is still open.
    fn active_panel(&self) -> Option<Entity<SessionPanel>> {
        self.active.as_ref()?.upgrade()
    }

    /// The panel waiting on an answer about its unsaved changes, if any.
    fn closing_panel(&self) -> Option<Entity<SessionPanel>> {
        self.closing.as_ref()?.upgrade()
    }

    /// `active` moves here, and only here.
    fn touch(&mut self, panel: &Entity<SessionPanel>, cx: &mut Context<Self>) {
        self.active = Some(panel.downgrade());
        self.sync_tree_selection(cx);
        cx.notify();
    }

    /// A panel left the dock — by the ✕, or by the `…` menu's Close, which
    /// never asks the session first. Idempotent: the ✕ path has already done
    /// this by the time the event arrives.
    fn forget(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.panels.iter().any(|p| p == panel) {
            return;
        }

        self.panels.retain(|p| p != panel);
        if self.closing.as_ref().is_some_and(|w| w == panel) {
            self.closing = None;
        }
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = self.panels.last().map(|p| p.downgrade());
        }

        if self.panels.is_empty() {
            // The session always shows one editor.
            self.open_tab(None, String::new(), false, window, cx);
            return;
        }

        self.sync_tree_selection(cx);
        cx.notify();
    }

    /// The hub every panel event passes through.
    fn on_panel_event(
        &mut self,
        panel: &Entity<SessionPanel>,
        event: &SessionPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionPanelEvent::Focused => self.touch(panel, cx),
            SessionPanelEvent::Removed => self.forget(panel, window, cx),
            SessionPanelEvent::CloseRequested => self.close_tab(panel, window, cx),
            SessionPanelEvent::NewTabRequested => {
                self.touch(panel, cx);
                self.open_tab(None, String::new(), false, window, cx);
            }
            SessionPanelEvent::Run(sql) => self.run(panel, sql.clone(), cx),
            SessionPanelEvent::RunScript(sql) => self.run_script(panel, sql.clone(), cx),
            SessionPanelEvent::ConfirmRun(sql) => self.run_now(panel, sql.clone(), cx),
            SessionPanelEvent::OpenFile => self.open_file(cx),
            SessionPanelEvent::Save => self.save(panel, false, cx),
        }
    }

    /// Add a tab and make it active.
    ///
    /// `title` defaults to a running "Query N"; `run` executes `sql` right
    /// away, which is how the sidebar opens a table.
    pub(crate) fn open_tab(
        &mut self,
        title: Option<String>,
        sql: String,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SessionPanel> {
        self.opened += 1;
        let title = title.unwrap_or_else(|| format!("Query {}", self.opened));
        let key = self.next_key;
        self.next_key += 1;

        let panel = cx.new(|cx| SessionPanel::query(key, title, sql.clone(), window, cx));
        self.install(panel.clone(), window, cx);

        if run && !sql.trim().is_empty() {
            self.run(&panel, sql, cx);
        }
        cx.notify();
        panel
    }

    /// Open a table or view from the sidebar in its own tab, either as its
    /// rows or its own structure.
    ///
    /// The same object already open in the same mode is brought forward
    /// rather than opened twice; a table's data tab and its structure tab are
    /// different tabs.
    pub(crate) fn open_object(
        &mut self,
        object: &DatabaseObject,
        mode: ObjectViewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| {
                let panel = panel.read(cx);
                panel.mode() == Some(mode) && panel.object(cx).as_ref() == Some(object)
            })
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;

        let panel = match mode {
            ObjectViewMode::Data => {
                let view = cx
                    .new(|cx| TableView::new(self.connection.clone(), object.clone(), window, cx));
                cx.subscribe_in(&view, window, Self::on_table_navigate)
                    .detach();
                let title = object.label();
                cx.new(|cx| SessionPanel::table(key, title, view, cx))
            }
            ObjectViewMode::Schema => {
                let view = cx
                    .new(|cx| SchemaView::new(self.connection.clone(), object.clone(), window, cx));
                let title = format!("{} — Structure", object.label());
                cx.new(|cx| SessionPanel::schema(key, title, view, cx))
            }
        };

        self.install(panel, window, cx);
    }

    /// A table view followed a foreign key; open (or reuse) the referenced
    /// table's tab and put the matching row's filter on it.
    fn on_table_navigate(
        &mut self,
        _: &Entity<TableView>,
        event: &TableViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableViewEvent::NavigateToForeignKey { object, filter } => {
                self.open_object_filtered(object, filter.clone(), window, cx);
            }
        }
    }

    /// Open or reuse `object`'s data tab, the way [`Self::open_object`] does,
    /// then replace its filters with `filter` — a table already open is
    /// re-filtered and brought forward rather than left showing whatever it
    /// had.
    fn open_object_filtered(
        &mut self,
        object: &DatabaseObject,
        filter: FilterSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_object(object, ObjectViewMode::Data, window, cx);

        let target = self
            .panels
            .iter()
            .find(|panel| {
                let panel = panel.read(cx);
                panel.mode() == Some(ObjectViewMode::Data)
                    && panel.object(cx).as_ref() == Some(object)
            })
            .and_then(|panel| panel.read(cx).table_view());
        let Some(view) = target else {
            return;
        };
        view.update(cx, |view, cx| {
            view.apply_external_filter(filter, window, cx)
        });
    }

    /// Close a tab, asking first if it holds unsaved changes.
    fn close_tab(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.panels.iter().any(|p| p == panel) {
            return;
        }

        if panel.read(cx).is_dirty(cx) {
            self.closing = Some(panel.downgrade());
            cx.notify();
            return;
        }

        self.close_tab_now(panel, window, cx);
    }

    /// Leave the tab open; the buffer it would have lost is untouched either
    /// way.
    fn cancel_close(&mut self, cx: &mut Context<Self>) {
        self.closing = None;
        cx.notify();
    }

    /// Close the tab waiting on an answer about its unsaved changes.
    fn close_confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.closing.take().and_then(|panel| panel.upgrade()) else {
            return;
        };
        self.close_tab_now(&panel, window, cx);
    }

    /// Remove a tab outright; the caller has already decided unsaved changes
    /// do not matter.
    fn close_tab_now(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.panels.iter().any(|p| p == panel) {
            return;
        }

        if self.panels.len() == 1 {
            // The session always shows one editor; opening it first, before
            // this one leaves, means it lands in the same group.
            self.open_tab(None, String::new(), false, window, cx);
        }

        self.panels.retain(|p| p != panel);
        if self.closing.as_ref().is_some_and(|w| w == panel) {
            self.closing = None;
        }
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = self.panels.last().map(|p| p.downgrade());
        }

        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel.clone(), window, cx));

        self.sync_tree_selection(cx);
        cx.notify();
    }

    pub(crate) fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panels.get(index).cloned() else {
            return;
        };
        self.activate_panel(&panel, window, cx);
    }

    fn activate_panel(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        SessionPanel::bring_forward(panel, window, cx);
        self.touch(panel, cx);
    }

    pub(crate) fn panels(&self) -> &[Entity<SessionPanel>] {
        &self.panels
    }

    /// Whether any tab in this connection has a buffer that would be lost by
    /// closing it, so the workspace knows to ask before closing the whole
    /// connection.
    pub(crate) fn has_unsaved_changes(&self, cx: &App) -> bool {
        self.panels.iter().any(|panel| panel.read(cx).is_dirty(cx))
    }

    pub(crate) fn active_tab_index(&self) -> usize {
        let Some(active) = self.active.as_ref() else {
            return 0;
        };
        self.panels.iter().position(|p| p == active).unwrap_or(0)
    }

    pub(crate) fn objects(&self) -> &[DatabaseObject] {
        &self.objects
    }

    pub(crate) fn databases(&self) -> &[String] {
        &self.databases
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
        self.reload_active_table(cx);
    }

    pub(crate) fn reload_active_table(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel() {
            panel.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    fn on_quick_switcher(
        &mut self,
        _: &QuickSwitcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().clone();
        crate::ui::quick_switcher::open(session, window, cx);
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel() {
            self.close_tab(&panel, window, cx);
        }
    }

    fn on_open_file(&mut self, _: &OpenFile, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(cx);
    }

    fn on_save_file(&mut self, _: &SaveFile, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel() {
            self.save(&panel, false, cx);
        }
    }

    fn on_save_file_as(&mut self, _: &SaveFileAs, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel() {
            self.save(&panel, true, cx);
        }
    }

    /// Ask for SQL files and give each one its own tab.
    pub(crate) fn open_file(&mut self, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_open(cx);

        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Some(paths)) => paths,
                Ok(None) => return,
                Err(error) => {
                    this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                        .ok();
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
                        this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                            .ok();
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
        let panel = self.open_tab(None, sql, false, window, cx);
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Done(format!("Opened {}", path.display())));
            panel.set_file(path, cx);
        });
    }

    /// Save `panel`'s query tab.
    ///
    /// A tab with no file yet — or "Save As", with `ask` set — asks the
    /// platform for a path first.
    fn save(&mut self, panel: &Entity<SessionPanel>, ask: bool, cx: &mut Context<Self>) {
        // A table tab has no buffer to save, so saving it writes its staged
        // edits instead. `TableView` binds the same key itself, which covers
        // the grid having focus; this covers everywhere else in the session.
        if panel.read(cx).table_view().is_some() {
            panel.update(cx, |panel, cx| panel.commit(cx));
            return;
        }

        let Some(editor) = panel.read(cx).editor() else {
            return;
        };
        let file = panel.read(cx).file_path();
        let sql = editor.read(cx).sql(cx);

        if let Some(path) = file.clone().filter(|_| !ask) {
            self.write(panel.downgrade(), path, sql, cx);
            return;
        }

        let title = panel.read(cx).title();
        let prompt = sql_file::prompt_for_save(file.as_deref(), &title, cx);
        let weak = panel.downgrade();

        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                this.update(cx, |this, cx| this.write(weak, path, sql, cx))
                    .ok();
            }
            Ok(None) => {}
            Err(error) => {
                this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Write `sql` to `path`, then bind the tab to it.
    fn write(
        &mut self,
        panel: WeakEntity<SessionPanel>,
        path: PathBuf,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_spawn(sql_file::write(path.clone(), sql.clone()));

        cx.spawn(async move |this, cx| {
            let written = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the file was written.
                let Some(panel) = panel.upgrade() else {
                    return;
                };

                let error = match written {
                    Ok(()) => None,
                    Err(error) => Some(format!("{error:#}")),
                };
                panel.update(cx, |panel, cx| {
                    match &error {
                        None => {
                            panel.set_status(Status::Done(format!("Saved {}", path.display())));
                            panel.set_file(path, cx);
                            panel.set_baseline(sql);
                        }
                        Some(error) => panel.set_status(Status::Error(error.clone())),
                    }
                    cx.notify();
                });
                if let Some(error) = error {
                    crate::ui::notify_error(window, cx, format!("Error: {error}"));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Put a file error where the user can see it.
    ///
    /// Only query tabs carry a status bar, so an error raised while a table
    /// tab is in front goes to the query tab nearest it.
    fn report(&mut self, error: anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.nearest_query_panel(cx) else {
            return;
        };
        let message = format!("{error:#}");
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Error(message.clone()));
            cx.notify();
        });
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
    }

    /// The active panel if it holds a query, else the next one that does.
    fn nearest_query_panel(&self, cx: &App) -> Option<Entity<SessionPanel>> {
        if self.panels.is_empty() {
            return None;
        }
        let start = self.active_tab_index();
        (0..self.panels.len())
            .map(|offset| (start + offset) % self.panels.len())
            .map(|index| &self.panels[index])
            .find(|panel| panel.read(cx).is_query())
            .cloned()
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
        let Some(panel) = self.active_panel() else {
            return;
        };
        panel.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// Read the database list and the current database's tables and views.
    pub(crate) fn reload_metadata(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let task =
            runtime::spawn(
                async move { (connection.databases().await, connection.objects().await) },
            );

        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update_in(cx, |this, window, cx| {
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
                if let Some(error) = &this.metadata_error {
                    crate::ui::notify_error(window, cx, format!("Error: {error}"));
                }
                this.rebuild_tree(cx);
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
            this.update_in(cx, |this, window, cx| {
                this.switching = false;
                match opened {
                    Ok(Ok(connection)) => {
                        let previous =
                            std::mem::replace(&mut this.connection, Arc::new(connection));
                        runtime::spawn(async move { previous.close().await });

                        this.objects.clear();
                        this.rebuild_tree(cx);
                        let connection = this.connection.clone();
                        for panel in this.panels.clone() {
                            let connection = connection.clone();
                            panel.update(cx, |panel, cx| panel.set_connection(connection, cx));
                        }
                        this.reload_metadata(cx);
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        if let Some(panel) = this.active_panel() {
                            panel.update(cx, |panel, _| {
                                panel.set_status(Status::Error(message.clone()))
                            });
                        }
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                    Err(_) => {
                        let message = "switching database was cancelled";
                        if let Some(panel) = this.active_panel() {
                            panel.update(cx, |panel, _| {
                                panel.set_status(Status::Error(message.into()))
                            });
                        }
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run `sql` for `panel`, asking first where the connection says every
    /// write is confirmed.
    fn run(&mut self, panel: &Entity<SessionPanel>, sql: String, cx: &mut Context<Self>) {
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            panel.update(cx, |panel, cx| {
                panel.set_status(Status::Confirm(sql));
                cx.notify();
            });
            return;
        }

        self.run_now(panel, sql, cx);
    }

    /// Run every statement in `sql`, one after another.
    fn run_script(&mut self, panel: &Entity<SessionPanel>, sql: String, cx: &mut Context<Self>) {
        // A script is confirmed as a whole: what the user is being asked
        // about is the buffer they are about to run.
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            panel.update(cx, |panel, cx| {
                panel.set_status(Status::Confirm(sql));
                cx.notify();
            });
            return;
        }

        self.send(panel, sql, true, cx);
    }

    /// Send one statement for `panel`.
    fn run_now(&mut self, panel: &Entity<SessionPanel>, sql: String, cx: &mut Context<Self>) {
        self.send(panel, sql, false, cx);
    }

    /// Send `sql` for `panel`; its own grid and status follow it.
    ///
    /// A script comes back as one result per statement; a single statement
    /// comes back as one result, so both land in the same place.
    fn send(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        script: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, grid)) = panel.read(cx).query_parts() else {
            return;
        };

        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));

        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            if script {
                connection.run_script(&sql).await
            } else {
                connection.run_query(&sql).await.map(|result| vec![result])
            }
        });
        panel.update(cx, |panel, _| panel.set_running(task.abort_handle()));

        let weak = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the query ran.
                let Some(panel) = weak.upgrade() else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));
                panel.update(cx, |panel, cx| {
                    panel.set_running(None);

                    match result {
                        Ok(Ok(results)) => panel.show_results(results, cx),
                        Ok(Err(error)) => {
                            let message = format!("{error:#}");
                            panel.set_status(Status::Error(message.clone()));
                            grid.update(cx, |grid, cx| grid.clear(cx));
                            crate::ui::notify_error(window, cx, format!("Error: {message}"));
                        }
                        // The sender is dropped when the run is given up on,
                        // which is what cancelling does.
                        Err(_) => panel.set_status(Status::Done("Cancelled".into())),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Give up on the run in the active tab.
    fn cancel_query(&mut self, _: &CancelQuery, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.active_panel() {
            panel.update(cx, |panel, cx| {
                panel.cancel_running(cx);
            });
        }
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

    /// The bar asking whether to close a tab with unsaved changes.
    fn render_close_confirm(
        &self,
        panel: &Entity<SessionPanel>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = panel.read(cx).title();

        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_1()
            .gap_3()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(format!("\"{title}\" has unsaved changes.")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("keep-tab")
                            .ghost()
                            .xsmall()
                            .label("Keep tab")
                            .tooltip("Leave the tab open")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_close(cx))),
                    )
                    .child(
                        Button::new("close-tab-anyway")
                            .danger()
                            .xsmall()
                            .label("Close Without Saving")
                            .tooltip("Throw the changes away and close the tab")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_confirmed(window, cx)),
                            ),
                    ),
            )
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
            .on_action(cx.listener(Self::on_quick_switcher))
            .child(
                h_resizable("session-columns")
                    .with_state(&self.columns)
                    .child(
                        resizable_panel()
                            .size(px(SIDEBAR_WIDTH))
                            .size_range(px(180.)..px(560.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .when_some(self.closing_panel(), |this, panel| {
                                    this.child(self.render_close_confirm(&panel, cx))
                                })
                                .child(div().flex_1().min_h_0().child(self.dock.clone())),
                        ),
                    ),
            )
    }
}
