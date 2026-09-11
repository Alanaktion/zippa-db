//! Root view: every open connection in a tab of its own.
//!
//! A tab holds either the connection manager or an open session. Connecting
//! turns the tab it was opened from into that session and disconnecting turns
//! it back, so a connection is always somewhere on the bar rather than
//! replacing what the window was showing. The window keeps at least one tab,
//! the way the session keeps at least one editor.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar, TabVariant};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, TitleBar, h_flex, v_flex,
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, MouseButton, SharedString, Window, actions, div, px,
};
use gpui_kit::prelude::*;

use crate::db::{Connection, runtime};
use crate::settings;
use crate::ui::session::{Session, SessionEvent};
use crate::ui::welcome::{Welcome, WelcomeEvent};

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
}

pub struct Workspace {
    tabs: Vec<TabContent>,
    active: usize,
    /// Held by the window itself, so the connection shortcuts answer even when
    /// nothing inside a tab has the caret.
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            focus,
        };
        workspace.open_connect_tab(window, cx);
        workspace
    }

    /// Add a tab showing the connection manager and make it active.
    fn open_connect_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let welcome = Self::welcome(window, cx);
        self.tabs.push(TabContent::Connect(welcome));
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
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
            // Nothing in the connection manager asks for the caret, and focus
            // left behind in a hidden tab would keep answering keystrokes.
            _ => self.focus.focus(window, cx),
        }
    }

    /// Close a tab, disconnecting it if it holds a connection.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        if let TabContent::Session(session) = &self.tabs[index] {
            close(session.read(cx).connection());
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

        // The tab the connection was opened from becomes the connection. Its
        // tab being gone would mean it was closed mid-connect, and the pool is
        // open either way, so it gets a tab of its own instead.
        match self.tab_of(|tab| matches!(tab, TabContent::Connect(open) if open == welcome)) {
            Some(index) => {
                self.tabs[index] = TabContent::Session(session);
                self.active = index;
            }
            None => {
                self.tabs.push(TabContent::Session(session));
                self.active = self.tabs.len() - 1;
            }
        }

        self.focus_active(window, cx);
        cx.notify();
    }

    fn on_session_event(
        &mut self,
        session: &Entity<Session>,
        event: &SessionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SessionEvent::Disconnected = event;
        let Some(index) =
            self.tab_of(|tab| matches!(tab, TabContent::Session(open) if open == session))
        else {
            return;
        };

        close(session.read(cx).connection());
        // Disconnecting keeps the tab, so another connection can be opened
        // from where the last one was.
        self.tabs[index] = TabContent::Connect(Self::welcome(window, cx));
        cx.notify();
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
            .with_variant(TabVariant::Pill)
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
                    .label(tab.title(cx))
                    .px_3()
                    // `.icon()` on this widget renders icon-only, so icon
                    // goes in `.prefix()` instead
                    .when_some(tab.icon(cx), |this, icon| this.prefix(Icon::new(icon)))
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
                            .tooltip("Close connection")
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
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::RefreshCw)
                            .tooltip("Refresh")
                            .disabled(!connected)
                            .on_click(cx.listener(Self::on_refresh_click)),
                    )
                    .child(
                        Button::new("new-query")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::FilePlus)
                            .tooltip("New query")
                            .disabled(!connected)
                            .on_click(cx.listener(Self::on_new_query_click)),
                    )
                    .child(
                        Button::new("new-connection")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::DatabasePlus)
                            .tooltip("Open another connection")
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.open_connect_tab(window, cx)
                                }),
                            ),
                    ),
            )
    }
}

/// Drain the pool in the background; the UI does not wait for it.
fn close(connection: Arc<Connection>) {
    runtime::spawn(async move { connection.close().await });
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_action(cx.listener(Self::on_next_connection))
            .on_action(cx.listener(Self::on_previous_connection))
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
    }
}

#[cfg(test)]
impl Workspace {
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
