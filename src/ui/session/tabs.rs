//! Opening, closing, and moving between a session's tabs.
//!
//! A session always shows at least one editor: every close path goes through
//! [`Session::close_tab_now`], which opens a fresh one before the last tab
//! leaves.

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::dock::PanelId;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window};

use crate::db::{DatabaseObject, Engine};
use crate::ui::console::ConsoleView;
use crate::ui::filter_bar::FilterSpec;
use crate::ui::process_list::ProcessListView;
use crate::ui::query_digest::QueryDigestView;
use crate::ui::schema_view::SchemaView;
use crate::ui::server_variables::ServerVariablesView;
use crate::ui::sqlite_maintenance::SqliteMaintenanceView;
use crate::ui::table_view::{TableView, TableViewEvent};

use super::{
    CloseScope, CloseTab, NewTab, NextTab, ObjectViewMode, PreviousTab, Session, SessionEvent,
    SessionPanel,
};

impl Session {
    /// Stop every tab's run, on the server too, before the connection
    /// closes: closing only waits for them, and a statement left running
    /// would still commit after the user had disconnected.
    pub(crate) fn stop_runs(&mut self, cx: &mut Context<Self>) {
        for panel in self.panels.clone() {
            panel.update(cx, |panel, _| panel.abort_running());
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
                self.catalog.clone(),
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
                // No "Structure" suffix: the tab's icon tells it from the data
                // tab of the same table.
                let title = object.label();
                cx.new(|cx| SessionPanel::schema(key, title, view, cx))
            }
        };

        self.install(panel, window, cx);
    }

    /// Open the console tab, or bring forward the one already open — there is
    /// only ever one per session, the same as a table's data and structure
    /// tabs are each one of a kind.
    pub(crate) fn open_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| panel.read(cx).is_console())
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;
        let view = cx.new(|cx| ConsoleView::new(self.connection.clone(), window, cx));
        let panel = cx.new(|cx| SessionPanel::console(key, "Console", view, cx));
        self.install(panel, window, cx);
    }

    /// Open the process list tab, or bring forward the one already open.
    /// SQLite has no server to ask, so this is never called for one — see
    /// `Session::render_sidebar`, which does not offer the button there.
    pub(crate) fn open_process_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.config.engine == Engine::Sqlite {
            return;
        }

        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| panel.read(cx).is_process_list())
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;
        let view = cx.new(|cx| ProcessListView::new(self.connection.clone(), window, cx));
        let panel = cx.new(|cx| SessionPanel::processes(key, "Processes", view, cx));
        self.install(panel, window, cx);
    }

    /// Open the server variables tab, or bring forward the one already open.
    /// SQLite has no server-side configuration to show, so this is never
    /// called for one — see `Session::render_sidebar`.
    pub(crate) fn open_server_variables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.config.engine == Engine::Sqlite {
            return;
        }

        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| panel.read(cx).is_server_variables())
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;
        let view = cx.new(|cx| ServerVariablesView::new(self.connection.clone(), window, cx));
        let panel = cx.new(|cx| SessionPanel::variables(key, "Variables", view, cx));
        self.install(panel, window, cx);
    }

    /// Open the query digest tab, or bring forward the one already open.
    /// SQLite has no query instrumentation to read this way, so this is
    /// never called for one — see `Session::render_sidebar`.
    pub(crate) fn open_query_digest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.config.engine == Engine::Sqlite {
            return;
        }

        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| panel.read(cx).is_query_digest())
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;
        let view = cx.new(|cx| QueryDigestView::new(self.connection.clone(), window, cx));
        let panel = cx.new(|cx| SessionPanel::digest(key, "Query Digest", view, cx));
        self.install(panel, window, cx);
    }

    /// Open the SQLite maintenance tab, or bring forward the one already
    /// open. The other engines have a server to look after this, so this is
    /// never called for one — see `Session::render_sidebar`.
    pub(crate) fn open_maintenance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.config.engine != Engine::Sqlite {
            return;
        }

        if let Some(panel) = self
            .panels
            .iter()
            .find(|panel| panel.read(cx).is_maintenance())
            .cloned()
        {
            self.activate_panel(&panel, window, cx);
            return;
        }

        let key = self.next_key;
        self.next_key += 1;
        let view = cx.new(|cx| SqliteMaintenanceView::new(self.connection.clone(), window, cx));
        let panel = cx.new(|cx| SessionPanel::maintenance(key, "Maintenance", view, cx));
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
            TableViewEvent::ViewStructure { object } => {
                self.open_object(object, ObjectViewMode::Schema, window, cx);
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

        let Some(view) = self.table_view_of(object, cx) else {
            return;
        };
        view.update(cx, |view, cx| {
            view.apply_external_filter(filter, window, cx)
        });
    }

    /// The open data tab for `object`, if it has one — the lookup
    /// [`Self::open_object_filtered`] and [`Self::restore_table_filters`]
    /// both need once the tab is open.
    fn table_view_of(&self, object: &DatabaseObject, cx: &App) -> Option<Entity<TableView>> {
        self.panels
            .iter()
            .find(|panel| {
                let panel = panel.read(cx);
                panel.mode() == Some(ObjectViewMode::Data)
                    && panel.object(cx).as_ref() == Some(object)
            })
            .and_then(|panel| panel.read(cx).table_view())
    }

    /// Put a restored table tab's filters back, keeping every one of them —
    /// unlike [`Self::open_object_filtered`], which replaces the lot with a
    /// single foreign key jump. Called right after [`Self::open_object`] has
    /// opened (or brought forward) the tab.
    pub(crate) fn restore_table_filters(
        &mut self,
        object: &DatabaseObject,
        filters: Vec<FilterSpec>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if filters.is_empty() {
            return;
        }
        let Some(view) = self.table_view_of(object, cx) else {
            return;
        };
        view.update(cx, |view, cx| view.restore_filters(filters, window, cx));
    }

    /// Close a tab, asking first if it holds unsaved changes.
    pub(super) fn close_tab(
        &mut self,
        panel: &Entity<SessionPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.panels.iter().any(|p| p == panel) {
            return;
        }

        if panel.read(cx).is_dirty(cx) || panel.read(cx).transaction().is_open() {
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
        let dirty = panel.read(cx).is_dirty(cx);
        let transaction = panel.read(cx).transaction().is_open();
        let session = cx.entity().downgrade();
        let panel = panel.clone();
        let (heading, description, ok) =
            close_question(&title, 1, dirty as usize, transaction as usize);

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();

            alert
                .title(heading.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok)
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
    pub(super) fn close_tab_now(
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

        let group = panel.read(cx).group().and_then(|group| group.upgrade());
        let index = self.panels.iter().position(|p| p == panel).unwrap_or(0);

        self.panels.retain(|p| p != panel);
        if self.active.as_ref().is_some_and(|w| w == panel) {
            self.active = None;
        }

        self.dock
            .update(cx, |dock, cx| dock.remove_panel(panel.clone(), window, cx));

        // The dock slides a tab into the closed one's place and drops the
        // focus with the panel, so which tab is in front — and where the
        // keyboard goes — is settled after the removal, not before it.
        self.adopt_dock_active(group, index, cx);
        self.sync_tree_selection(cx);
        self.focus(window, cx);
        cx.emit(SessionEvent::Changed);
        cx.notify();
    }

    /// Close the tabs a tab command named.
    pub(super) fn close_scope(
        &mut self,
        panel: &Entity<SessionPanel>,
        scope: CloseScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.scope_targets(panel, scope, cx);
        if targets.is_empty() {
            return;
        }

        self.close_tabs(targets, window, cx);
    }

    /// The tabs a command takes with the tab it was opened from: its
    /// neighbours in the strip, or every tab of one kind in the session.
    fn scope_targets(
        &self,
        panel: &Entity<SessionPanel>,
        scope: CloseScope,
        cx: &App,
    ) -> Vec<Entity<SessionPanel>> {
        match scope {
            CloseScope::QueryTabs => self
                .panels
                .iter()
                .filter(|panel| panel.read(cx).is_query())
                .cloned()
                .collect(),
            CloseScope::TableTabs => self
                .panels
                .iter()
                .filter(|panel| !panel.read(cx).is_query())
                .cloned()
                .collect(),
            CloseScope::Others | CloseScope::ToTheRight => {
                let strip = self.strip(panel, cx);
                let Some(index) = strip.iter().position(|candidate| candidate == panel) else {
                    return Vec::new();
                };
                match scope {
                    CloseScope::Others => strip
                        .into_iter()
                        .enumerate()
                        .filter(|(ix, _)| *ix != index)
                        .map(|(_, panel)| panel)
                        .collect(),
                    _ => strip.into_iter().skip(index + 1).collect(),
                }
            }
        }
    }

    /// The tabs shown in the same strip as `panel`, in the order they appear
    /// on screen. A drag can reorder a strip, so the dock is asked rather than
    /// the session's creation order used; that order is the fallback for a
    /// panel the dock cannot place.
    pub(super) fn strip(
        &self,
        panel: &Entity<SessionPanel>,
        cx: &App,
    ) -> Vec<Entity<SessionPanel>> {
        let order: Option<Vec<PanelId>> = panel
            .read(cx)
            .group()
            .and_then(|group| group.upgrade())
            .map(|group| {
                group
                    .read(cx)
                    .panels()
                    .iter()
                    .map(|panel| panel.panel_id(cx))
                    .collect()
            });
        let Some(order) = order else {
            return self.panels.clone();
        };

        let strip: Vec<_> = order
            .iter()
            .filter_map(|id| {
                self.panels
                    .iter()
                    .find(|panel| PanelId::from(panel.entity_id()) == *id)
                    .cloned()
            })
            .collect();
        if strip.is_empty() {
            self.panels.clone()
        } else {
            strip
        }
    }

    /// Close `targets` together, asking once when any of them holds work that
    /// was never written.
    fn close_tabs(
        &mut self,
        targets: Vec<Entity<SessionPanel>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = targets
            .iter()
            .filter(|panel| panel.read(cx).is_dirty(cx))
            .count();
        let transactions = targets
            .iter()
            .filter(|panel| panel.read(cx).transaction().is_open())
            .count();
        if dirty == 0 && transactions == 0 {
            self.close_tabs_now(&targets, window, cx);
            return;
        }

        self.confirm_close_tabs(targets, dirty, transactions, window, cx);
    }

    /// Ask before throwing away several tabs' unsaved buffers at once.
    ///
    /// One question about the set, rather than one per tab: the user picked a
    /// command about a group of tabs, and a dialog each is not what they asked
    /// for. The tabs are held in the dialog's closure, so the answer closes
    /// exactly the tabs that were on screen when the question was asked.
    fn confirm_close_tabs(
        &mut self,
        targets: Vec<Entity<SessionPanel>>,
        dirty: usize,
        transactions: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = targets.len();
        let title = targets
            .first()
            .map(|panel| panel.read(cx).title())
            .unwrap_or_default();
        let session = cx.entity().downgrade();
        let (heading, description, ok) = close_question(&title, count, dirty, transactions);

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let targets = targets.clone();

            alert
                .title(heading.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok)
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Keep Open")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.close_tabs_now(&targets, window, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Remove every tab in `targets`; the caller has already decided their
    /// unsaved work does not matter.
    ///
    /// One at a time, through [`Self::close_tab_now`]: that is what keeps the
    /// session's "always one editor" rule and the tab in front in step when
    /// the set takes the last of them.
    fn close_tabs_now(
        &mut self,
        targets: &[Entity<SessionPanel>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for panel in targets {
            if self.panels.iter().any(|open| open == panel) {
                self.close_tab_now(panel, window, cx);
            }
        }
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

    /// How many tabs hold staged rows or structure edits that a new
    /// connection — a database switch, a reconnect — would throw away.
    pub(crate) fn unapplied_changes(&self, cx: &App) -> usize {
        self.panels
            .iter()
            .filter(|panel| panel.read(cx).has_unapplied_changes(cx))
            .count()
    }

    /// How many tabs have a transaction open — work that closing the
    /// connection, or switching it to another database, would roll back.
    pub(crate) fn open_transactions(&self, cx: &App) -> usize {
        self.panels
            .iter()
            .filter(|panel| panel.read(cx).transaction().is_open())
            .count()
    }

    /// What leaving this connection would throw away — `(title, sentence)`
    /// for the workspace's question — or `None` when nothing would be lost.
    pub(crate) fn leave_warning(&self, cx: &App) -> Option<(&'static str, String)> {
        let name = self.display_name();
        let unsaved = self.has_unsaved_changes(cx);
        let open = match self.open_transactions(cx) {
            0 => None,
            1 => Some("a tab with an open transaction, which will be rolled back".to_string()),
            count => Some(format!(
                "{count} tabs with open transactions, which will be rolled back"
            )),
        };
        match (unsaved, open) {
            (false, None) => None,
            (true, None) => Some((
                "Unsaved Changes",
                format!("\"{name}\" has tabs with unsaved changes."),
            )),
            (false, Some(open)) => Some(("Open Transaction", format!("\"{name}\" has {open}."))),
            (true, Some(open)) => Some((
                "Unsaved Changes",
                format!("\"{name}\" has tabs with unsaved changes, and {open}."),
            )),
        }
    }

    pub(crate) fn active_tab_index(&self) -> usize {
        let Some(active) = self.active.as_ref() else {
            return 0;
        };
        self.panels.iter().position(|p| p == active).unwrap_or(0)
    }

    pub(super) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(None, String::new(), false, window, cx);
    }

    /// Same as [`Self::on_new_tab`], for the toolbar button which does not
    /// have an `&NewTab` to hand it.
    pub(crate) fn new_query_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(None, String::new(), false, window, cx);
    }

    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(1, window, cx);
    }

    pub(super) fn on_previous_tab(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn on_close_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel() {
            self.close_tab(&panel, window, cx);
        }
    }
}

/// The question asked before closing tabs that hold something closing would
/// lose: `(title, description, button)`. `count` tabs are closing, `dirty` of
/// them hold unsaved changes and `transactions` an open transaction.
pub(super) fn close_question(
    title: &str,
    count: usize,
    dirty: usize,
    transactions: usize,
) -> (String, String, &'static str) {
    let what = match count {
        1 => format!("\"{title}\""),
        _ => format!("{count} tabs"),
    };
    let unsaved = match (count, dirty) {
        (_, 0) => None,
        (1, _) => Some("The tab has unsaved changes.".to_string()),
        (_, dirty) => Some(format!("{dirty} of them have unsaved changes.")),
    };
    let open = match (count, transactions) {
        (_, 0) => None,
        (1, _) => Some(
            "The tab has an open transaction; closing it rolls back everything since it began."
                .to_string(),
        ),
        (_, 1) => Some("1 of them has an open transaction, which closing rolls back.".to_string()),
        (_, open) => Some(format!(
            "{open} of them have open transactions, which closing rolls back."
        )),
    };

    match (unsaved, open) {
        (Some(unsaved), None) => (
            format!("Close {what} without saving?"),
            unsaved,
            "Close Without Saving",
        ),
        (None, Some(open)) => (
            format!("Roll back and close {what}?"),
            open,
            "Roll Back and Close",
        ),
        (Some(unsaved), Some(open)) => (
            format!("Close {what} without saving?"),
            format!("{unsaved} {open}"),
            "Roll Back and Close",
        ),
        (None, None) => (format!("Close {what}?"), String::new(), "Close"),
    }
}
