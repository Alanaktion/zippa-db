//! Root view: shows either the connection manager or an open session.

use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Window};

use crate::db::{Connection, runtime};
use crate::ui::session::{Session, SessionEvent};
use crate::ui::welcome::{Welcome, WelcomeEvent};

enum Screen {
    Welcome(Entity<Welcome>),
    Session(Entity<Session>),
}

pub struct Workspace {
    screen: Screen,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            screen: Screen::Welcome(Self::welcome(window, cx)),
        }
    }

    fn welcome(window: &mut Window, cx: &mut Context<Self>) -> Entity<Welcome> {
        let welcome = cx.new(|cx| Welcome::new(window, cx));
        cx.subscribe_in(&welcome, window, Self::on_welcome_event)
            .detach();
        welcome
    }

    fn on_welcome_event(
        &mut self,
        _: &Entity<Welcome>,
        event: &WelcomeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let WelcomeEvent::Connected(connection) = event;
        let session = cx.new(|cx| Session::new(connection.clone(), window, cx));
        cx.subscribe_in(&session, window, Self::on_session_event)
            .detach();
        self.screen = Screen::Session(session);
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
        close(session.read(cx).connection());
        self.screen = Screen::Welcome(Self::welcome(window, cx));
        cx.notify();
    }
}

/// Drain the pool in the background; the UI does not wait for it.
fn close(connection: Arc<Connection>) {
    runtime::spawn(async move { connection.close().await });
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(match &self.screen {
                Screen::Welcome(welcome) => welcome.clone().into_any_element(),
                Screen::Session(session) => session.clone().into_any_element(),
            })
    }
}
