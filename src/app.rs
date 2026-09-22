//! Root view: every open connection in a tab of its own.
//!
//! A tab holds either the connection manager or an open session. Connecting
//! turns the tab it was opened from into that session and disconnecting turns
//! it back, so a connection is always somewhere on the bar rather than
//! replacing what the window was showing. The window keeps at least one tab,
//! the way the session keeps at least one editor.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::tab::{Tab, TabBar, TabVariant};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Root, Sizable, TitleBar, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EntityId, FocusHandle, Hsla, MouseButton, Pixels, SharedString, Window,
    actions, div, px,
};

use crate::db::{Connection, TagColor, runtime, store};
use crate::settings;
use crate::ui::session::{NewTab, QuickSwitcher, Refresh, SearchSchema, Session, SessionEvent};
use crate::ui::settings_window::{self, OpenSettings};
use crate::ui::shortcuts_dialog::{self, ShowShortcuts};
use crate::ui::value_dialog::{self, Dismissed, ValueView};
use crate::ui::welcome::{Welcome, WelcomeEvent};
use crate::workspace_state::{self, SessionState, WorkspaceState};

/// The value dialog is a reading pane rather than a prompt, so it is wider
/// and taller than a dialog's default.
const VALUE_DIALOG_WIDTH: Pixels = px(640.);
const VALUE_DIALOG_HEIGHT: Pixels = px(416.);

actions!(
    zippa_db,
    [
        NewConnection,
        CloseConnection,
        NextConnection,
        PreviousConnection
    ]
);

/// What a tab holds: the connection manager, or a connection that is open.
enum TabContent {
    Connect(Entity<Welcome>),
    Session(Entity<Session>),
}

impl TabContent {
    fn title(&self, cx: &App) -> SharedString {
        match self {
            Self::Connect(_) => "New connection".into(),
            Self::Session(session) => session.read(cx).connection().config.display_name().into(),
        }
    }

    /// Tells a connection to a server from one to a file at a glance.
    fn icon(&self, cx: &App) -> Option<IconName> {
        match self {
            Self::Connect(_) => None,
            Self::Session(session) => {
                match session.read(cx).connection().config.engine.is_file_based() {
                    true => Some(IconName::File),
                    false => Some(IconName::Globe),
                }
            }
        }
    }

    /// The engine's own accent, so tabs for different engines are told apart
    /// without reading the target string.
    fn icon_color(&self, cx: &App) -> Option<Hsla> {
        match self {
            Self::Connect(_) => None,
            Self::Session(session) => Some(crate::ui::engine_color(
                session.read(cx).connection().config.engine,
                cx,
            )),
        }
    }

    /// The connection's environment tag and colour, if it has one.
    fn tag(&self, cx: &App) -> Option<(String, Option<TagColor>)> {
        match self {
            Self::Connect(_) => None,
            Self::Session(session) => {
                let config = &session.read(cx).connection().config;
                config.tag.as_ref().map(|tag| (tag.clone(), config.color))
            }
        }
    }

    /// The tab's title, with the tag appended so the environment reads at a
    /// glance without losing the name.
    fn label(&self, cx: &App) -> SharedString {
        match self.tag(cx) {
            Some((tag, _)) => format!("{} · {}", self.title(cx), tag).into(),
            None => self.title(cx),
        }
    }

    /// The icon and tag chip shown before the label.
    fn prefix(&self, cx: &App) -> Option<impl IntoElement> {
        let icon = self.icon(cx)?;
        let icon = Icon::new(icon);
        let icon = match self.icon_color(cx) {
            Some(color) => icon.text_color(color),
            None => icon,
        };

        Some(
            h_flex()
                .gap_1()
                .items_center()
                .child(icon)
                .when_some(self.tag(cx), |this, (tag, color)| {
                    this.child(crate::ui::tag_chip(&tag, color, cx))
                }),
        )
    }
}

pub struct Workspace {
    tabs: Vec<TabContent>,
    active: usize,
    /// The value dialog, while one is open over the window.
    value: Option<Entity<ValueView>>,
    /// Held by the window itself, so the connection shortcuts answer even when
    /// nothing inside a tab has the caret.
    focus: FocusHandle,
    /// Tabs a startup restore is reconnecting, keyed by the connection manager
    /// that is connecting them. Their contents are kept here until the session
    /// exists, so a failed reconnect does not erase the user's tabs.
    pending: HashMap<EntityId, SessionState>,
    /// The snapshot last written, so an unchanged one is not written again.
    last_saved: Option<WorkspaceState>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A workspace file that cannot be read must not keep the app from
        // starting; an empty one is simply the first launch.
        let state = workspace_state::load().unwrap_or_else(|error| {
            eprintln!("could not read the workspace: {error:#}");
            WorkspaceState::default()
        });
        Self::build(state, window, cx)
    }

    /// Build a workspace, restoring `state`'s tabs into it.
    fn build(state: WorkspaceState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // The window knows the system appearance more reliably than the app
        // does on Linux, and it is the only thing that hears about a change.
        settings::follow_system_appearance(window, cx);
        window
            .observe_window_appearance(settings::follow_system_appearance)
            .detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);

        let mut workspace = Self {
            tabs: Vec::new(),
            active: 0,
            value: None,
            focus,
            pending: HashMap::new(),
            last_saved: None,
        };
        workspace.restore_tabs(state, window, cx);
        workspace
    }

    /// Reopen every connection the last session had, then let each turn its
    /// tab into a session and restore its own tabs.
    ///
    /// Nothing is restored synchronously: a connection has to be opened first,
    /// and that follows the same path a manual connect does.
    fn restore_tabs(&mut self, state: WorkspaceState, window: &mut Window, cx: &mut Context<Self>) {
        let connections = if state.sessions.is_empty() {
            Vec::new()
        } else {
            store::load().unwrap_or_else(|error| {
                eprintln!("could not read the saved connections: {error:#}");
                Vec::new()
            })
        };

        let mut active = 0;
        for (index, session) in state.sessions.into_iter().enumerate() {
            // A connection deleted since the file was written is skipped.
            if !connections
                .iter()
                .any(|config| config.id == session.connection)
            {
                continue;
            }

            let welcome = Self::welcome(window, cx);
            self.pending.insert(welcome.entity_id(), session.clone());
            welcome.update(cx, |welcome, cx| {
                welcome.open(session.connection, window, cx)
            });
            if index == state.active {
                active = self.tabs.len();
            }
            self.tabs.push(TabContent::Connect(welcome));
        }

        if self.tabs.is_empty() {
            // Nothing to restore: the window opens on the connection manager.
            self.open_connect_tab(window, cx);
            return;
        }

        self.active = active.min(self.tabs.len() - 1);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Add a tab showing the connection manager and make it active.
    fn open_connect_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let welcome = Self::welcome(window, cx);
        self.tabs.push(TabContent::Connect(welcome));
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    fn welcome(window: &mut Window, cx: &mut Context<Self>) -> Entity<Welcome> {
        let welcome = cx.new(|cx| Welcome::new(window, cx));
        cx.subscribe_in(&welcome, window, Self::on_welcome_event)
            .detach();
        welcome
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }

        self.active = index;
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// Put the caret in the session being shown, so its shortcuts work without
    /// clicking into it first.
    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(TabContent::Session(session)) => {
                let session = session.clone();
                session.update(cx, |session, cx| session.focus(window, cx));
            }
            // The launcher puts the caret in its search box when there is
            // something to search; focus left behind in a hidden tab would
            // keep answering keystrokes.
            Some(TabContent::Connect(welcome)) => {
                let welcome = welcome.clone();
                welcome.update(cx, |welcome, cx| welcome.focus(window, cx));
            }
            _ => self.focus.focus(window, cx),
        }
    }

    /// Close a tab, asking first if it holds a connection with unsaved
    /// changes in one of its own tabs.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        if let TabContent::Session(session) = &self.tabs[index]
            && session.read(cx).has_unsaved_changes(cx)
        {
            self.confirm_close_tab(session.clone(), window, cx);
            return;
        }

        self.close_tab_now(index, window, cx);
    }

    /// Ask before throwing away a connection's unsaved tabs; closes it on
    /// confirmation, by whatever index it still holds by then.
    fn confirm_close_tab(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = session.read(cx).display_name();
        let workspace = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let session = session.clone();
            alert
                .title("Unsaved Changes")
                .description(format!(
                    "\"{name}\" has tabs with unsaved changes. Close it anyway?"
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Close Without Saving")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Keep Open")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(workspace) = workspace.upgrade() {
                        workspace.update(cx, |this, cx| {
                            if let Some(index) = this.tab_of(
                                |tab| matches!(tab, TabContent::Session(open) if open == &session),
                            ) {
                                this.close_tab_now(index, window, cx);
                            }
                        });
                    }
                    true
                })
        });
    }

    /// Replace a session's tab with a fresh connection manager, the way the
    /// sidebar's Disconnect button does. The caller has already decided any
    /// unsaved work in the session's tabs does not matter.
    fn disconnect(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) =
            self.tab_of(|tab| matches!(tab, TabContent::Session(open) if open == &session))
        else {
            return;
        };

        close(session.read(cx).connection());
        // Disconnecting keeps the tab, so another connection can be opened
        // from where the last one was.
        self.tabs[index] = TabContent::Connect(Self::welcome(window, cx));
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// Ask before disconnecting a connection whose own tabs hold unsaved
    /// changes; disconnects it on confirmation.
    fn confirm_disconnect(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = session.read(cx).display_name();
        let workspace = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let session = session.clone();
            alert
                .title("Unsaved Changes")
                .description(format!(
                    "\"{name}\" has tabs with unsaved changes. Disconnect anyway?"
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Disconnect Without Saving")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Keep Connected")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(workspace) = workspace.upgrade() {
                        workspace.update(cx, |this, cx| {
                            this.disconnect(session.clone(), window, cx);
                        });
                    }
                    true
                })
        });
    }

    /// Remove a tab outright; the caller has already decided any unsaved
    /// changes in it do not matter.
    fn close_tab_now(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        if let TabContent::Session(session) = &self.tabs[index] {
            close(session.read(cx).connection());
        }
        // A connection manager on its way out has nothing left to restore into.
        if let TabContent::Connect(welcome) = &self.tabs[index] {
            let welcome = welcome.entity_id();
            self.pending.remove(&welcome);
        }
        self.tabs.remove(index);

        if self.tabs.is_empty() {
            // The window always shows something.
            self.open_connect_tab(window, cx);
            return;
        }

        // Closing a tab to the left of the active one shifts it along; the
        // tab the user was looking at stays in front either way.
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    fn on_welcome_event(
        &mut self,
        welcome: &Entity<Welcome>,
        event: &WelcomeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let WelcomeEvent::Connected(connection) = event;
        let session = cx.new(|cx| Session::new(connection.clone(), window, cx));
        cx.subscribe_in(&session, window, Self::on_session_event)
            .detach();

        // A startup restore parked this tab's contents here; a manual connect
        // has nothing pending. The connection id is checked because a user who
        // reconnects somewhere else from a failed restore's launcher should not
        // get the old connection's tabs. The entry stays in `pending` until the
        // tab has been swapped, so a checkpoint taken while the tabs are being
        // rebuilt still records them.
        let pending = self.pending.get(&welcome.entity_id()).cloned();
        if let Some(state) = pending
            && state.connection == connection.config.id
        {
            session.update(cx, |session, cx| session.restore(state, window, cx));
        }

        // The tab the connection was opened from becomes the connection. Its
        // tab being gone would mean it was closed mid-connect, and the pool is
        // open either way, so it gets a tab of its own instead.
        match self.tab_of(|tab| matches!(tab, TabContent::Connect(open) if open == welcome)) {
            Some(index) => {
                self.tabs[index] = TabContent::Session(session);
                // The tab is already in front: a manual connect is started from
                // the tab showing it, and a restore has already put the saved
                // tab in front. Stepping to whichever connection finished last
                // would lose that, so `active` is left alone.
            }
            None => {
                self.tabs.push(TabContent::Session(session));
                self.active = self.tabs.len() - 1;
            }
        }
        self.pending.remove(&welcome.entity_id());

        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    fn on_session_event(
        &mut self,
        session: &Entity<Session>,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            // Something about the session's tabs changed; the saved state is
            // rewritten at these checkpoints rather than on every keystroke.
            SessionEvent::Changed => self.save_state(cx),
            SessionEvent::Disconnected => {
                // Disconnecting drops the session and every tab in it, so ask
                // first when any of them holds work that was never written —
                // the same guard closing the tab uses.
                if session.read(cx).has_unsaved_changes(cx) {
                    self.confirm_disconnect(session.clone(), window, cx);
                    return;
                }
                self.disconnect(session.clone(), window, cx);
            }
        }
    }

    /// The window's tabs as they would be restored.
    ///
    /// Only session tabs are recorded: a blank connection manager is scratch
    /// form state, so it is neither saved nor restored. A manager still holding
    /// a pending restore is recorded from that state, so a failed reconnect
    /// does not erase the user's tabs from the file.
    pub(crate) fn snapshot(&self, cx: &App) -> WorkspaceState {
        let mut sessions = Vec::new();
        let mut active = 0;

        for (index, tab) in self.tabs.iter().enumerate() {
            let state = match tab {
                TabContent::Session(session) => Some(session.read(cx).snapshot(cx)),
                TabContent::Connect(welcome) => self.pending.get(&welcome.entity_id()).cloned(),
            };

            if let Some(state) = state {
                if index == self.active {
                    active = sessions.len();
                }
                sessions.push(state);
            }
        }

        WorkspaceState {
            active,
            sessions,
            ..WorkspaceState::default()
        }
    }

    /// Write the current state, unless it is what was written last.
    ///
    /// The file is small but the UI thread should not wait on it, so the write
    /// goes to the background.
    fn save_state(&mut self, cx: &mut Context<Self>) {
        let state = self.snapshot(cx);
        if self.last_saved.as_ref() == Some(&state) {
            return;
        }
        self.last_saved = Some(state.clone());

        cx.background_spawn(async move {
            if let Err(error) = workspace_state::save(&state) {
                eprintln!("could not save the workspace: {error:#}");
            }
        })
        .detach();
    }

    /// Write the current state now, for the process that is about to end.
    ///
    /// Synchronous on purpose: this is the last chance to record text typed
    /// since the last checkpoint, and a background write would race the
    /// shutdown that is already under way.
    pub(crate) fn flush_state(&mut self, cx: &mut Context<Self>) {
        let state = self.snapshot(cx);
        if self.last_saved.as_ref() == Some(&state) {
            return;
        }
        self.last_saved = Some(state.clone());

        if let Err(error) = workspace_state::save(&state) {
            eprintln!("could not save the workspace: {error:#}");
        }
    }

    /// Show the value a grid asked for, if one is waiting.
    ///
    /// A cell has no window to build a text box with, so it leaves the value
    /// in a global and the dialog is opened here, where there is one. The
    /// surface is `gpui_kit`'s `Dialog`, so the focus trap and the overlay
    /// that keeps mouse events off the grid behind come with it; the view
    /// this workspace holds is only the body inside it.
    fn take_value_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = value_dialog::take(cx) else {
            return;
        };

        let view = cx.new(|cx| ValueView::new(request, window, cx));
        // Saving or dismissing from inside the body closes the dialog from
        // this side; the routes the dialog answers itself (its close button,
        // a press on the overlay) come back through `on_close` below.
        cx.subscribe_in(&view, window, |this, _, _: &Dismissed, window, cx| {
            this.value = None;
            window.close_dialog(cx);
            this.focus_active(window, cx);
            cx.notify();
        })
        .detach();

        let workspace = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let workspace = workspace.clone();
            dialog
                // The body carries its own heading and its own buttons, so
                // the dialog contributes the surface alone.
                .w(VALUE_DIALOG_WIDTH)
                .h(VALUE_DIALOG_HEIGHT)
                .close_button(false)
                // `enter` is the text box's own key and the box is
                // multi-line, so the dialog's keys are left to `ValueDialog`.
                .keyboard(false)
                .on_close(move |_, _window, cx| {
                    workspace
                        .update(cx, |this, cx| {
                            this.value = None;
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        view.update(cx, |view, cx| view.focus(window, cx));
        self.value = Some(view);
    }

    fn tab_of(&self, held_by: impl Fn(&TabContent) -> bool) -> Option<usize> {
        self.tabs.iter().position(held_by)
    }

    fn on_new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connect_tab(window, cx);
    }

    fn on_show_shortcuts(
        &mut self,
        _: &ShowShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        shortcuts_dialog::open(window, cx);
    }

    fn on_close_connection(
        &mut self,
        _: &CloseConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab(self.active, window, cx);
    }

    fn on_next_connection(
        &mut self,
        _: &NextConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step(1, window, cx);
    }

    fn on_previous_connection(
        &mut self,
        _: &PreviousConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step(-1, window, cx);
    }

    /// Move `delta` tabs along the bar, wrapping at either end.
    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count < 2 {
            return;
        }

        let next = (self.active as isize + delta).rem_euclid(count);
        self.activate_tab(next as usize, window, cx);
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = cx.entity().downgrade();

        TabBar::new("connection-tabs")
            .with_variant(TabVariant::Outline)
            .p_1()
            .selected_index(self.active)
            .on_click(
                cx.listener(|this, index: &usize, window, cx| {
                    this.activate_tab(*index, window, cx)
                }),
            )
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let close_button = workspace.clone();
                let middle_click = workspace.clone();

                Tab::new()
                    .label(tab.label(cx))
                    .px_3()
                    // The engine icon and, for a tagged connection, the tag
                    // chip ride in the prefix; `.icon()` renders icon-only.
                    .when_some(tab.prefix(cx), |this, prefix| this.prefix(prefix))
                    // Middle-click closes, the way it does in a browser.
                    .on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        if let Some(workspace) = middle_click.upgrade() {
                            workspace.update(cx, |this, cx| this.close_tab(index, window, cx));
                        }
                    })
                    .suffix(
                        Button::new(SharedString::from(format!("close-connection-{index}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(format!("Close {}", tab.title(cx)))
                            .tooltip_with_action(
                                "Close connection",
                                &CloseConnection,
                                Some("Workspace"),
                            )
                            .on_click(move |_, window, cx| {
                                if let Some(workspace) = close_button.upgrade() {
                                    workspace
                                        .update(cx, |this, cx| this.close_tab(index, window, cx));
                                }
                            }),
                    )
            }))
    }

    /// The session showing in the active tab, if any.
    fn active_session(&self) -> Option<Entity<Session>> {
        match self.tabs.get(self.active)? {
            TabContent::Session(session) => Some(session.clone()),
            TabContent::Connect(_) => None,
        }
    }

    fn on_quick_switcher_click(
        &mut self,
        _: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.active_session() {
            crate::ui::quick_switcher::open(session, window, cx);
        }
    }

    fn on_quick_switcher(
        &mut self,
        _: &QuickSwitcher,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.active_session() {
            crate::ui::quick_switcher::open(session, window, cx);
        }
    }

    fn on_search_schema(&mut self, _: &SearchSchema, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.active_session() {
            crate::ui::schema_search::open(session, window, cx);
        }
    }

    fn on_refresh_click(
        &mut self,
        _: &gpui_kit::ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.active_session() {
            session.update(cx, |session, cx| session.refresh(cx));
        }
    }

    fn on_new_query_click(
        &mut self,
        _: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.active_session() {
            session.update(cx, |session, cx| session.new_query_tab(window, cx));
        }
    }

    /// The window's own bar: who we are connected to on the left, the actions
    /// that apply to it on the right, OS buttons inset by the component.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.active_session();
        let connected = session.is_some();

        TitleBar::new()
            .w_full()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .when_some(session.clone(), |this, session| {
                        let (name, target) = {
                            let session = session.read(cx);
                            (session.display_name(), session.display_target())
                        };
                        this.child(div().text_sm().truncate().max_w(px(130.)).child(name))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .max_w(px(200.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(target),
                            )
                            // The bar starts a window move on mouse-move while
                            // held, so interactive children swallow their own
                            // press to avoid dragging the window instead.
                            .child(
                                h_flex()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(Session::render_database_picker(&session, cx)),
                            )
                    }),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // Every button here is icon-only, so each one carries the
                    // name a screen reader announces: without it there is
                    // nothing to announce but the icon's file.
                    .child(
                        Button::new("quick-switcher")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::Search)
                            .accessibility_label("Quick switcher")
                            .tooltip_with_action("Quick switcher", &QuickSwitcher, Some("Session"))
                            .disabled(!connected)
                            .on_click(cx.listener(Self::on_quick_switcher_click)),
                    )
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::RefreshCw)
                            .accessibility_label("Refresh")
                            .tooltip_with_action("Refresh", &Refresh, Some("Session"))
                            .disabled(!connected)
                            .on_click(cx.listener(Self::on_refresh_click)),
                    )
                    .child(
                        Button::new("new-query")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::FilePlus)
                            .accessibility_label("New query")
                            .tooltip_with_action("New query", &NewTab, Some("Session"))
                            .disabled(!connected)
                            .on_click(cx.listener(Self::on_new_query_click)),
                    )
                    .child(
                        Button::new("new-connection")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::DatabasePlus)
                            .accessibility_label("Open another connection")
                            .tooltip_with_action(
                                "Open another connection",
                                &NewConnection,
                                Some("Workspace"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_connect_tab(window, cx)
                                }),
                            ),
                    )
                    // The settings used to be reachable only by a keystroke
                    // nobody is told about, so they get a button of their own.
                    .child(
                        Button::new("settings")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::Settings)
                            .accessibility_label("Settings")
                            .tooltip_with_action("Settings", &OpenSettings, None)
                            .on_click(|_, _window, cx| settings_window::open(cx)),
                    ),
            )
    }
}

/// Drain the pool in the background; the UI does not wait for it.
fn close(connection: Arc<Connection>) {
    runtime::spawn(async move { connection.close().await });
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.take_value_dialog(window, cx);
        // Taking it first puts the dialog in the root's list in time for this
        // layer, so it is on screen in the frame that asked for it.
        let dialogs = Root::render_dialog_layer(window, cx);

        let body = match &self.tabs[self.active] {
            TabContent::Connect(welcome) => welcome.clone().into_any_element(),
            TabContent::Session(session) => session.clone().into_any_element(),
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(Self::on_new_connection))
            .on_action(cx.listener(Self::on_close_connection))
            .on_action(cx.listener(Self::on_show_shortcuts))
            .on_action(cx.listener(Self::on_next_connection))
            .on_action(cx.listener(Self::on_previous_connection))
            .on_action(cx.listener(Self::on_quick_switcher))
            .on_action(cx.listener(Self::on_search_schema))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .w_full()
                    .flex_none()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(self.render_tab_bar(cx)),
            )
            .child(div().flex_1().min_h_0().child(body))
            // The value dialog lives in the window's `Root` rather than in the
            // tree above, so it covers the whole window and takes the focus.
            .children(dialogs)
    }
}

#[cfg(test)]
impl Workspace {
    /// Build a workspace from an injected state, so a test never reads the
    /// developer's real workspace file.
    pub(crate) fn with_state_for_test(
        state: WorkspaceState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(state, window, cx)
    }

    /// The state this workspace would write, for a test to check.
    pub(crate) fn state_for_test(&self, cx: &App) -> WorkspaceState {
        self.snapshot(cx)
    }

    pub(crate) fn tab_titles_for_test(&self, cx: &App) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| tab.title(cx).to_string())
            .collect()
    }

    pub(crate) fn active_for_test(&self) -> usize {
        self.active
    }

    /// The connection manager in the active tab, to connect from.
    pub(crate) fn value_dialog_for_test(&self) -> Option<Entity<ValueView>> {
        self.value.clone()
    }

    pub(crate) fn active_welcome_for_test(&self) -> Option<Entity<Welcome>> {
        match self.tabs.get(self.active)? {
            TabContent::Connect(welcome) => Some(welcome.clone()),
            TabContent::Session(_) => None,
        }
    }

    pub(crate) fn active_session_for_test(&self) -> Option<Entity<Session>> {
        match self.tabs.get(self.active)? {
            TabContent::Session(session) => Some(session.clone()),
            TabContent::Connect(_) => None,
        }
    }
}
