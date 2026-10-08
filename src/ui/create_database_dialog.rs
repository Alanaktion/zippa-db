//! The Create Database dialog.
//!
//! Opened from the database picker menu. A name box and Create/Cancel; the
//! new database is created with `CREATE DATABASE` and the picker's list is
//! reloaded on success.
//!
//! The dialog is `gpui_kit`'s [`Dialog`](gpui_kit::component::dialog::Dialog),
//! opened by the session, and this view is its body. It emits
//! [`CreateDatabaseEvent::Dismissed`] when the user is done with it and
//! [`CreateDatabaseEvent::Created`] when the database was created, so the
//! session can reload the database list.

use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Window, actions, div};

use crate::db::sql;
use crate::db::{Connection, runtime};

actions!(zippa_db, [CloseCreateDatabase]);

/// What the dialog tells its owner.
pub enum CreateDatabaseEvent {
    /// The user is done with the dialog; the session takes it off screen.
    Dismissed,
    /// The database was created; the picker's list is stale.
    Created,
}

pub struct CreateDatabaseView {
    connection: Arc<Connection>,
    name: Entity<InputState>,
    error: Option<String>,
    creating: bool,
    /// The dialog body's own focus, so `escape` reaches `CloseCreateDatabase`.
    focus: gpui_kit::FocusHandle,
}

impl EventEmitter<CreateDatabaseEvent> for CreateDatabaseView {}

impl CreateDatabaseView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(&name, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.create(cx);
            }
        })
        .detach();

        Self {
            connection,
            name,
            error: None,
            creating: false,
            focus: cx.focus_handle(),
        }
    }

    /// Put the keyboard on the name box, so typing starts at once.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        use gpui_kit::Focusable as _;
        self.name.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(CreateDatabaseEvent::Dismissed);
    }

    /// `Escape` closes the dialog.
    fn on_close(&mut self, _: &CloseCreateDatabase, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let engine = self.connection.config.engine;
        let Some(statement) = sql::create_database_sql(&name, engine) else {
            self.error = Some(if name.is_empty() {
                "Enter a name for the new database.".to_string()
            } else {
                format!("{} does not support creating databases.", engine.label())
            });
            cx.notify();
            return;
        };

        self.error = None;
        self.creating = true;
        cx.notify();

        // `CREATE DATABASE` cannot run inside a transaction block on
        // Postgres; `execute` runs it bare, so there is nothing to avoid.
        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.execute(&statement, vec![]).await });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(Ok(_)) => {
                        cx.emit(CreateDatabaseEvent::Created);
                        cx.emit(CreateDatabaseEvent::Dismissed);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                    }
                    Err(_) => {
                        this.error = Some("Creating the database was cancelled.".to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for CreateDatabaseView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("create-database-dialog")
            .test_support()
            .size_full()
            .gap_3()
            .track_focus(&self.focus)
            .key_context("CreateDatabaseDialog")
            .on_action(cx.listener(Self::on_close))
            .child(div().text_sm().child("Database name"))
            .child(
                Input::new(&self.name)
                    .id("create-database-name")
                    .aria_label("Database name")
                    .small()
                    .disabled(self.creating),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("create-database-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("create-database-create")
                            .primary()
                            .small()
                            .label("Create database")
                            .disabled(self.creating)
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    ),
            )
    }
}

#[cfg(test)]
impl CreateDatabaseView {
    pub(crate) fn set_name_for_test(&self, name: &str, window: &mut Window, cx: &mut App) {
        let name = name.to_string();
        let input = self.name.clone();
        input.update(cx, |state, cx| state.set_value(name, window, cx));
    }

    pub(crate) fn submit_for_test(&mut self, cx: &mut Context<Self>) {
        self.create(cx);
    }

    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }
}
