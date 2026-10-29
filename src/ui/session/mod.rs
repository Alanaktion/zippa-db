//! An open connection: object sidebar, and a dock of query, table,
//! structure, and console panels, split and reordered by dragging their tabs.

use std::sync::Arc;

use gpui_kit::component::dock::{
    DockArea, DockLayout, DockPlacement, DockSkin, InsertTarget, NodeId, PanelId, PanelStyle,
    TabGroup, panel_handle,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::select::{SearchableVec, SelectState};
use gpui_kit::component::tree::TreeState;
use gpui_kit::component::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, Focusable, SharedString, WeakEntity, Window, actions, div,
    px,
};

use regex::Regex;

use crate::db::{
    CatalogEntry, CatalogKind, Connection, DatabaseObject, PinnedConnection, StoredObject,
};
use crate::saved_queries::SavedQueries;
use crate::ui::agents::{AgentEvent, AgentPanel, ToggleAgents};
use crate::ui::backup_dialog::BackupView;
use crate::ui::completion::SharedCatalog;
use crate::ui::create_database_dialog::CreateDatabaseView;
use crate::ui::import_dialog::ImportView;
use crate::ui::variables_dialog::VariablesView;
use crate::workspace_state::SessionState;

pub(crate) mod chart;
pub(crate) mod dashboard;
mod files;
mod limit;
mod metadata;
mod panel;
mod running;
mod save_dialog;
mod sidebar;
mod state;
pub(crate) mod tab;
mod tabs;
#[cfg(test)]
mod test_support;
mod tool;
mod variables;

pub(crate) use panel::CloseScope;
use panel::{SessionPanel, SessionPanelEvent};
use sidebar::SidebarTab;
use tab::{ObjectViewMode, Status};
pub(crate) use tool::Tool;

/// Starting pane sizes; the user drags from here.
const SIDEBAR_WIDTH: f32 = 260.;
const AGENTS_WIDTH: f32 = 340.;

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
        Disconnect,
        Reconnect,
        BackupDatabase,
        ImportSqlDump,
        SearchSchema,
        OpenConsole,
        OpenConnectionInfo,
        OpenProcessList,
        OpenServerVariables,
        OpenQueryDigest,
        OpenMaintenance,
        OpenUserManagement,
        OpenErd,
        NewTable,
        SaveQuery,
        SaveQueryAs,
    ]
);

pub enum SessionEvent {
    /// Something about the session's tabs or its connection changed in a way
    /// the workspace's saved state should record.
    Changed,
    /// The user left the session; the workspace puts the connection manager
    /// back in its tab and drains the pool.
    Disconnected,
}

pub struct Session {
    connection: Arc<Connection>,
    /// The one connection every query tab shares when the
    /// `share_connection_across_buffers` setting is on, checked out of the
    /// pool on first use. `None` until a tab runs, or when each tab keeps its
    /// own.
    shared_pin: Option<PinnedConnection>,
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
    /// The Agents panel's width, beside the dock.
    agents_columns: Entity<ResizableState>,
    /// The Agents panel. Built with the session so its thread lives as long
    /// as the connection; only drawn while `agents_open`.
    agents: Entity<AgentPanel>,
    agents_open: bool,
    databases: Vec<String>,
    /// The title bar's database picker: a searchable list, since a server
    /// can hold more databases than a menu reads comfortably. Its items and
    /// selection follow `databases` and the current database, synced in
    /// `render` (see `sync_database_select`).
    database_select: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The list `database_select` was last given, so a render only resets
    /// it when the databases changed.
    database_select_items: Vec<SharedString>,
    objects: Vec<DatabaseObject>,
    /// Functions, procedures and sequences, listed after the tables.
    stored: Vec<StoredObject>,
    /// Text filter over the object list.
    filter: Entity<InputState>,
    /// Compiled form of the filter; `None` means "show everything".
    matcher: Option<Regex>,
    /// Which of the sidebar's two tabs — the object list or the server tools —
    /// is showing.
    sidebar_tab: SidebarTab,
    /// The sidebar's object list; its selection follows the active tab's
    /// table, cleared for a query tab.
    objects_tree: Entity<TreeState>,
    /// Set when the schema could not be read; queries still work.
    metadata_error: Option<String>,
    /// The whole schema, for [`SearchSchema`] and the query editors'
    /// completions. Read once in the background as the session opens and
    /// again on refresh, so a keystroke never waits on the server. Holds only
    /// names until the read lands. Shared with every query tab's editor, which
    /// reads whichever snapshot is current. Other databases' schemas are
    /// fetched through it on demand, when a query names them.
    catalog: SharedCatalog,
    /// Whether that read is still in flight, so the dialog can say so.
    catalog_loading: bool,
    /// Why the full catalog could not be read, when it could not; search then
    /// finds names but not columns, indexes or triggers.
    catalog_error: Option<String>,
    switching: bool,
    /// The saved tabs a restore is holding while it switches to their
    /// database. Until they are built, they are what this session records,
    /// so a checkpoint or quit in the meantime does not save them away.
    restoring: Option<SessionState>,
    /// Whether that reopen is a [`Reconnect`] to the same database rather
    /// than a switch, so the picker can say which.
    reconnecting: bool,
    /// The next panel's stable key. See `SessionPanel`'s `key` field.
    next_key: usize,
    /// Counts "Copy DDL" reads: a read captures the count it started under
    /// and only writes the clipboard when no newer read has started since,
    /// so an older read finishing last cannot replace a newer definition.
    copy_ddl_generation: u64,
    /// The import dialog, while one is open over the window.
    import: Option<Entity<ImportView>>,
    /// The backup dialog, while one is open over the window.
    backup: Option<Entity<BackupView>>,
    /// The create-database dialog, while one is open over the window.
    create_database: Option<Entity<CreateDatabaseView>>,
    /// The query-variables dialog, while one is open over the window, and
    /// the tab whose variables it edits.
    variables_dialog: Option<(Entity<SessionPanel>, Entity<VariablesView>)>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables"));
        cx.subscribe_in(&filter, window, Self::on_filter_event)
            .detach();
        // The sidebar's Saved tab draws the app's one saved-query list, which
        // any window's save dialog or delete button may change.
        cx.observe_global::<SavedQueries>(|_, cx| cx.notify())
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

        let database_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        cx.subscribe_in(&database_select, window, Self::on_database_selected)
            .detach();

        let catalog = SharedCatalog::default();
        let agents = cx.new(|cx| AgentPanel::new(connection.clone(), catalog.clone(), window, cx));
        cx.subscribe_in(&agents, window, Self::on_agent_event)
            .detach();

        let mut session = Self {
            connection,
            shared_pin: None,
            dock,
            panels: Vec::new(),
            active: None,
            opened: 0,
            columns: cx.new(|_| ResizableState::default()),
            agents_columns: cx.new(|_| ResizableState::default()),
            agents,
            agents_open: false,
            databases: Vec::new(),
            database_select,
            database_select_items: Vec::new(),
            objects: Vec::new(),
            stored: Vec::new(),
            filter,
            matcher: None,
            sidebar_tab: SidebarTab::Schema,
            objects_tree: cx.new(|cx| TreeState::new(cx)),
            metadata_error: None,
            catalog,
            catalog_loading: true,
            catalog_error: None,
            switching: false,
            restoring: None,
            reconnecting: false,
            next_key: 0,
            copy_ddl_generation: 0,
            import: None,
            backup: None,
            create_database: None,
            variables_dialog: None,
        };
        session
            .catalog
            .set_source(session.connection.clone(), Vec::new());
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
        // A newly installed tab is not reached through `bring_forward`, which
        // is what puts the keyboard in an already-open tab when it is
        // switched to — so a tab opened from anywhere but a mouse click on
        // its own element (the quick switcher, a keybinding, the sidebar)
        // would otherwise leave focus wherever it was, and every shortcut
        // bound under the new tab's context reads as having nothing to act
        // on until the user clicks into it by hand.
        self.focus(window, cx);
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

        let group = panel.read(cx).group().and_then(|group| group.upgrade());
        let index = self.panels.iter().position(|p| p == panel).unwrap_or(0);

        self.panels.retain(|p| p != panel);
        // Whatever the dock decides to show, it is not the tab that has just
        // gone; the choice is made below, once the dock has made it.
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = None;
        }

        if self.panels.is_empty() {
            // The session always shows one editor.
            self.open_tab(None, String::new(), false, window, cx);
            self.focus(window, cx);
            return;
        }

        self.adopt_dock_active(group, index, cx);
        self.sync_tree_selection(cx);
        self.focus(window, cx);
        cx.emit(SessionEvent::Changed);
        cx.notify();
    }

    /// Take the dock's word for which tab is in front.
    ///
    /// A tab closing is the dock's cue to slide the one after it into its
    /// place, or the one before it when it was last. That tab is what is on
    /// screen, so it is the one the session has to call active: leaving it on
    /// the tab that has just gone — or guessing from creation order — points
    /// the keyboard at an element the dock is no longer drawing, and the next
    /// keystroke, `Cmd+W` included, lands nowhere at all.
    ///
    /// `index` is the closed tab's place in creation order, used only when the
    /// dock cannot be asked.
    fn adopt_dock_active(&mut self, group: Option<Entity<TabGroup>>, index: usize, cx: &App) {
        // A tab opened while this one was going keeps the session's active tab
        // pointing at something that exists, and the dock agrees with it.
        if self
            .active
            .as_ref()
            .is_some_and(|active| self.panels.iter().any(|panel| panel.downgrade() == *active))
        {
            return;
        }

        let displayed = group.and_then(|group| {
            let id = group.read(cx).active_panel(cx)?.panel_id(cx);
            self.panels
                .iter()
                .find(|panel| PanelId::from(panel.entity_id()) == id)
                .cloned()
        });

        self.active = displayed
            .or_else(|| self.panels.get(index).cloned())
            .or_else(|| self.panels.last().cloned())
            .map(|panel| panel.downgrade());
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
            SessionPanelEvent::CloseScopeRequested(scope) => {
                self.close_scope(panel, *scope, window, cx)
            }
            SessionPanelEvent::NewTabRequested => {
                self.touch(panel, cx);
                self.open_tab(None, String::new(), false, window, cx);
            }
            SessionPanelEvent::Run(sql) => self.run(panel, sql.clone(), window, cx),
            SessionPanelEvent::RunScript(sql) => {
                self.run_script(panel, sql.clone(), false, window, cx)
            }
            SessionPanelEvent::RunScriptIgnoringErrors(sql) => {
                self.run_script(panel, sql.clone(), true, window, cx)
            }
            SessionPanelEvent::EndTransaction(commit) => self.end_transaction(panel, *commit, cx),
            SessionPanelEvent::Explain { sql, analyze } => {
                self.explain(panel, sql.clone(), *analyze, window, cx)
            }
            SessionPanelEvent::OpenFile => self.open_file(cx),
            SessionPanelEvent::Save { as_new } => self.save(panel, *as_new, cx),
            SessionPanelEvent::SaveQuery { as_new } => self.save_query(panel, *as_new, window, cx),
            SessionPanelEvent::EditVariables => self.open_variables_dialog(panel, window, cx),
        }
    }

    pub(crate) fn objects(&self) -> &[DatabaseObject] {
        &self.objects
    }

    pub(crate) fn databases(&self) -> &[String] {
        &self.databases
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
        let _ = crate::ui::schema_search::open(session, window, cx);
    }

    /// Leave the connection; the workspace puts a fresh manager in this tab.
    ///
    /// Closing the tab would drop the connection and every tab in it, so this
    /// keeps the tab and its place in the bar for the next connection. The
    /// workspace asks first when a tab holds unsaved work.
    fn on_disconnect(&mut self, _: &Disconnect, _window: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(cx);
    }

    /// Hand the tab back to the connection manager, as `Disconnect` does.
    pub(crate) fn disconnect(&mut self, cx: &mut Context<Self>) {
        cx.emit(SessionEvent::Disconnected);
    }

    fn on_reconnect(&mut self, _: &Reconnect, window: &mut Window, cx: &mut Context<Self>) {
        self.request_reconnect(window, cx);
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
}

impl Session {
    /// The dock, with the Agents panel beside it while that is open.
    fn render_workspace_area(&self) -> gpui_kit::AnyElement {
        let dock = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .size_full()
            .child(self.dock.clone());
        if !self.agents_open {
            return dock.into_any_element();
        }
        h_resizable("session-agents")
            .with_state(&self.agents_columns)
            .child(resizable_panel().child(dock))
            .child(
                resizable_panel()
                    .size(px(AGENTS_WIDTH))
                    .size_range(px(280.)..px(720.))
                    .child(self.agents.clone()),
            )
            .into_any_element()
    }

    fn toggle_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agents_open = !self.agents_open;
        if self.agents_open {
            let handle = self.agents.read(cx).focus_handle(cx);
            handle.focus(window, cx);
        }
        cx.notify();
    }

    fn on_agent_event(
        &mut self,
        _: &Entity<AgentPanel>,
        event: &AgentEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AgentEvent::Send(text) => {
                let titles: Vec<String> = self
                    .panels
                    .iter()
                    .map(|panel| panel.read(cx).title().to_string())
                    .collect();
                let buffer = self
                    .active_panel()
                    .and_then(|panel| panel.read(cx).editor())
                    .map(|editor| editor.read(cx).sql(cx));
                let text = text.clone();
                self.agents
                    .update(cx, |agents, cx| agents.start_turn(text, titles, buffer, cx));
            }
            AgentEvent::OpenTable(object) => {
                self.open_object(object, ObjectViewMode::Data, window, cx);
            }
            AgentEvent::ProposeQuery(sql) => {
                self.open_tab(Some("Agent".into()), sql.clone(), false, window, cx);
            }
        }
    }
}

impl Render for Session {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_database_select(window, cx);
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
            .on_action(cx.listener(Self::on_backup_database))
            .on_action(cx.listener(Self::on_quick_switcher))
            .on_action(cx.listener(Self::on_search_schema))
            .on_action(cx.listener(|this, _: &OpenConsole, window, cx| {
                this.open_tool(Tool::Console, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenConnectionInfo, window, cx| {
                this.open_tool(Tool::ConnectionInfo, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenProcessList, window, cx| {
                this.open_tool(Tool::Processes, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenServerVariables, window, cx| {
                this.open_tool(Tool::Variables, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenQueryDigest, window, cx| {
                this.open_tool(Tool::Digest, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenMaintenance, window, cx| {
                this.open_tool(Tool::Maintenance, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenUserManagement, window, cx| {
                this.open_tool(Tool::Users, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenErd, window, cx| {
                this.open_tool(Tool::Erd, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NewTable, window, cx| {
                this.open_new_table(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleAgents, window, cx| {
                this.toggle_agents(window, cx);
            }))
            .on_action(cx.listener(Self::on_save_query))
            .on_action(cx.listener(Self::on_save_query_as))
            .on_action(cx.listener(Self::on_disconnect))
            .on_action(cx.listener(Self::on_reconnect))
            .child(
                h_resizable("session-columns")
                    .with_state(&self.columns)
                    .child(
                        resizable_panel()
                            .size(px(SIDEBAR_WIDTH))
                            .size_range(px(180.)..px(560.))
                            .child(self.render_sidebar(cx)),
                    )
                    .child(resizable_panel().child(self.render_workspace_area())),
            )
    }
}
