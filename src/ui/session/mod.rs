//! An open connection: object sidebar, and a dock of query, table and
//! structure panels, split and reordered by dragging their tabs.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::dock::{
    DockArea, DockLayout, DockPlacement, DockSkin, InsertTarget, NodeId, PanelId, PanelStyle,
    panel_handle,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tree::TreeState;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, ResizableState, Sizable, WindowExt, h_flex, h_resizable,
    resizable_panel,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, Focusable, SharedString, WeakEntity, Window, actions, div,
    px,
};

use regex::Regex;

use crate::db::{
    Catalog, CatalogEntry, CatalogKind, Connection, DatabaseObject, Explained, StoredObject,
    runtime, statement,
};
use crate::ui::filter_bar::FilterSpec;
use crate::ui::import_dialog::{ImportEvent, ImportView};
use crate::ui::schema_view::SchemaView;
use crate::ui::sql_file;
use crate::ui::table_view::{TableView, TableViewEvent};
use crate::workspace_state::{PanelState, SessionState};

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
        NextTab,
        PreviousTab,
        OpenFile,
        SaveFile,
        SaveFileAs,
        Refresh,
        CancelQuery,
        QuickSwitcher,
        ImportSqlDump,
        SearchSchema,
    ]
);

pub enum SessionEvent {
    /// Something about the session's tabs or its connection changed in a way
    /// the workspace's saved state should record.
    Changed,
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
    /// Functions, procedures and sequences, listed after the tables.
    stored: Vec<StoredObject>,
    /// Text filter over the object list.
    filter: Entity<InputState>,
    /// Compiled form of the filter; `None` means "show everything".
    matcher: Option<Regex>,
    /// The sidebar's object list; its selection follows the active tab's
    /// table, cleared for a query tab.
    objects_tree: Entity<TreeState>,
    /// Set when the schema could not be read; queries still work.
    metadata_error: Option<String>,
    /// The whole schema, for [`SearchSchema`]. Read once in the background as
    /// the session opens and again on refresh, so a keystroke never waits on
    /// the server. Holds only names until the read lands.
    catalog: Arc<Catalog>,
    /// Whether that read is still in flight, so the dialog can say so.
    catalog_loading: bool,
    /// Why the full catalog could not be read, when it could not; search then
    /// finds names but not columns, indexes or triggers.
    catalog_error: Option<String>,
    switching: bool,
    /// The next panel's stable key. See `SessionPanel`'s `key` field.
    next_key: usize,
    /// The import dialog, while one is open over the window.
    import: Option<Entity<ImportView>>,
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
            stored: Vec::new(),
            filter,
            matcher: None,
            objects_tree: cx.new(|cx| TreeState::new(cx)),
            metadata_error: None,
            catalog: Arc::new(Catalog::default()),
            catalog_loading: true,
            catalog_error: None,
            switching: false,
            next_key: 0,
            import: None,
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
        cx.emit(SessionEvent::Changed);
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

    /// `active` moves here, and only here.
    fn touch(&mut self, panel: &Entity<SessionPanel>, cx: &mut Context<Self>) {
        self.active = Some(panel.downgrade());
        self.sync_tree_selection(cx);
        cx.emit(SessionEvent::Changed);
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
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = self.panels.last().map(|p| p.downgrade());
        }

        if self.panels.is_empty() {
            // The session always shows one editor.
            self.open_tab(None, String::new(), false, window, cx);
            return;
        }

        self.sync_tree_selection(cx);
        cx.emit(SessionEvent::Changed);
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
            SessionPanelEvent::Run(sql) => self.run(panel, sql.clone(), window, cx),
            SessionPanelEvent::RunScript(sql) => self.run_script(panel, sql.clone(), window, cx),
            SessionPanelEvent::Explain { sql, analyze } => {
                self.explain(panel, sql.clone(), *analyze, window, cx)
            }
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

        let panel = cx.new(|cx| {
            SessionPanel::query(
                key,
                title,
                sql.clone(),
                self.connection.config.engine,
                window,
                cx,
            )
        });
        self.install(panel.clone(), window, cx);

        if run && !sql.trim().is_empty() {
            self.run(&panel, sql, window, cx);
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
            self.confirm_close(panel, window, cx);
            return;
        }

        self.close_tab_now(panel, window, cx);
    }

    /// Ask before throwing away a dirty buffer.
    ///
    /// A dialog rather than a bar, so the question is about the tab alone and
    /// the answer closes over the tab it is about — the tab cannot move out
    /// from under it while the user reads it.
    fn confirm_close(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = panel.read(cx).title();
        let session = cx.entity().downgrade();
        let panel = panel.clone();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();

            alert
                .title(format!("Close \"{title}\" without saving?"))
                .description("The tab has unsaved changes.")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Close Without Saving")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Keep Open")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| session.close_tab_now(&panel, window, cx));
                    }
                    true
                })
        });
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
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = self.panels.last().map(|p| p.downgrade());
        }

        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel.clone(), window, cx));

        self.sync_tree_selection(cx);
        cx.emit(SessionEvent::Changed);
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

    /// This session as it would be restored: the connection it belongs to, the
    /// database it is on, and every tab in creation order.
    pub(crate) fn snapshot(&self, cx: &App) -> SessionState {
        SessionState {
            connection: self.connection.config.id,
            database: Some(self.connection.database().to_string()),
            active: self.active_tab_index(),
            panels: self
                .panels
                .iter()
                .map(|panel| panel.read(cx).snapshot(cx))
                .collect(),
        }
    }

    /// Rebuild the tabs this session had, in order.
    ///
    /// Restoring goes through the same constructors a manual open does, so
    /// panel keys, `opened` numbering, and the dock all stay consistent. A
    /// restored buffer is not run; a restored table loads its first page the
    /// way opening it from the sidebar does.
    pub(crate) fn restore(
        &mut self,
        state: SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `Session::new` always opens one empty editor; it is closed again
        // below once there is something restored to take its place.
        let placeholder = self.panels.first().cloned();
        let mut restored = 0;
        // Restored query tabs whose file is read to decide whether the buffer is
        // dirty. The reads happen after the loop, off the UI thread.
        let mut file_backed: Vec<(Entity<SessionPanel>, PathBuf)> = Vec::new();

        for panel in state.panels {
            match panel {
                PanelState::Query { title, sql, file } => {
                    let panel = self.open_tab(Some(title), sql.clone(), false, window, cx);
                    if let Some(path) = file {
                        panel.update(cx, |panel, cx| panel.set_file(path.clone(), cx));
                        file_backed.push((panel, path));
                    }
                    restored += 1;
                }
                PanelState::Table { object } => {
                    self.open_object(&object, ObjectViewMode::Data, window, cx);
                    restored += 1;
                }
                PanelState::Schema { object } => {
                    self.open_object(&object, ObjectViewMode::Schema, window, cx);
                    restored += 1;
                }
                // Written by a newer build; leave it out rather than guess.
                PanelState::Unknown => {}
            }
        }

        // The saved buffer wins: re-reading the file could replace unsaved text.
        // The file is only read to decide whether the restored buffer matches it,
        // so an unsaved buffer shows as dirty and is asked about before closing.
        // Reading it is blocking I/O, so it goes to the background executor and
        // folds in when it lands; a file that cannot be read is reported rather
        // than left to look like an empty one.
        if !file_backed.is_empty() {
            let paths: Vec<PathBuf> = file_backed.iter().map(|(_, path)| path.clone()).collect();
            let reads = cx.background_spawn(async move {
                paths
                    .into_iter()
                    .map(|path| std::fs::read_to_string(&path).ok())
                    .collect::<Vec<_>>()
            });
            cx.spawn(async move |_this, cx| {
                let texts = reads.await;
                for ((panel, path), text) in file_backed.into_iter().zip(texts) {
                    panel.update(cx, |panel, _cx| match text {
                        Some(text) => panel.set_baseline(text),
                        None => {
                            panel.set_baseline(String::new());
                            panel.set_status(Status::Error(format!(
                                "could not read {}",
                                path.display()
                            )));
                        }
                    });
                }
            })
            .detach();
        }

        if restored > 0
            && let Some(placeholder) = placeholder
        {
            // Opening the restored tabs first and closing the placeholder
            // after keeps the "a session always shows one editor" rule. That
            // shifts the restored tabs down one, which is the order they were
            // saved in, so the saved active index applies directly.
            self.close_tab_now(&placeholder, window, cx);
        }

        self.activate_tab(state.active, window, cx);

        if let Some(database) = state.database
            && database != self.connection.database()
        {
            self.switch_database(database, cx);
        }
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

    /// Read the active tab's statement plan, for a caller that is not the
    /// editor's own shortcut — the quick switcher's Explain items.
    ///
    /// The editor is asked the way its own buttons ask it, so an `ANALYZE`
    /// still goes through the confirmation a careful connection wants, and the
    /// plan lands in the tab exactly as the shortcut would put it there. A tab
    /// that has no editor, or an empty one, has nothing to explain.
    pub(crate) fn explain_active(&mut self, analyze: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.active_panel() else {
            return;
        };
        let Some(editor) = panel.read(cx).editor() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.emit_explain(analyze, cx));
    }

    fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(1, window, cx);
    }

    fn on_previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(-1, window, cx);
    }

    /// Move `delta` tabs along the panels, wrapping at either end.
    fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.panels.len() as isize;
        if count < 2 {
            return;
        }

        let next = (self.active_tab_index() as isize + delta).rem_euclid(count);
        self.activate_tab(next as usize, window, cx);
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

    fn on_search_schema(&mut self, _: &SearchSchema, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.entity().clone();
        crate::ui::schema_search::open(session, window, cx);
    }

    /// Open what a schema search result is about.
    ///
    /// A table or view opens its rows; a column opens its table's rows with
    /// the column selected; an index or trigger opens the table's structure.
    /// There is no routine view yet, so a routine copies its callable name the
    /// way the sidebar's routine menu does.
    pub(crate) fn open_catalog_entry(
        &mut self,
        entry: CatalogEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match entry.kind {
            CatalogKind::Table | CatalogKind::View => {
                if let Some(object) = entry.owner() {
                    self.open_object(object, ObjectViewMode::Data, window, cx);
                }
            }
            CatalogKind::Column => {
                let Some(object) = entry.owner() else {
                    return;
                };
                self.open_object(object, ObjectViewMode::Data, window, cx);
                if let Some(view) = self.table_view_for(object, cx) {
                    view.update(cx, |view, cx| view.reveal_column(&entry.name, cx));
                }
            }
            CatalogKind::Index | CatalogKind::Trigger => {
                if let Some(object) = entry.owner() {
                    self.open_object(object, ObjectViewMode::Schema, window, cx);
                }
            }
            CatalogKind::Routine => {
                if let Some(routine) = &entry.routine {
                    let name = routine.label();
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(name.clone()));
                    crate::ui::notify_info(window, cx, format!("Copied {name}"));
                }
            }
        }
    }

    /// The open data tab's view for `object`, if it has one.
    fn table_view_for(
        &self,
        object: &DatabaseObject,
        cx: &App,
    ) -> Option<Entity<crate::ui::table_view::TableView>> {
        self.panels
            .iter()
            .find(|panel| {
                let panel = panel.read(cx);
                panel.mode() == Some(ObjectViewMode::Data)
                    && panel.object(cx).as_ref() == Some(object)
            })
            .and_then(|panel| panel.read(cx).table_view())
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

    fn on_import_dump(&mut self, _: &ImportSqlDump, _window: &mut Window, cx: &mut Context<Self>) {
        self.import_dump(cx);
    }

    /// Ask for a dump file, then open the import dialog over the session.
    pub(crate) fn import_dump(&mut self, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_import(cx);

        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                this.update_in(cx, |this, window, cx| {
                    this.open_import_dialog(path, window, cx)
                })
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

    /// Build the import dialog for `path`, if one is not already open.
    fn open_import_dialog(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.import.is_some() {
            return;
        }

        let view = cx.new(|cx| ImportView::new(self.connection.clone(), path, window, cx));
        cx.subscribe_in(&view, window, Self::on_import_event)
            .detach();

        let session = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let session = session.clone();
            dialog
                .title("Import SQL dump")
                .w(px(560.))
                .h(px(460.))
                // The dialog's own keys are off so `enter` cannot dismiss it;
                // `escape` is bound to `CloseImport` on the body instead.
                .keyboard(false)
                .on_close(move |_, _window, cx| {
                    session
                        .update(cx, |this, cx| {
                            this.import = None;
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        // Put the keyboard on the body so `escape` reaches `CloseImport`.
        view.update(cx, |view, cx| view.focus(window, cx));
        self.import = Some(view);
    }

    /// The dialog's own buttons: dismiss it, or note that the run changed the
    /// schema.
    fn on_import_event(
        &mut self,
        _: &Entity<ImportView>,
        event: &ImportEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ImportEvent::Dismissed => {
                self.import = None;
                window.close_dialog(cx);
                cx.notify();
            }
            ImportEvent::Finished => {
                // The import wrote objects and rows; the sidebar and any open
                // table are stale.
                self.refresh(cx);
            }
        }
    }

    /// Give up on an import in flight, the way [`Self::cancel_query`] gives up
    /// on a query.
    pub(crate) fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.import.clone() {
            view.update(cx, |view, cx| view.cancel(cx));
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
        let prompt = sql_file::prompt_for_save(file.as_deref(), &title, "sql", cx);
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
                // A save changes the file binding and the clean state, both of
                // which the saved workspace records.
                cx.emit(SessionEvent::Changed);
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

    /// Put the caret in the active tab's editor, or on its plan when that is
    /// what the tab is showing.
    ///
    /// Called when the session is brought forward, so `Cmd+Enter` runs the
    /// query without having to click into the editor first.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.active_panel() else {
            return;
        };
        match panel.read(cx).active_plan() {
            Some(plan) => plan.update(cx, |plan, cx| plan.focus(window, cx)),
            None => {
                let handle = panel.read(cx).focus_handle(cx);
                handle.focus(window, cx);
            }
        }
    }

    /// Read the database list and the current database's tables and views.
    pub(crate) fn reload_metadata(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            (
                connection.databases().await,
                connection.objects().await,
                connection.stored_objects().await,
                connection.catalog().await,
            )
        });

        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update_in(cx, |this, window, cx| {
                this.metadata_error = None;
                match loaded {
                    Ok((databases, objects, stored, catalog)) => {
                        match databases {
                            Ok(databases) => this.databases = databases,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        match objects {
                            Ok(objects) => this.objects = objects,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        match stored {
                            Ok(stored) => this.stored = stored,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        this.store_catalog(catalog);
                    }
                    Err(_) => {
                        this.metadata_error = Some("reading the schema was cancelled".into());
                        this.catalog_loading = false;
                    }
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

    /// Keep the catalog read's answer, falling back to the names the sidebar
    /// already has when the full read failed — a database whose column query
    /// times out should still be searchable by name.
    fn store_catalog(&mut self, catalog: Result<Catalog, anyhow::Error>) {
        self.catalog_loading = false;
        match catalog {
            Ok(catalog) => {
                self.catalog = Arc::new(catalog);
                self.catalog_error = None;
            }
            Err(error) => {
                let mut fallback = Catalog::default();
                fallback.entries = self
                    .objects
                    .iter()
                    .cloned()
                    .map(CatalogEntry::object)
                    .chain(self.stored.iter().cloned().map(CatalogEntry::routine))
                    .collect();
                fallback.total = fallback.entries.len();
                self.catalog = Arc::new(fallback);
                self.catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    /// The whole schema, for [`SearchSchema`].
    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        self.catalog.clone()
    }

    pub(crate) fn catalog_loading(&self) -> bool {
        self.catalog_loading
    }

    pub(crate) fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
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
                        this.stored.clear();
                        this.catalog = Arc::new(Catalog::default());
                        this.catalog_loading = true;
                        this.catalog_error = None;
                        this.rebuild_tree(cx);
                        let connection = this.connection.clone();
                        for panel in this.panels.clone() {
                            let connection = connection.clone();
                            panel.update(cx, |panel, cx| panel.set_connection(connection, cx));
                        }
                        this.reload_metadata(cx);
                        cx.emit(SessionEvent::Changed);
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
    fn run(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.confirm_write(panel.clone(), sql, false, window, cx);
            return;
        }

        self.run_now(panel, sql, cx);
    }

    /// Run every statement in `sql`, one after another.
    fn run_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A script is confirmed as a whole: what the user is being asked
        // about is the buffer they are about to run.
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.confirm_write(panel.clone(), sql, true, window, cx);
            return;
        }

        self.send(panel, sql, true, cx);
    }

    /// Ask before running a write, on a connection that confirms them.
    ///
    /// The dialog closes over the buffer it is about, so the answer runs
    /// exactly what was on screen when the question was asked.
    fn confirm_write(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        script: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.display_target();
        let statements = statement::split(&sql).len();
        let session = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            let description = match statement::first_write(&sql) {
                Some(_) if script => {
                    format!("{statements} statements change data on {target}.")
                }
                Some(word) => format!("The {word} statement changes data on {target}."),
                None => format!("This changes data on {target}."),
            };

            alert
                .title(if script {
                    "Run this script?"
                } else {
                    "Run this statement?"
                })
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Run")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.send(&panel, sql.clone(), script, cx)
                        });
                    }
                    true
                })
        });
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

        // A run is a natural checkpoint for the buffer text.
        cx.emit(SessionEvent::Changed);

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

    /// Read one statement's plan for `panel`.
    ///
    /// `ANALYZE` is the only form that runs anything, so it is the only form
    /// a careful connection asks about first.
    fn explain(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        analyze: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if analyze
            && !self.connection.config.safety.auto_applies()
            && !self.connection.config.safety.is_read_only()
        {
            self.confirm_explain(panel.clone(), sql, window, cx);
            return;
        }

        self.explain_now(panel, sql, analyze, cx);
    }

    /// Ask before running the query an `ANALYZE` would, the way a write is
    /// asked about: the statement the plan comes from is here, so the answer
    /// is the same either way.
    fn confirm_explain(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            alert
                .title("Explain and run this query?")
                .description(
                    "EXPLAIN ANALYZE runs the statement to report actual times, so the query \
                     really executes.",
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Explain")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.explain_now(&panel, sql.clone(), true, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Send `sql` for its plan; a tree lands in the tab's plan viewer, and a
    /// server that only knows the classic table lands in the grid.
    fn explain_now(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        analyze: bool,
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
        let task = runtime::spawn(async move { connection.explain(&sql, analyze).await });
        panel.update(cx, |panel, _| panel.set_running(task.abort_handle()));

        let weak = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the plan was read.
                let Some(panel) = weak.upgrade() else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));
                panel.update(cx, |panel, cx| {
                    panel.set_running(None);

                    match result {
                        Ok(Ok(Explained::Plan(plan))) => panel.set_plan(plan, cx),
                        Ok(Ok(Explained::Rows(result))) => {
                            panel.show_results(vec![result], cx);
                            panel.set_status(Status::Done(
                                "Classic plan output; shown in the result grid".into(),
                            ));
                        }
                        Ok(Err(error)) => {
                            let message = format!("{error:#}");
                            panel.set_status(Status::Error(message.clone()));
                            grid.update(cx, |grid, cx| grid.clear(cx));
                            crate::ui::notify_error(window, cx, format!("Error: {message}"));
                        }
                        // The sender is dropped when the read is given up on,
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
        // An import in flight is the other long-running thing a session owns,
        // so the same key gives up on it.
        self.cancel_import(cx);
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

    /// A slim, full-width strip in the connection's tag colour, shown under
    /// the toolbar for the whole session. The tag text is always present, and
    /// a lock icon plus "Read only" joins it when the safety mode is
    /// [`SafetyMode::ReadOnly`]; untagged connections show no strip.
    fn render_tag_strip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let tag = self.connection.config.tag.as_deref()?;
        let color = self.connection.config.color;
        let bg = color
            .map(|color| color.hsla(cx))
            .unwrap_or_else(|| cx.theme().muted);
        let fg = color
            .map(|color| color.on_color(cx))
            .unwrap_or_else(|| cx.theme().muted_foreground);
        let read_only = self.connection.config.safety.is_read_only();

        Some(
            h_flex()
                .id("session-tag")
                .w_full()
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .bg(bg)
                .text_color(fg)
                .child(div().text_sm().truncate().child(tag.to_string()))
                .when(read_only, |this| {
                    this.child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(Icon::new(IconName::Lock).size_3().text_color(fg))
                            .child(div().text_xs().child("Read only")),
                    )
                }),
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
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_save_file))
            .on_action(cx.listener(Self::on_save_file_as))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::on_import_dump))
            .on_action(cx.listener(Self::on_quick_switcher))
            .on_action(cx.listener(Self::on_search_schema))
            .when_some(self.render_tag_strip(cx), |this, strip| this.child(strip))
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
                        resizable_panel().child(div().flex_1().min_h_0().child(self.dock.clone())),
                    ),
            )
    }
}
