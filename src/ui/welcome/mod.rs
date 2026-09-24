//! The welcome screen: a launcher for saved connections.
//!
//! The form that used to sit beside the list is now a dialog
//! ([`ConnectionEditor`]), opened from here. The screen itself is launcher-first:
//! saved connections are cards the user clicks to connect, searchable and
//! ordered most-recently-connected first, with an editor behind "New
//! connection" (and each card's `…` menu).

use std::cmp::Ordering;
use std::sync::Arc;

use chrono::Utc;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
// `App` is only named by the test-only reach-in below.
#[cfg(test)]
use gpui_kit::App;
use gpui_kit::{Context, Entity, EventEmitter, FocusHandle, Window, div, px};
use uuid::Uuid;

use crate::app::NewConnection;
use crate::db::{Connection, ConnectionConfig, Engine, runtime, store};
use crate::ui::{notify_error, sql_file};

mod card;
mod editor;

use card::render_card;
use editor::{ConnectionEditor, EditorEvent};
pub(crate) use editor::{EditorClose, EditorConnect};

/// Width of the editor dialog: enough for the form, not a full window.
const EDITOR_WIDTH: f32 = 560.;

pub enum WelcomeEvent {
    /// A connection was opened and the session can start.
    Connected(Arc<Connection>),
}

pub struct Welcome {
    connections: Vec<ConnectionConfig>,
    /// Search text over the saved list.
    query: Entity<InputState>,
    /// The connection being connected to, if any.
    connecting: Option<Uuid>,
    /// An error to show in the banner at the top of the screen.
    error: Option<String>,
    /// The editor dialog open over the screen, if any.
    editor: Option<Entity<ConnectionEditor>>,
    /// Held by the launcher itself, so its own key context answers even when
    /// the screen has no search box to focus.
    focus: FocusHandle,
}

impl EventEmitter<WelcomeEvent> for Welcome {}

impl Welcome {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (connections, error) = match store::load() {
            Ok(connections) => (connections, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };

        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search connections"));
        cx.subscribe_in(&query, window, Self::on_search_event)
            .detach();

        Self {
            connections,
            query,
            connecting: None,
            error,
            editor: None,
            focus: cx.focus_handle(),
        }
    }

    fn on_search_event(
        &mut self,
        _query: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    }

    /// Put the caret in the search box when there is something to search, or
    /// on the launcher itself so its shortcuts keep working on an empty screen.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connections.is_empty() {
            self.query.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
    }

    fn is_connecting(&self, id: &Uuid) -> bool {
        self.connecting == Some(*id)
    }

    /// Open the editor for a new connection, or pre-filled from `config`.
    fn open_editor(
        &mut self,
        config: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if config.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };

        let editor = cx.new(|cx| ConnectionEditor::new(config, window, cx));
        cx.subscribe_in(&editor, window, Self::on_editor_event)
            .detach();
        self.editor = Some(editor.clone());

        let welcome = cx.entity().downgrade();
        let body = editor.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let welcome = welcome.clone();
            dialog
                .w(px(EDITOR_WIDTH))
                .h(px(600.))
                .title(title)
                .keyboard(false)
                .on_close(move |_, _window, cx| {
                    welcome
                        .update(cx, |this, cx| {
                            this.editor = None;
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    /// Take the editor away and hand focus back to the launcher.
    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor = None;
        window.close_dialog(cx);
        cx.notify();
    }

    fn on_editor_event(
        &mut self,
        _editor: &Entity<ConnectionEditor>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorEvent::Saved { config, password } => {
                self.save(config.clone(), password.clone(), window, cx);
                self.close_editor(window, cx);
            }
            EditorEvent::Connect { config, password } => {
                self.close_editor(window, cx);
                // An untouched box reports no password; a saved connection then
                // connects with whatever the keychain holds, the way a card
                // click does. A file database has no password to look up, and a
                // connection that was never saved has nothing stored.
                let saved = self.connections.iter().any(|saved| saved.id == config.id);
                match password {
                    Some(password) => {
                        self.connect(config.clone(), Some(password.clone()), window, cx)
                    }
                    None if saved && !config.engine.is_file_based() => {
                        self.connect_using_stored_password(config.clone(), cx)
                    }
                    None => self.connect(config.clone(), None, window, cx),
                }
            }
            EditorEvent::Dismissed => self.close_editor(window, cx),
        }
    }

    fn on_new_connection(
        &mut self,
        _: &NewConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_editor(None, window, cx);
    }

    /// Save the editor's config into the list and the store.
    ///
    /// `password` is `None` when the user never touched the password box, which
    /// leaves whatever the keychain already holds alone; an empty string means
    /// they emptied the box on purpose, which forgets the password.
    fn save(
        &mut self,
        config: ConnectionConfig,
        password: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self
            .connections
            .iter()
            .position(|saved| saved.id == config.id)
        {
            Some(index) => {
                // Editing a connection is not connecting to it, so its "last
                // connected" stamp survives the edit: the card keeps its place
                // in most-recent-first order and its "Connected … ago" line.
                let mut updated = config.clone();
                updated.last_connected = self.connections[index].last_connected;
                self.connections[index] = updated;
            }
            None => self.connections.push(config.clone()),
        }

        // The list is small, but writing it is still disk I/O; the keychain is a
        // second, slower one behind it.
        let connections = self.connections.clone();
        let id = config.id;
        store_in_background(
            move || {
                store::save(&connections)?;
                if let Some(password) = password {
                    store::set_password(&id, &password)?;
                }
                Ok(())
            },
            cx,
        );
        cx.notify();
    }

    /// Connect to a saved connection the way a card click does, asking first
    /// when it is the footgun a tag exists to flag: production, set to
    /// auto-apply. The editor already warns about this combination while it
    /// is being set up, but that warning is easy to click past and easy to
    /// forget by the time the card is clicked days later — this is the last
    /// chance to catch it before a write goes out unasked. Every other
    /// connection connects straight away, as it always has.
    pub(crate) fn connect_saved_with_confirmation(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !config.is_risky_auto_apply() {
            self.connect_saved(config, window, cx);
            return;
        }

        let name = config.display_name();
        let welcome = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let welcome = welcome.clone();
            let config = config.clone();
            alert
                .title(format!("Connect to {name}?"))
                .description(
                    "This connection is tagged production and set to auto-apply: edits are \
                     written the moment you leave a row, with no confirmation.",
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Connect")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(welcome) = welcome.upgrade() {
                        welcome.update(cx, |this, cx| {
                            this.connect_saved(config.clone(), window, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Connect to a saved connection, looking its password up on demand.
    fn connect_saved(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A file database has no password to look up, and asking the keychain
        // for one it never stored can prompt the user for nothing.
        if config.engine.is_file_based() {
            self.connect(config, None, window, cx);
            return;
        }

        self.connect_using_stored_password(config, cx);
    }

    /// Look the connection's password up off the UI thread and connect with it.
    ///
    /// The credential store can prompt, or simply be slow, so it is not read on
    /// the UI thread; the connect starts when the password comes back.
    fn connect_using_stored_password(&mut self, config: ConnectionConfig, cx: &mut Context<Self>) {
        let id = config.id;
        let password = cx.background_spawn(async move { store::password(&id) });
        cx.spawn(async move |this, cx| {
            let password = password.await;
            this.update_in(cx, |this, window, cx| match password {
                Ok(password) => this.connect(config, password, window, cx),
                Err(error) => {
                    this.error = Some(format!("{error:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Open a saved connection by id, connecting to it right away.
    ///
    /// This is the path a card click takes, so a restored connection starts up
    /// exactly the way a manual one does: its password comes from the keychain
    /// and the failure is shown on the launcher.
    pub(crate) fn open(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
        else {
            return;
        };
        self.connect_saved(config, window, cx);
    }

    /// Open `config` with `password`, and hand the live connection on.
    fn connect(
        &mut self,
        config: ConnectionConfig,
        password: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connecting = Some(config.id);
        self.error = None;
        cx.notify();

        let task = runtime::spawn(async move { Connection::open(config.clone(), password).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.connecting = None;
                match result {
                    Ok(Ok(connection)) => {
                        this.error = None;
                        this.mark_connected(&connection.config.id, cx);
                        cx.emit(WelcomeEvent::Connected(Arc::new(connection)));
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        this.error = Some(message.clone());
                        notify_error(window, cx, format!("Error: {message}"));
                    }
                    Err(_) => {
                        this.error = Some("the connection was cancelled".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Record that a connection was just opened, for most-recent-first order.
    fn mark_connected(&mut self, id: &Uuid, cx: &mut Context<Self>) {
        if let Some(config) = self.connections.iter_mut().find(|config| &config.id == id) {
            config.last_connected = Some(Utc::now());
        }

        // Writes the launcher's own in-memory list, the same path every other
        // mutation goes through (`save`, `delete_confirmed`), rather than a
        // separate load-mutate-save against the file: a second read of the
        // file here could race a concurrent save from the editor — e.g. a
        // newly added connection whose own write has not landed yet — and
        // overwrite it with a copy that never had the edit. `store_in_background`
        // also surfaces a failed write as a toast rather than only `eprintln!`.
        let connections = self.connections.clone();
        store_in_background(move || store::save(&connections), cx);
    }

    /// Open the editor pre-filled from the saved connection.
    fn edit(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
        else {
            return;
        };
        self.open_editor(Some(config), window, cx);
    }

    /// Copy a saved connection under a new id, without its stored password.
    fn duplicate(&mut self, id: Uuid, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
        else {
            return;
        };

        let mut copy = source.clone();
        copy.id = Uuid::new_v4();
        copy.last_connected = None;
        if !source.name.trim().is_empty() {
            copy.name = format!("{} copy", source.name.trim());
        }
        self.connections.push(copy);

        let connections = self.connections.clone();
        store_in_background(move || store::save(&connections), cx);
        cx.notify();
    }

    /// Ask before throwing a saved connection away.
    fn delete(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self
            .connections
            .iter()
            .find(|config| config.id == id)
            .cloned()
        else {
            return;
        };
        let name = config.display_name();
        let welcome = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let welcome = welcome.clone();
            alert
                .title(format!("Delete \"{name}\"?"))
                .description("This removes the saved connection and its stored password.")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    if let Some(welcome) = welcome.upgrade() {
                        welcome.update(cx, |this, cx| this.delete_confirmed(id, window, cx));
                    }
                    true
                })
        });
    }

    fn delete_confirmed(&mut self, id: Uuid, _window: &mut Window, cx: &mut Context<Self>) {
        self.connections.retain(|config| config.id != id);

        let connections = self.connections.clone();
        store_in_background(
            move || {
                store::save(&connections)?;
                store::delete_password(&id)
            },
            cx,
        );
        cx.notify();
    }

    /// Open the platform file picker and start a SQLite connection from it.
    fn open_sqlite_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_open(cx);
        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Some(paths)) => paths,
                _ => return,
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            this.update_in(cx, |this, window, cx| {
                let mut config = ConnectionConfig::new(Engine::Sqlite);
                config.database = path.to_string_lossy().into_owned();
                this.open_editor(Some(config), window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .max_w(px(640.))
            .mx_auto()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .px_6()
            .pt_6()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_xl().child("Zippa DB"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Pick a connection, or set up a new one."),
                    ),
            )
            .child(
                Button::new("welcome-new-connection")
                    .primary()
                    .label("New connection")
                    .tooltip_with_action("New connection", &NewConnection, Some("Welcome"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_editor(None, window, cx)),
                    ),
            )
    }

    fn render_error(&self, error: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("error-banner")
            .w_full()
            .flex_none()
            .px_6()
            .pt_3()
            .child(
                div()
                    .w_full()
                    .max_h(px(96.))
                    .overflow_y_scrollbar()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(format!("Error: {error}")),
            )
    }

    /// The search box and the cards it filters, or the empty state.
    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.connections.is_empty() {
            return self.render_empty(cx).into_any_element();
        }

        let query = self.query.read(cx).value().trim().to_string();
        let visible: Vec<&ConnectionConfig> = ordered(&self.connections)
            .into_iter()
            .filter(|config| matches_query(config, &query))
            .collect();

        v_flex()
            .flex_1()
            .min_h_0()
            .pt_3()
            .pb_6()
            .child(
                // A single reading column, centered and capped so the cards
                // stay a comfortable width on a wide window.
                v_flex()
                    .w_full()
                    .max_w(px(640.))
                    .mx_auto()
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .gap_3()
                    .child(Input::new(&self.query).id("connection-search"))
                    .when(visible.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No connections match"),
                        )
                    })
                    .child(
                        div()
                            .id("connections")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(
                                v_flex().gap_2().children(
                                    visible
                                        .into_iter()
                                        .map(|config| render_card(self, config, cx)),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The state shown before any connection is saved.
    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(div().text_lg().child("No connections yet"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Add a connection to start querying your databases."),
            )
            .child(
                Button::new("empty-new-connection")
                    .primary()
                    .label("New connection")
                    .tooltip_with_action("New connection", &NewConnection, Some("Welcome"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_editor(None, window, cx)),
                    ),
            )
            .child(
                Button::new("open-sqlite-file")
                    .outline()
                    .label("Open SQLite file…")
                    .on_click(cx.listener(|this, _, window, cx| this.open_sqlite_file(window, cx))),
            )
    }

    /// Put the saved list in place without touching the on-disk store.
    #[cfg(test)]
    pub(crate) fn set_connections_for_test(
        &mut self,
        connections: Vec<ConnectionConfig>,
        cx: &mut Context<Self>,
    ) {
        self.connections = connections;
        cx.notify();
    }

    /// Show an error banner without a live connection attempt.
    #[cfg(test)]
    pub(crate) fn show_error_for_test(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.error = Some(message.into());
        cx.notify();
    }

    /// Whether the editor dialog is open over the launcher.
    #[cfg(test)]
    pub(crate) fn editor_open_for_test(&self) -> bool {
        self.editor.is_some()
    }

    /// The saved connections as the launcher holds them.
    #[cfg(test)]
    pub(crate) fn connections_for_test(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    /// Save a connection the way the editor's Save button does. `password` is
    /// `None` for a box the user never touched.
    #[cfg(test)]
    pub(crate) fn save_for_test(
        &mut self,
        config: ConnectionConfig,
        password: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save(config, password.map(str::to_string), window, cx);
    }

    /// Open the editor pre-filled from a saved connection, as its card does.
    #[cfg(test)]
    pub(crate) fn edit_for_test(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(id, window, cx);
    }

    /// The password the open editor would write to the keychain, for a test to
    /// read. `None` when the box has not been typed in.
    #[cfg(test)]
    pub(crate) fn editor_password_intent_for_test(&self, cx: &App) -> Option<String> {
        self.editor
            .as_ref()
            .and_then(|editor| editor.read(cx).password_intent_for_test(cx))
    }

    /// Put the caret in the open editor's password box, the way a click would.
    #[cfg(test)]
    pub(crate) fn focus_editor_password_for_test(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.focus_password_for_test(window, cx));
    }

    /// Duplicate a connection the way the card's `…` menu does.
    #[cfg(test)]
    pub(crate) fn duplicate_for_test(
        &mut self,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(id, window, cx);
    }

    /// Remove a connection outright, as the confirmation dialog's Delete does.
    #[cfg(test)]
    pub(crate) fn delete_confirmed_for_test(
        &mut self,
        id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_confirmed(id, window, cx);
    }
}

/// Run a store write on the background executor, reporting a failure on the
/// launcher the way a synchronous write used to.
///
/// The connection list is small, but writing it is still disk I/O and the UI
/// thread should not wait on it; the keychain behind it is slower still.
fn store_in_background<F>(work: F, cx: &mut Context<Welcome>)
where
    F: FnOnce() -> anyhow::Result<()> + Send + 'static,
{
    let result = cx.background_spawn(async move { work() });
    cx.spawn(async move |this, cx| {
        if let Err(error) = result.await {
            let message = format!("{error:#}");
            this.update_in(cx, |this, window, cx| {
                notify_error(window, cx, format!("Error: {message}"));
                this.error = Some(message);
                cx.notify();
            })
            .ok();
        }
    })
    .detach();
}

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Welcome")
            .on_action(cx.listener(Self::on_new_connection))
            .child(self.render_header(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(self.render_error(&error, cx))
            })
            .child(self.render_body(cx))
    }
}

/// Saved connections, most recently connected first, then by name.
fn ordered(connections: &[ConnectionConfig]) -> Vec<&ConnectionConfig> {
    let mut ordered: Vec<&ConnectionConfig> = connections.iter().collect();
    ordered.sort_by(|a, b| match (a.last_connected, b.last_connected) {
        (Some(x), Some(y)) => y
            .cmp(&x)
            .then_with(|| a.display_name().cmp(&b.display_name())),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.display_name().cmp(&b.display_name()),
    });
    ordered
}

/// Whether a connection matches the search text, across name, host, database,
/// tag and engine.
fn matches_query(config: &ConnectionConfig, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    let tag = config.tag.as_deref().unwrap_or("");
    config.name.to_lowercase().contains(&query)
        || config.host.to_lowercase().contains(&query)
        || config.database.to_lowercase().contains(&query)
        || tag.to_lowercase().contains(&query)
        || config.engine.label().to_lowercase().contains(&query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str, last_connected: Option<&str>) -> ConnectionConfig {
        ConnectionConfig {
            name: name.to_string(),
            host: "db.internal".into(),
            database: "app".into(),
            tag: Some("Production".into()),
            last_connected: last_connected
                .map(|when| when.parse::<chrono::DateTime<Utc>>().expect("a timestamp")),
            ..ConnectionConfig::new(Engine::Postgres)
        }
    }

    #[test]
    fn ordering_puts_recent_first_then_name() {
        let a = config("alpha", Some("2024-01-01T00:00:00Z"));
        let b = config("beta", Some("2024-01-03T00:00:00Z"));
        let c = config("gamma", None);

        let names: Vec<String> = ordered(&[a, c, b])
            .into_iter()
            .map(|config| config.name.clone())
            .collect();
        assert_eq!(names, ["beta", "alpha", "gamma"]);
    }

    #[test]
    fn search_matches_name_host_database_tag_and_engine() {
        let config = config("Prod DB", None);

        assert!(matches_query(&config, "prod"));
        assert!(matches_query(&config, "internal"));
        assert!(matches_query(&config, "app"));
        assert!(matches_query(&config, "Production"));
        assert!(matches_query(&config, "postgres"));
        assert!(!matches_query(&config, "mysql"));
    }

    #[test]
    fn ordering_uses_display_name_when_untitled() {
        let mut a = config("", None);
        a.database = "zebra.sqlite".into();
        a.engine = Engine::Sqlite;
        let mut b = config("", None);
        b.database = "alpha.sqlite".into();
        b.engine = Engine::Sqlite;

        let names: Vec<String> = ordered(&[a, b])
            .into_iter()
            .map(|config| config.display_name())
            .collect();
        assert_eq!(names, ["alpha.sqlite", "zebra.sqlite"]);
    }
}
