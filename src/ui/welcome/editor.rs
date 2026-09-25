//! The connection editor: the form that was once half of the welcome screen,
//! now a dialog opened from the launcher.
//!
//! [`ConnectionEditor`] is only the *body* of the dialog; the surface around it
//! is `gpui_kit`'s `Dialog`, opened by [`Welcome`](super::Welcome). It emits
//! [`EditorEvent`] and the launcher reacts, the way [`Session`](crate::ui::session::Session)
//! reacts to its panels.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants, DropdownButton};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, Task, Window, actions, div, px};
use uuid::Uuid;

use crate::db::{
    Connection, ConnectionConfig, Engine, SafetyMode, TagColor, is_risky_auto_apply, runtime, store,
};

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
    /// Connect with this config, saving it first when `save` is set.
    ///
    /// `password` means what it does for [`EditorEvent::Saved`]: `None` for a
    /// box the user never touched, `Some("")` for one they emptied.
    Connect {
        config: ConnectionConfig,
        password: Option<String>,
        save: bool,
    },
    /// The dialog was dismissed without saving or connecting.
    Dismissed,
}

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
    /// Whether the user has typed in the password box since the dialog opened.
    /// The stored password is never loaded into it, so an untouched box has to
    /// mean "leave it alone" rather than "no password".
    password_edited: bool,
    /// What the form has to say below the fields: a test under way or its
    /// outcome, or why the inputs cannot be used.
    status: Status,
    /// The connection test under way, dropped (and so cancelled) with the
    /// editor or by the next test.
    test: Option<Task<()>>,
}

/// The line between the form and its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    None,
    Testing,
    Succeeded,
    Failed(String),
}

/// What each field shows while it is empty, for one engine: `(name, user,
/// database)`. The user and database are the ones a fresh server has; MySQL
/// needs no database at all, and SQLite's is a file.
fn placeholders(engine: Engine) -> (&'static str, &'static str, &'static str) {
    match engine {
        Engine::Postgres => ("Local PostgreSQL", "postgres", "postgres"),
        Engine::MySql => ("Local MySQL", "root", "Optional"),
        Engine::Sqlite if cfg!(windows) => ("Local SQLite", "", r"C:\path\to\database.db"),
        Engine::Sqlite => ("Local SQLite", "", "/path/to/database.db"),
    }
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
            name: cx.new(|cx| InputState::new(window, cx)),
            host: cx.new(|cx| InputState::new(window, cx).default_value("localhost")),
            port: cx.new(|cx| {
                InputState::new(window, cx).default_value(engine.default_port().to_string())
            }),
            username: cx.new(|cx| InputState::new(window, cx)),
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
            database: cx.new(|cx| InputState::new(window, cx)),
            password_edited: false,
            status: Status::None,
            test: None,
        };

        if let Some(config) = config {
            editor.load(&config, window, cx);
        }
        editor.set_placeholders(window, cx);

        // Subscribed after `load`, so filling the form in is not mistaken for
        // the user typing: only a change made from here on is a new password.
        let password = editor.password.clone();
        cx.subscribe_in(&password, window, Self::on_password_event)
            .detach();
        for field in editor.fields() {
            cx.subscribe_in(&field, window, Self::on_field_event)
                .detach();
        }

        editor
    }

    fn fields(&self) -> [Entity<InputState>; 6] {
        [
            self.name.clone(),
            self.host.clone(),
            self.port.clone(),
            self.username.clone(),
            self.password.clone(),
            self.database.clone(),
        ]
    }

    /// Show the current engine's examples in the empty fields.
    fn set_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, username, database) = placeholders(self.engine);
        for (field, placeholder) in [
            (&self.name, name),
            (&self.username, username),
            (&self.database, database),
        ] {
            field.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx)
            });
        }
    }

    /// A result reported for other inputs no longer says anything about
    /// these, so an edit clears it.
    fn on_field_event(
        &mut self,
        _field: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.clear_status(cx);
        }
    }

    /// Forget the last outcome, leaving a test still under way alone.
    fn clear_status(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.status, Status::None | Status::Testing) {
            self.status = Status::None;
            cx.notify();
        }
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

        ConnectionConfig {
            id: self.id.unwrap_or_else(Uuid::new_v4),
            name: self.name.read(cx).value().trim().to_string(),
            engine: self.engine,
            host: self.host.read(cx).value().trim().to_string(),
            port,
            username: self.username.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
            safety: self.safety,
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
        self.set_placeholders(window, cx);
        self.clear_status(cx);
        cx.notify();
    }

    /// Why the inputs cannot be used as they stand, in words for the user.
    ///
    /// An empty port is the engine's default rather than a mistake; one that
    /// is not a port number is, where it used to fall back to the default
    /// without a word.
    fn invalid(&self, cx: &App) -> Option<&'static str> {
        if self.engine.is_file_based() {
            return self
                .database
                .read(cx)
                .value()
                .trim()
                .is_empty()
                .then_some("Enter the path to the database file.");
        }

        if self.host.read(cx).value().trim().is_empty() {
            return Some("Enter the host to connect to.");
        }
        let port = self.port.read(cx).value().trim().to_string();
        if !port.is_empty() && !port.parse::<u16>().is_ok_and(|port| port > 0) {
            return Some("The port must be a number from 1 to 65535.");
        }
        None
    }

    /// Report invalid inputs below the form; `true` when there were any.
    fn refuse_invalid(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.invalid(cx) else {
            return false;
        };
        self.test = None;
        self.status = Status::Failed(message.to_string());
        cx.notify();
        true
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.refuse_invalid(cx) {
            return;
        }
        let config = self.config(cx);
        let password = self.password(cx);
        cx.emit(EditorEvent::Saved { config, password });
    }

    /// Connect, saving the connection first unless `save` is off.
    fn connect(&mut self, save: bool, cx: &mut Context<Self>) {
        if self.refuse_invalid(cx) {
            return;
        }
        let config = self.config(cx);
        let password = self.password(cx);
        cx.emit(EditorEvent::Connect {
            config,
            password,
            save,
        });
    }

    /// Open a connection with the form as it stands and close it again,
    /// reporting how it went without leaving the dialog.
    ///
    /// The password is the one a Connect would use: what the box holds once
    /// typed in, otherwise what the keychain holds for a saved connection.
    fn test_connection(&mut self, cx: &mut Context<Self>) {
        if self.refuse_invalid(cx) {
            return;
        }

        let config = self.config(cx);
        let typed = self.password(cx);
        let stored = typed.is_none() && self.id.is_some() && !config.engine.is_file_based();
        let id = config.id;
        // The keychain can prompt, or simply be slow, so it is read off the
        // UI thread like the launcher's own lookup.
        let password = cx.background_spawn(async move {
            match typed {
                Some(password) => Ok(Some(password).filter(|password| !password.is_empty())),
                None if stored => store::password(&id),
                None => Ok(None),
            }
        });

        self.status = Status::Testing;
        cx.notify();

        self.test = Some(cx.spawn(async move |this, cx| {
            let outcome = match password.await {
                Ok(password) => {
                    let task = runtime::spawn(async move {
                        let connection = Connection::open(config, password).await?;
                        connection.close().await;
                        anyhow::Ok(())
                    });
                    match task.await {
                        Ok(Ok(())) => Status::Succeeded,
                        Ok(Err(error)) => Status::Failed(format!("{error:#}")),
                        Err(_) => Status::Failed("the test was cancelled".into()),
                    }
                }
                Err(error) => Status::Failed(format!("{error:#}")),
            };
            this.update(cx, |this, cx| {
                this.status = outcome;
                this.test = None;
                cx.notify();
            })
            .ok();
        }));
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
        self.connect(true, cx);
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
                        let swatch = color.hsla(cx);
                        Button::new(SharedString::from(format!("color-{}", color.key())))
                            .xsmall()
                            .rounded_full()
                            // A custom variant rather than a plain `bg`, so the
                            // hover and press states stay the swatch's colour
                            // instead of the default button's.
                            .custom(
                                ButtonCustomVariant::new(cx)
                                    .color(swatch)
                                    .foreground(color.on_color(cx))
                                    .hover(swatch.opacity(0.85))
                                    .active(swatch.opacity(0.7)),
                            )
                            .bg(swatch)
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
        let name = self.name.read(cx).value().trim().to_string();
        let hint = is_risky_auto_apply(&name, self.color, self.safety);

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
            .child(self.render_colors(cx))
            .child(self.render_safety(cx))
    }

    /// The test's progress or outcome, or why the inputs were refused. Told in
    /// words as well as colour.
    fn render_status(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = cx.theme();
        let (icon, color, text) = match &self.status {
            Status::None => return None,
            Status::Testing => (
                IconName::Loader,
                theme.muted_foreground,
                "Testing connection…".to_string(),
            ),
            Status::Succeeded => (
                IconName::CircleCheck,
                theme.success,
                "Connection succeeded.".to_string(),
            ),
            Status::Failed(message) => {
                (IconName::CircleX, theme.danger, format!("Error: {message}"))
            }
        };

        Some(
            h_flex()
                .id("editor-status")
                .flex_none()
                .items_start()
                .gap_2()
                .text_sm()
                .text_color(color)
                .child(Icon::new(icon).flex_none().mt_0p5())
                .child(div().flex_1().min_w_0().child(text)),
        )
    }

    /// Connect, which saves first, with a menu for connecting without saving.
    fn render_connect(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity().downgrade();
        let unsaved = if self.id.is_some() {
            "Connect Without Saving Changes"
        } else {
            "Connect Without Saving"
        };

        DropdownButton::new("editor-connect-split")
            .primary()
            .button(
                Button::new("editor-connect")
                    .label("Connect")
                    .tooltip_with_action(
                        "Save and connect",
                        &EditorConnect,
                        Some("ConnectionEditor"),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.connect(true, cx))),
            )
            .dropdown_menu(move |menu, _window, _cx| {
                let editor = editor.clone();
                menu.item(PopupMenuItem::new(unsaved).on_click(move |_, _window, cx| {
                    editor.update(cx, |this, cx| this.connect(false, cx)).ok();
                }))
            })
    }

    /// The password this form would write to the keychain, for a test to read.
    #[cfg(test)]
    pub(crate) fn password_intent_for_test(&self, cx: &App) -> Option<String> {
        self.password(cx)
    }

    /// Pick `engine` and fill the database field in, the way clicking and
    /// typing would.
    #[cfg(test)]
    pub(crate) fn fill_for_test(
        &mut self,
        engine: Engine,
        database: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_engine(engine, window, cx);
        self.set_field(&self.database.clone(), database, window, cx);
    }

    /// Type `value` over the port box's contents.
    #[cfg(test)]
    pub(crate) fn set_port_for_test(&self, value: &str, window: &mut Window, cx: &mut App) {
        self.set_field(&self.port, value, window, cx);
    }

    /// The line below the form, as the user reads it; empty when there is none.
    #[cfg(test)]
    pub(crate) fn status_for_test(&self) -> String {
        match &self.status {
            Status::None => String::new(),
            Status::Testing => "testing".into(),
            Status::Succeeded => "succeeded".into(),
            Status::Failed(message) => format!("Error: {message}"),
        }
    }

    /// What the name, user, and database boxes show while empty.
    #[cfg(test)]
    pub(crate) fn placeholders_for_test(&self, cx: &App) -> [String; 3] {
        [&self.name, &self.username, &self.database]
            .map(|field| field.read(cx).presentation().placeholder().to_string())
    }

    /// Run the Test Connection button's check.
    #[cfg(test)]
    pub(crate) fn test_connection_for_test(&mut self, cx: &mut Context<Self>) {
        self.test_connection(cx);
    }

    /// Connect the way the split button does: saving first, or from its menu
    /// without.
    #[cfg(test)]
    pub(crate) fn connect_for_test(&mut self, save: bool, cx: &mut Context<Self>) {
        self.connect(save, cx);
    }

    /// Put the caret in the name box, where the dialog opens with it.
    #[cfg(test)]
    pub(crate) fn focus_name_for_test(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
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
                // The inputs' focus ring is drawn outside their border, which
                // this scroll area would clip at its edges; the padding gives
                // the ring room, and the negative margin keeps the fields
                // lined up with the title and the buttons. The margin sits on
                // a wrapper because a scrollable keeps its own margin on the
                // content, inside the edge that clips.
                div()
                    .id("editor-form")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .mx(px(-4.))
                    .child(
                        div()
                            .size_full()
                            .overflow_y_scrollbar()
                            .child(div().p(px(4.)).child(self.render_form(cx))),
                    ),
            )
            .children(self.render_status(cx))
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Button::new("editor-test")
                            .outline()
                            .label("Test Connection")
                            .loading(self.status == Status::Testing)
                            .tooltip("Check that these settings connect, without closing")
                            .on_click(cx.listener(|this, _, _window, cx| this.test_connection(cx))),
                    )
                    .child(div().flex_1())
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
                    .child(self.render_connect(cx)),
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
        .child(
            div()
                .id(SharedString::from(format!("field-{label}")))
                .test_support()
                .child(Input::new(input)),
        )
}
