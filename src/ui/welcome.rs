//! Connection manager: the screen shown before a database is opened.
//!
//! Covers TODO.md section 1: saved connections, per-engine settings, and
//! opening a connection. Passwords live in the OS keychain, never in the
//! config file.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Selectable, Sizable, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, Window, div, px};
use uuid::Uuid;

use crate::db::{Connection, ConnectionConfig, Engine, runtime, store};

pub enum WelcomeEvent {
    /// A connection was opened and the session can start.
    Connected(Arc<Connection>),
}

enum Status {
    Idle,
    Connecting,
    Message(String),
    Error(String),
}

pub struct Welcome {
    connections: Vec<ConnectionConfig>,
    /// `None` while editing a connection that has not been saved.
    selected: Option<Uuid>,
    engine: Engine,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    status: Status,
}

impl EventEmitter<WelcomeEvent> for Welcome {}

impl Welcome {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (connections, status) = match store::load() {
            Ok(connections) => (connections, Status::Idle),
            Err(error) => (Vec::new(), Status::Error(format!("{error:#}"))),
        };

        let engine = Engine::Postgres;
        Self {
            connections,
            selected: None,
            engine,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Local Postgres")),
            host: cx.new(|cx| InputState::new(window, cx).default_value("localhost")),
            port: cx.new(|cx| {
                InputState::new(window, cx).default_value(engine.default_port().to_string())
            }),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("postgres")),
            password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            database: cx.new(|cx| InputState::new(window, cx).placeholder("postgres")),
            status,
        }
    }

    /// Read the form into a config, reusing the selected connection's id.
    fn config(&self, cx: &App) -> ConnectionConfig {
        let port = self
            .port
            .read(cx)
            .value()
            .trim()
            .parse()
            .unwrap_or_else(|_| self.engine.default_port());

        ConnectionConfig {
            id: self.selected.unwrap_or_else(Uuid::new_v4),
            name: self.name.read(cx).value().trim().to_string(),
            engine: self.engine,
            host: self.host.read(cx).value().trim().to_string(),
            port,
            username: self.username.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
        }
    }

    fn set_field(
        &self,
        field: &Entity<InputState>,
        value: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        field.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx)
        });
    }

    fn load(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
        else {
            return;
        };

        self.selected = Some(config.id);
        self.engine = config.engine;
        self.set_field(&self.name.clone(), &config.name, window, cx);
        self.set_field(&self.host.clone(), &config.host, window, cx);
        self.set_field(&self.port.clone(), &config.port.to_string(), window, cx);
        self.set_field(&self.username.clone(), &config.username, window, cx);
        self.set_field(&self.database.clone(), &config.database, window, cx);

        // Reading the keychain can prompt the user, so only do it on demand.
        match store::password(&config.id) {
            Ok(password) => {
                self.set_field(
                    &self.password.clone(),
                    password.as_deref().unwrap_or(""),
                    window,
                    cx,
                );
                self.status = Status::Idle;
            }
            Err(error) => self.status = Status::Error(format!("{error:#}")),
        }
        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        self.engine = Engine::Postgres;
        self.set_field(&self.name.clone(), "", window, cx);
        self.set_field(&self.host.clone(), "localhost", window, cx);
        self.set_field(
            &self.port.clone(),
            &Engine::Postgres.default_port().to_string(),
            window,
            cx,
        );
        self.set_field(&self.username.clone(), "", window, cx);
        self.set_field(&self.password.clone(), "", window, cx);
        self.set_field(&self.database.clone(), "", window, cx);
        self.status = Status::Idle;
        cx.notify();
    }

    fn set_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if self.engine == engine {
            return;
        }

        // Keep the port in step unless the user typed their own.
        let port = self.port.read(cx).value().trim().to_string();
        if port.is_empty() || port == self.engine.default_port().to_string() {
            self.set_field(
                &self.port.clone(),
                &engine.default_port().to_string(),
                window,
                cx,
            );
        }

        self.engine = engine;
        cx.notify();
    }

    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config(cx);
        let password = self.password.read(cx).value().to_string();

        match self
            .connections
            .iter()
            .position(|saved| saved.id == config.id)
        {
            Some(index) => self.connections[index] = config.clone(),
            None => self.connections.push(config.clone()),
        }

        self.status = match store::save(&self.connections)
            .and_then(|()| store::set_password(&config.id, &password))
        {
            Ok(()) => {
                self.selected = Some(config.id);
                Status::Message(format!("Saved {}", config.display_name()))
            }
            Err(error) => Status::Error(format!("{error:#}")),
        };
        cx.notify();
    }

    fn delete(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.connections.retain(|config| config.id != id);

        if let Err(error) =
            store::save(&self.connections).and_then(|()| store::delete_password(&id))
        {
            self.status = Status::Error(format!("{error:#}"));
        }
        if self.selected == Some(id) {
            self.reset(window, cx);
        }
        cx.notify();
    }

    fn connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let config = self.config(cx);
        let password = self.password.read(cx).value().to_string();
        let password = (!password.is_empty()).then_some(password);

        self.status = Status::Connecting;
        cx.notify();

        let task = runtime::spawn(async move { Connection::open(config, password).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(connection)) => {
                        this.status = Status::Idle;
                        cx.emit(WelcomeEvent::Connected(Arc::new(connection)));
                    }
                    Ok(Err(error)) => this.status = Status::Error(format!("{error:#}")),
                    Err(_) => this.status = Status::Error("the connection was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put the footer into its error state without a live connection attempt.
    #[cfg(test)]
    pub(crate) fn show_error_for_test(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.status = Status::Error(message.into());
        cx.notify();
    }

    fn is_connecting(&self) -> bool {
        matches!(self.status, Status::Connecting)
    }

    fn render_saved(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(260.))
            .h_full()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("SAVED CONNECTIONS"),
            )
            .when(self.connections.is_empty(), |this| {
                this.child(
                    div()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Nothing saved yet"),
                )
            })
            .children(self.connections.iter().map(|config| {
                let id = config.id;
                let selected = self.selected == Some(id);

                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("open-{id}")))
                            .ghost()
                            .selected(selected)
                            .w_full()
                            .label(format!(
                                "{}  ·  {}",
                                config.display_name(),
                                config.engine.label()
                            ))
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.load(id, window, cx)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("delete-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Delete connection")
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.delete(id, window, cx)),
                            ),
                    )
            }))
            .child(
                Button::new("new-connection")
                    .outline()
                    .w_full()
                    .label("New connection")
                    .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
            )
    }

    fn render_engines(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().gap_2().children(Engine::ALL.map(|engine| {
            let button = Button::new(SharedString::from(format!("engine-{}", engine.label())))
                .label(engine.label())
                .on_click(
                    cx.listener(move |this, _, window, cx| this.set_engine(engine, window, cx)),
                );

            if self.engine == engine {
                button.primary()
            } else {
                button.outline()
            }
        }))
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let file_based = self.engine.is_file_based();

        v_flex()
            .flex_1()
            .gap_3()
            .child(self.render_engines(cx))
            .child(field("Name", &self.name, cx))
            .when(file_based, |this| {
                this.child(field("Database file", &self.database, cx))
            })
            .when(!file_based, |this| {
                this.child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(field("Host", &self.host, cx)))
                        .child(div().w(px(96.)).child(field("Port", &self.port, cx))),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(field("User", &self.username, cx)))
                        .child(div().flex_1().child(field("Password", &self.password, cx))),
                )
                .child(field("Database", &self.database, cx))
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (message, color) = match &self.status {
            Status::Idle => (String::new(), cx.theme().muted_foreground),
            Status::Connecting => ("Connecting…".to_string(), cx.theme().muted_foreground),
            Status::Message(message) => (message.clone(), cx.theme().muted_foreground),
            Status::Error(error) => (error.clone(), cx.theme().danger),
        };

        v_flex()
            .w_full()
            .flex_none()
            .gap_2()
            // A server error can run to several lines, so it gets its own row
            // above the buttons instead of squeezing them out of the window.
            .when(!message.is_empty(), |this| {
                this.child(
                    div()
                        .id("status")
                        .w_full()
                        .max_h(px(96.))
                        .overflow_y_scroll()
                        .text_sm()
                        .text_color(color)
                        .child(message),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("save")
                            .outline()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        Button::new("connect")
                            .primary()
                            .label("Connect")
                            .disabled(self.is_connecting())
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    ),
            )
    }
}

fn field(label: &str, input: &Entity<InputState>, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(Input::new(input))
}

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p_6()
            .gap_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xl().child("Zippa DB"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Pick a saved connection or set up a new one."),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_6()
                    .items_start()
                    .child(self.render_saved(cx))
                    .child(self.render_form(cx)),
            )
            .child(self.render_footer(cx))
    }
}
