//! Server user management: every user (Postgres: every role) in a grid,
//! with create, password change, and drop. Postgres and MySQL only — SQLite
//! has no users, so this tab is never offered for one.
//!
//! Reuses [`DataGrid`] the way [`crate::ui::process_list`] does, and the
//! grid's own row-picking for drop: pick the rows, then "Drop Selected".

use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Window, actions, div,
    px,
};

use crate::db::{Connection, Engine, runtime};
use crate::ui::busy::busy_label;
use crate::ui::data_grid::{DataGrid, Scope};

actions!(zippa_db, [CloseUserDialog]);

pub struct UserManagementView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    loading: bool,
    error: Option<String>,
    notice: Option<String>,
}

impl UserManagementView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| {
            let mut grid = DataGrid::new(connection.config.engine, window, cx);
            grid.set_placeholder("No users to show", cx);
            grid
        });
        let mut this = Self {
            connection,
            grid,
            loading: false,
            error: None,
            notice: None,
        };
        this.refresh(cx);
        this
    }

    /// Point this view at another connection — a database switch reopens the
    /// pool, so the server it asks moves with it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.notice = None;
        self.grid.update(cx, |grid, cx| grid.set_loading(true, cx));
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.users().await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.grid.update(cx, |grid, cx| grid.set_loading(false, cx));
                match result {
                    Ok(Ok(result)) => {
                        this.grid.update(cx, |grid, cx| grid.set_result(result, cx));
                    }
                    // Rows from before the failure must not stay pickable:
                    // "Drop Selected" would act on a list that is out of date.
                    Ok(Err(error)) => {
                        this.error = Some(format!("{error:#}"));
                        this.grid.update(cx, |grid, cx| grid.clear(cx));
                    }
                    Err(_) => this.error = Some("loading the user list was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The picked rows as `(name, host)` pairs — the host only on MySQL, where
    /// a user is `name@host`.
    fn picked_users(&self, cx: &App) -> Vec<(String, Option<String>)> {
        let engine = self.connection.config.engine;
        self.grid
            .read(cx)
            .snapshot(Scope::Picked, cx)
            .rows
            .iter()
            .filter_map(|row| {
                let name = row.first().cloned().flatten()?;
                let host = if engine == Engine::MySql {
                    Some(row.get(1).cloned().flatten()?)
                } else {
                    None
                };
                Some((name, host))
            })
            .collect()
    }

    fn open_create_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_user_dialog(UserDialogMode::Create, "Create user", window, cx);
    }

    fn open_password_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let users = self.picked_users(cx);
        let Some((name, host)) = users.first().cloned() else {
            self.notice = Some("Pick a user first.".to_string());
            cx.notify();
            return;
        };
        if users.len() > 1 {
            self.notice = Some("Pick exactly one user to set a password.".to_string());
            cx.notify();
            return;
        }
        self.open_user_dialog(
            UserDialogMode::SetPassword { name, host },
            "Set password",
            window,
            cx,
        );
    }

    fn open_user_dialog(
        &mut self,
        mode: UserDialogMode,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let view = cx.new(|cx| UserDialogView::new(self.connection.clone(), mode, window, cx));
        cx.subscribe_in(&view, window, Self::on_user_dialog_event)
            .detach();

        let body = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title)
                .w(px(400.))
                // The dialog's own keys are off; `escape` is bound to
                // `CloseUserDialog` on the body instead.
                .keyboard(false)
                .child(body.clone())
        });
        view.update(cx, |view, cx| view.focus(window, cx));
    }

    fn on_user_dialog_event(
        &mut self,
        _view: &Entity<UserDialogView>,
        event: &UserDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            UserDialogEvent::Dismissed => window.close_dialog(cx),
            // The list is re-read to show who is there now.
            UserDialogEvent::Changed => {
                window.close_dialog(cx);
                self.refresh(cx);
            }
        }
    }

    /// Ask before dropping the picked users, the way a destructive table
    /// action already does — there is no staging for this, so it asks every
    /// time rather than following the connection's safety mode.
    fn drop_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }

        let users = self.picked_users(cx);
        if users.is_empty() {
            return;
        }

        let count = users.len();
        let first = user_label(&users[0].0, users[0].1.as_deref());
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let users = users.clone();

            alert
                .title(if count == 1 {
                    format!("Drop user {first}?")
                } else {
                    format!("Drop {count} users?")
                })
                .description(
                    "The users lose access immediately; objects they own may be left behind. This cannot be undone.",
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Drop")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.run_drop(users.clone(), cx));
                    }
                    true
                })
        });
    }

    fn run_drop(&mut self, users: Vec<(String, Option<String>)>, cx: &mut Context<Self>) {
        // The confirm dialog answers later; a drop started in the meantime
        // wins. `loading` doubles as the busy flag: it disables the toolbar
        // buttons, and the trailing `refresh` clears it when the list lands.
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();

        let connection = self.connection.clone();
        let count = users.len();
        let task = runtime::spawn(async move {
            let mut failed = 0;
            let mut first_error = None;
            for (name, host) in users {
                if let Err(error) = connection.drop_user(&name, host.as_deref()).await {
                    failed += 1;
                    first_error.get_or_insert_with(|| {
                        format!("{}: {error:#}", user_label(&name, host.as_deref()))
                    });
                }
            }
            (failed, first_error)
        });

        cx.spawn(async move |this, cx| {
            let (failed, first_error) = task
                .await
                .unwrap_or((count, Some("the drop was cancelled".to_string())));
            this.update(cx, |this, cx| {
                // The list is re-read to show who is left, and the outcome
                // set after it: a refresh starts by clearing the notice and
                // the error.
                this.refresh(cx);
                this.notice = Some(if failed == 0 {
                    format!("Dropped {count} user{}", if count == 1 { "" } else { "s" })
                } else {
                    format!("Dropped {}, {failed} failed", count - failed)
                });
                // Why the first one failed, so a refusal is not a bare count.
                this.error = first_error;
            })
            .ok();
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let read_only = self.connection.config.safety.is_read_only();

        h_flex()
            .id("user-management-toolbar")
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading {
                        busy_label("Loading…", cx.theme().muted_foreground).into_any_element()
                    } else {
                        self.notice.clone().unwrap_or_default().into_any_element()
                    }),
            )
            .when(!read_only, |this| {
                this.child(
                    Button::new("new-user")
                        .primary()
                        .xsmall()
                        .disabled(self.loading)
                        .label("New User")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_create_dialog(window, cx)),
                        ),
                )
                .child(
                    Button::new("set-user-password")
                        .ghost()
                        .xsmall()
                        .disabled(self.loading)
                        .label("Set Password")
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.open_password_dialog(window, cx)
                            }),
                        ),
                )
                .child(
                    Button::new("drop-selected-users")
                        .ghost()
                        .xsmall()
                        .disabled(self.loading)
                        .label("Drop Selected")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.drop_selected(window, cx)),
                        ),
                )
            })
            .child(
                Button::new(SharedString::from("refresh-user-list"))
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
    }
}

impl Focusable for UserManagementView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for UserManagementView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("user-management-view")
            .size_full()
            .child(self.render_toolbar(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}

/// What the user dialog tells its owner.
pub enum UserDialogEvent {
    /// The user is done with the dialog; take it off screen.
    Dismissed,
    /// A user was created or a password changed; the list is stale.
    Changed,
}

enum UserDialogMode {
    Create,
    SetPassword { name: String, host: Option<String> },
}

pub struct UserDialogView {
    connection: Arc<Connection>,
    mode: UserDialogMode,
    name: Entity<InputState>,
    host: Entity<InputState>,
    password: Entity<InputState>,
    error: Option<String>,
    working: bool,
    /// The dialog body's own focus, so `escape` reaches `CloseUserDialog`.
    focus: FocusHandle,
}

impl EventEmitter<UserDialogEvent> for UserDialogView {}

impl UserDialogView {
    fn new(
        connection: Arc<Connection>,
        mode: UserDialogMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let host = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value("%".to_string(), window, cx);
            state
        });
        // Masked like the connection editor's password box.
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        cx.subscribe(&password, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit(cx);
            }
        })
        .detach();

        Self {
            connection,
            mode,
            name,
            host,
            password,
            error: None,
            working: false,
            focus: cx.focus_handle(),
        }
    }

    /// Put the keyboard on the first box, so typing starts at once.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        use gpui_kit::Focusable as _;
        match self.mode {
            UserDialogMode::Create => self.name.read(cx).focus_handle(cx).focus(window, cx),
            UserDialogMode::SetPassword { .. } => {
                self.password.read(cx).focus_handle(cx).focus(window, cx)
            }
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(UserDialogEvent::Dismissed);
    }

    /// `Escape` closes the dialog.
    fn on_close(&mut self, _: &CloseUserDialog, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        let engine = self.connection.config.engine;
        // Taken as typed: a space at either end is part of the password,
        // and trimming it would set one the user never chose.
        let password = self.password.read(cx).value().to_string();
        if password.is_empty() {
            self.error = Some("Enter a password.".to_string());
            cx.notify();
            return;
        }

        let (name, host) = match &self.mode {
            UserDialogMode::Create => {
                let name = self.name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    self.error = Some("Enter a name for the new user.".to_string());
                    cx.notify();
                    return;
                }
                let host = (engine == Engine::MySql)
                    .then(|| self.host.read(cx).value().trim().to_string())
                    .filter(|host| !host.is_empty());
                if engine == Engine::MySql && host.is_none() {
                    self.error = Some("Enter the host the user may connect from.".to_string());
                    cx.notify();
                    return;
                }
                (name, host)
            }
            UserDialogMode::SetPassword { name, host } => (name.clone(), host.clone()),
        };

        self.error = None;
        self.working = true;
        cx.notify();

        let connection = self.connection.clone();
        let is_create = matches!(self.mode, UserDialogMode::Create);
        let task = runtime::spawn(async move {
            if is_create {
                connection
                    .create_user(&name, host.as_deref(), &password)
                    .await
            } else {
                connection
                    .set_user_password(&name, host.as_deref(), &password)
                    .await
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.working = false;
                match result {
                    Ok(Ok(_)) => cx.emit(UserDialogEvent::Changed),
                    Ok(Err(error)) => {
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                    }
                    Err(_) => {
                        this.error = Some("The request was cancelled.".to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_labeled_input(
        &self,
        label: &'static str,
        id: &'static str,
        input: &Entity<InputState>,
    ) -> impl IntoElement {
        v_flex().gap_1().child(div().text_sm().child(label)).child(
            Input::new(input)
                .id(id)
                .aria_label(label)
                .small()
                .disabled(self.working),
        )
    }
}

impl Render for UserDialogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = self.connection.config.engine;
        let is_create = matches!(self.mode, UserDialogMode::Create);
        let title = match &self.mode {
            UserDialogMode::Create => "Create user".to_string(),
            UserDialogMode::SetPassword { name, host } => {
                format!("Set password for {}", user_label(name, host.as_deref()))
            }
        };

        v_flex()
            .id("user-dialog")
            .test_support()
            .size_full()
            .gap_3()
            .track_focus(&self.focus)
            .key_context("UserDialog")
            .on_action(cx.listener(Self::on_close))
            .child(div().text_sm().child(title))
            .when(is_create, |this| {
                this.child(self.render_labeled_input("Name", "user-dialog-name", &self.name))
            })
            .when(is_create && engine == Engine::MySql, |this| {
                this.child(self.render_labeled_input("Host", "user-dialog-host", &self.host))
            })
            .child(self.render_labeled_input("Password", "user-dialog-password", &self.password))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("user-dialog-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("user-dialog-submit")
                            .primary()
                            .small()
                            .label(if is_create {
                                "Create user"
                            } else {
                                "Set password"
                            })
                            .disabled(self.working)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}

/// A user as the server names one: `'name'@'host'` on MySQL, `'name'` on
/// Postgres.
fn user_label(name: &str, host: Option<&str>) -> String {
    match host {
        Some(host) => format!("'{name}'@'{host}'"),
        None => format!("'{name}'"),
    }
}

#[cfg(test)]
impl UserDialogView {
    /// The new-user dialog, as "New User" opens it.
    pub(crate) fn create_for_test(
        connection: Arc<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(connection, UserDialogMode::Create, window, cx)
    }
}
