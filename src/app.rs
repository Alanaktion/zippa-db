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
    ActiveTheme, Disableable, Icon, IconName, Root, Sizable, TitleBar, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, FocusHandle, MouseButton, Pixels, SharedString, Window, actions, div, px,
};

use crate::db::{Connection, runtime};
use crate::settings;
use crate::ui::session::{NewTab, Refresh, Session, SessionEvent};
use crate::ui::settings_window::{self, OpenSettings};
use crate::ui::value_dialog::{self, Dismissed, ValueView};
use crate::ui::welcome::{Welcome, WelcomeEvent};

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
}

pub struct Workspace {
    tabs: Vec<TabContent>,
    active: usize,
    /// The value dialog, while one is open over the window.
    value: Option<Entity<ValueView>>,
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
            value: None,
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
            // The value dialog lives in the window's `Root` rather than in the
            // tree above, so it covers the whole window and takes the focus.
            .children(dialogs)
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
