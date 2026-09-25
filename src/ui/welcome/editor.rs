//! The connection editor: the form that was once half of the welcome screen,
//! now a dialog opened from the launcher.
//!
//! [`ConnectionEditor`] is only the *body* of the dialog; the surface around it
//! is `gpui_kit`'s `Dialog`, opened by [`Welcome`](super::Welcome). It emits
//! [`EditorEvent`] and the launcher reacts, the way [`Session`](crate::ui::session::Session)
//! reacts to its panels.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, Window, actions, div};
use uuid::Uuid;

use crate::db::{ConnectionConfig, Engine, SafetyMode, TagColor, is_risky_auto_apply};

actions!(zippa_db, [EditorClose, EditorConnect]);

/// What the editor asks its owner to do.
pub enum EditorEvent {
    /// Save the connection without connecting.
    ///
    /// `password` is `None` when the user never touched the box, so a save that
    /// was not about the password leaves the stored one alone.
    Saved {
        config: ConnectionConfig,
        password: Option<String>,
    },
    /// Connect immediately with this config. `password` is `None` when the box
    /// is blank, which is no password rather than the stored one.
    Connect {
        config: ConnectionConfig,
        password: Option<String>,
    },
    /// The dialog was dismissed without saving or connecting.
    Dismissed,
}

/// A tag preset: one click fills both the tag text and its colour.
const PRESETS: [(&str, TagColor); 4] = [
    ("Production", TagColor::Red),
    ("Staging", TagColor::Orange),
    ("Development", TagColor::Blue),
    ("Local", TagColor::Green),
];

pub struct ConnectionEditor {
    /// `None` while editing a connection that has not been saved.
    id: Option<Uuid>,
    engine: Engine,
    safety: SafetyMode,
    color: Option<TagColor>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    tag: Entity<InputState>,
    /// Whether the user has typed in the password box since the dialog opened.
    /// The stored password is never loaded into it, so an untouched box has to
    /// mean "leave it alone" rather than "no password".
    password_edited: bool,
}

impl EventEmitter<EditorEvent> for ConnectionEditor {}

impl ConnectionEditor {
    /// Build the form for a new connection, or pre-filled from `config`.
    pub fn new(
        config: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = config
            .as_ref()
            .map(|c| c.engine)
            .unwrap_or(Engine::Postgres);
        let editing = config.is_some();

        let mut editor = Self {
            id: config.as_ref().map(|c| c.id),
            engine,
            safety: config.as_ref().map(|c| c.safety).unwrap_or_default(),
            color: config.as_ref().and_then(|c| c.color),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Local Postgres")),
            host: cx.new(|cx| InputState::new(window, cx).default_value("localhost")),
            port: cx.new(|cx| {
                InputState::new(window, cx).default_value(engine.default_port().to_string())
            }),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("postgres")),
            password: cx.new(|cx| {
                let input = InputState::new(window, cx).masked(true);
                // A saved connection's password is never read back into the
                // form, so the placeholder is what tells the user that leaving
                // it blank keeps what is already stored.
                if editing {
                    input.placeholder("Leave blank to keep the saved password")
                } else {
                    input
                }
            }),
            database: cx.new(|cx| InputState::new(window, cx).placeholder("postgres")),
            tag: cx.new(|cx| InputState::new(window, cx).placeholder("Production")),
            password_edited: false,
        };

        if let Some(config) = config {
            editor.load(&config, window, cx);
        }

        // Subscribed after `load`, so filling the form in is not mistaken for
        // the user typing: only a change made from here on is a new password.
        let password = editor.password.clone();
        cx.subscribe_in(&password, window, Self::on_password_event)
            .detach();

        editor
    }

    /// Note that the user typed in the password box.
    ///
    /// This is what tells an untouched box (keep the stored password) apart
    /// from one the user emptied (forget it).
    fn on_password_event(
        &mut self,
        _password: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.password_edited = true;
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

    /// Fill the form in from a saved connection.
    fn load(&mut self, config: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.engine = config.engine;
        self.safety = config.safety;
        self.color = config.color;
        self.set_field(&self.name.clone(), &config.name, window, cx);
        self.set_field(&self.host.clone(), &config.host, window, cx);
        self.set_field(&self.port.clone(), &config.port.to_string(), window, cx);
        self.set_field(&self.username.clone(), &config.username, window, cx);
        self.set_field(&self.database.clone(), &config.database, window, cx);
        self.set_field(
            &self.tag.clone(),
            config.tag.as_deref().unwrap_or(""),
            window,
            cx,
        );
        self.set_field(&self.password.clone(), "", window, cx);
    }

    /// Put the caret in the first field the user is about to fill in.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Read the form into a config, reusing the saved connection's id.
    fn config(&self, cx: &App) -> ConnectionConfig {
        let port = self
            .port
            .read(cx)
            .value()
            .trim()
            .parse()
            .unwrap_or_else(|_| self.engine.default_port());

        let tag = self.tag.read(cx).value().trim().to_string();
        let tag = (!tag.is_empty()).then_some(tag);

        ConnectionConfig {
            id: self.id.unwrap_or_else(Uuid::new_v4),
            name: self.name.read(cx).value().trim().to_string(),
            engine: self.engine,
            host: self.host.read(cx).value().trim().to_string(),
            port,
            username: self.username.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
            safety: self.safety,
            tag,
            color: self.color,
            last_connected: None,
        }
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

    fn apply_preset(
        &mut self,
        tag: &str,
        color: TagColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_field(&self.tag.clone(), tag, window, cx);
        self.color = Some(color);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let config = self.config(cx);
        let password = self.password(cx);
        cx.emit(EditorEvent::Saved { config, password });
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let config = self.config(cx);
        // A blank box means no password here: connecting cannot pick up the
        // stored one from inside the dialog.
        let password = self.password(cx).filter(|password| !password.is_empty());
        cx.emit(EditorEvent::Connect { config, password });
    }

    /// What the password box means for the keychain.
    ///
    /// `None` when the user never touched it, so an edit that was not about the
    /// password leaves the stored one in place; `Some("")` when they emptied
    /// it, which is how a saved password is forgotten.
    fn password(&self, cx: &App) -> Option<String> {
        self.password_edited
            .then(|| self.password.read(cx).value().to_string())
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Dismissed);
    }

    fn on_close(&mut self, _: &EditorClose, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn on_connect(&mut self, _: &EditorConnect, _window: &mut Window, cx: &mut Context<Self>) {
        self.connect(cx);
    }

    fn render_engines(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().gap_2().children(Engine::ALL.map(|engine| {
            let button = Button::new(SharedString::from(format!("engine-{}", engine.label())))
                .label(engine.label())
                // The logo travels with the label so the engine is picked by
                // sight as well as by reading; the button's own variant colours
                // it, light on the selected one.
                .icon(crate::ui::engine_icon(engine))
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

    /// The tag text (with its presets) and the colour swatches, on one line.
    fn render_tag(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_3()
                    .items_end()
                    .child(div().flex_1().child(field("Label", &self.tag, cx)))
                    .child(self.render_colors(cx)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(PRESETS.map(|(tag, color)| {
                        Button::new(SharedString::from(format!("preset-{tag}")))
                            .ghost()
                            .xsmall()
                            .label(tag)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply_preset(tag, color, window, cx)
                            }))
                    })),
            )
    }

    /// A row of colour swatches, plus "None"; the selected one is ringed and
    /// carries a check so the state is not told by colour alone.
    fn render_colors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Colour"),
            )
            .child(
                h_flex()
                    .gap_1()
                    .children(TagColor::ALL.map(|color| {
                        let selected = self.color == Some(color);
                        Button::new(SharedString::from(format!("color-{}", color.key())))
                            .xsmall()
                            .rounded_full()
                            .bg(color.hsla(cx))
                            .selected(selected)
                            .accessibility_label(color.label())
                            .tooltip(color.label())
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.color = Some(color);
                                cx.notify();
                            }))
                            .when(selected, |this| {
                                this.child(
                                    Icon::new(IconName::Check).text_color(color.on_color(cx)),
                                )
                            })
                    }))
                    .child(
                        Button::new("color-none")
                            .ghost()
                            .xsmall()
                            .label("None")
                            .selected(self.color.is_none())
                            .tooltip("No colour")
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.color = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// Pick how careful this connection is about writes.
    fn render_safety(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tag = self.tag.read(cx).value().trim().to_string();
        let hint = is_risky_auto_apply(&tag, self.safety);

        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Inline edits"),
            )
            .when(hint, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child("Writes apply immediately on a production connection."),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(SafetyMode::ALL.map(|safety| {
                        let button = Button::new(SharedString::from(format!("safety-{safety:?}")))
                            .label(safety.label())
                            .tooltip(safety.description())
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.safety = safety;
                                cx.notify();
                            }));

                        if self.safety == safety {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })),
            )
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let file_based = self.engine.is_file_based();

        v_flex()
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
                        .child(div().w_24().child(field("Port", &self.port, cx))),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().child(field("User", &self.username, cx)))
                        .child(div().flex_1().child(field("Password", &self.password, cx))),
                )
                .child(field("Database", &self.database, cx))
            })
            .child(self.render_tag(cx))
            .child(self.render_safety(cx))
    }

    /// The password this form would write to the keychain, for a test to read.
    #[cfg(test)]
    pub(crate) fn password_intent_for_test(&self, cx: &App) -> Option<String> {
        self.password(cx)
    }

    /// Put the caret in the password box, the way a click would.
    #[cfg(test)]
    pub(crate) fn focus_password_for_test(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.password
            .update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Render for ConnectionEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("connection-editor")
            .size_full()
            .gap_3()
            .key_context("ConnectionEditor")
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_connect))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(self.render_form(cx)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("editor-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _window, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("editor-save")
                            .outline()
                            .label("Save")
                            .tooltip("Save this connection without connecting")
                            .on_click(cx.listener(|this, _, _window, cx| this.save(cx))),
                    )
                    .child(
                        Button::new("editor-connect")
                            .primary()
                            .label("Connect")
                            .tooltip_with_action(
                                "Connect",
                                &EditorConnect,
                                Some("ConnectionEditor"),
                            )
                            .on_click(cx.listener(|this, _, _window, cx| this.connect(cx))),
                    ),
            )
    }
}

/// A label and its input, the form's repeated row.
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
