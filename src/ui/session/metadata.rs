//! What the session knows about the server: the database list, the sidebar's
//! objects, the schema-search catalog, and switching database.

use std::sync::Arc;

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::Button;
use gpui_kit::component::button::{ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Window, px};

use crate::db::{Catalog, CatalogEntry, Connection, runtime};
use crate::ui::create_database_dialog::{CreateDatabaseEvent, CreateDatabaseView};

use super::{Session, SessionEvent, Status};

impl Session {
    /// Read the database list and the current database's tables and views.
    pub(crate) fn reload_metadata(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let started_on = connection.clone();
        let task = runtime::spawn(async move {
            (
                connection.databases().await,
                connection.objects().await,
                connection.stored_objects().await,
                connection.catalog().await,
            )
        });

        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update_in(cx, |this, window, cx| {
                // `switch_database` replaces `self.connection` and closes the
                // one this task started on, then kicks off its own reload. A
                // read still in flight against the old connection at that
                // point loses its pool mid-await and comes back an error;
                // that error is stale (the fresh reload already has, or will
                // have, the right answer), so it is dropped rather than
                // shown.
                if !Arc::ptr_eq(&this.connection, &started_on) {
                    return;
                }
                this.metadata_error = None;
                match loaded {
                    Ok((databases, objects, stored, catalog)) => {
                        match databases {
                            Ok(databases) => this.databases = databases,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        match objects {
                            Ok(objects) => this.objects = objects,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        match stored {
                            Ok(stored) => this.stored = stored,
                            Err(error) => this.metadata_error = Some(format!("{error:#}")),
                        }
                        this.store_catalog(catalog);
                        this.catalog
                            .set_source(this.connection.clone(), this.databases.clone());
                    }
                    Err(_) => {
                        this.metadata_error = Some("reading the schema was cancelled".into());
                        this.catalog_loading = false;
                    }
                }
                if let Some(error) = &this.metadata_error {
                    crate::ui::notify_error(window, cx, format!("Error: {error}"));
                }
                this.rebuild_tree(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keep the catalog read's answer, falling back to the names the sidebar
    /// already has when the full read failed — a database whose column query
    /// times out should still be searchable by name.
    fn store_catalog(&mut self, catalog: Result<Catalog, anyhow::Error>) {
        self.catalog_loading = false;
        match catalog {
            Ok(catalog) => {
                self.catalog.set(Arc::new(catalog));
                self.catalog_error = None;
            }
            Err(error) => {
                let mut fallback = Catalog::default();
                fallback.entries = self
                    .objects
                    .iter()
                    .cloned()
                    .map(CatalogEntry::object)
                    .chain(self.stored.iter().cloned().map(CatalogEntry::routine))
                    .collect();
                fallback.total = fallback.entries.len();
                self.catalog.set(Arc::new(fallback));
                self.catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    /// The whole schema, for [`SearchSchema`].
    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        self.catalog.get()
    }

    pub(crate) fn catalog_loading(&self) -> bool {
        self.catalog_loading
    }

    pub(crate) fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    /// Switch to `database` for the user, asking first when a tab has a
    /// transaction open: each query tab's connection is to the database being
    /// left, so switching closes it and rolls the transaction back.
    pub(crate) fn request_switch_database(
        &mut self,
        database: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.open_transactions(cx);
        let unapplied = self.unapplied_changes(cx);
        if (open == 0 && unapplied == 0) || database == self.connection.database() {
            self.switch_database(database, cx);
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }

        let mut description = match open {
            0 => String::new(),
            1 => "A tab has an open transaction. Switching database closes its connection, \
                  which rolls the transaction back."
                .to_string(),
            count => format!(
                "{count} tabs have open transactions. Switching database closes their \
                 connections, which rolls the transactions back."
            ),
        };
        if unapplied > 0 {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(&unapplied_sentence(unapplied, "Switching database"));
        }
        let (title, ok) = if open > 0 {
            (
                format!("Roll back and switch to {database}?"),
                "Roll Back and Switch",
            )
        } else {
            (
                format!("Discard changes and switch to {database}?"),
                "Discard and Switch",
            )
        };
        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let database = database.clone();
            alert
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok)
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.switch_database(database.clone(), cx)
                        });
                    }
                    true
                })
        });
    }

    /// Reopen the pool against `database` and reload the object list.
    ///
    /// No engine can move an open pool to another database, so this replaces
    /// the connection and drains the old one in the background.
    pub(crate) fn switch_database(&mut self, database: String, cx: &mut Context<Self>) {
        // A file-based engine has a single database; "switching" would try to
        // open a file named after it.
        if self.connection.config.engine.is_file_based() {
            return;
        }
        if database == self.connection.database() {
            return;
        }
        self.reopen(database, false, cx);
    }

    /// Throw the pool away and open a fresh one to the same database, for a
    /// connection the server dropped or that stopped answering. The tabs, the
    /// console history, and anything typed stay as they are.
    /// Reconnect for the user, asking first when a tab holds staged rows or
    /// structure edits: the tabs are re-read from the new connection, which
    /// drops them. An open transaction is not asked about — a reconnect is
    /// for a connection the server has already dropped, taking it along.
    pub(crate) fn request_reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let unapplied = self.unapplied_changes(cx);
        if unapplied == 0 {
            self.reconnect(cx);
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let description = unapplied_sentence(unapplied, "Reconnecting");
        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            alert
                .title("Discard changes and reconnect?")
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Discard and Reconnect")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| session.reconnect(cx));
                    }
                    true
                })
        });
    }

    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        let database = self.connection.database().to_string();
        self.reopen(database, true, cx);
    }

    /// Open a new pool to `database` and adopt it in place of the current
    /// one: a database switch, or (`reconnect`) a reconnect to the same one.
    fn reopen(&mut self, database: String, reconnect: bool, cx: &mut Context<Self>) {
        if self.switching {
            return;
        }

        self.switching = true;
        self.reconnecting = reconnect;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.with_database(&database).await });

        cx.spawn(async move |this, cx| {
            let opened = task.await;
            this.update_in(cx, |this, window, cx| {
                this.switching = false;
                this.reconnecting = false;
                match opened {
                    Ok(Ok(connection)) => {
                        this.adopt_connection(connection, cx);
                        cx.emit(SessionEvent::Changed);
                        if reconnect {
                            crate::ui::notify_info(window, cx, "Reconnected.");
                        }
                    }
                    Ok(Err(error)) => {
                        let message = format!("{error:#}");
                        if let Some(panel) = this.active_panel() {
                            panel.update(cx, |panel, _| {
                                panel.set_status(Status::Error(message.clone()))
                            });
                        }
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                    Err(_) => {
                        let message = if reconnect {
                            "reconnecting was cancelled"
                        } else {
                            "switching database was cancelled"
                        };
                        if let Some(panel) = this.active_panel() {
                            panel.update(cx, |panel, _| {
                                panel.set_status(Status::Error(message.into()))
                            });
                        }
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Swap `self.connection` for a freshly opened one on another database:
    /// close what it replaces, carry its console history forward, reset the
    /// schema read, and point every open panel at the new connection.
    ///
    /// Shared by [`Self::switch_database`] and a restore that switches before
    /// building any tabs, so both keep the panels, the catalog, and the
    /// console in step the same way.
    pub(super) fn adopt_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.connection, Arc::new(connection));

        // The console's history belongs to the tab, not the pool about to
        // close: an internal read that failed right before the switch (or
        // any read from before it) should still be there to look at
        // afterward rather than vanish with the connection that logged it.
        for entry in previous.query_log().snapshot() {
            self.connection.query_log().record(entry);
        }
        runtime::spawn(async move { previous.close().await });

        self.objects.clear();
        self.stored.clear();
        self.catalog.set(Arc::new(Catalog::default()));
        self.catalog
            .set_source(self.connection.clone(), self.databases.clone());
        self.catalog_loading = true;
        self.catalog_error = None;
        self.rebuild_tree(cx);
        let connection = self.connection.clone();
        for panel in self.panels.clone() {
            let connection = connection.clone();
            panel.update(cx, |panel, cx| panel.set_connection(connection, cx));
        }
        self.reload_metadata(cx);
    }

    /// Build the create-database dialog, if one is not already open.
    pub(crate) fn open_create_database_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.create_database.is_some() {
            return;
        }

        let view = cx.new(|cx| CreateDatabaseView::new(self.connection.clone(), window, cx));
        cx.subscribe_in(&view, window, Self::on_create_database_event)
            .detach();

        let session = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let session = session.clone();
            dialog
                .title("Create database")
                .w(px(400.))
                // The dialog's own keys are off; `escape` is bound to
                // `CloseCreateDatabase` on the body instead.
                .keyboard(false)
                .on_close(move |_, _window, cx| {
                    session
                        .update(cx, |this, cx| {
                            this.create_database = None;
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        // Put the keyboard on the name box so typing starts at once.
        view.update(cx, |view, cx| view.focus(window, cx));
        self.create_database = Some(view);
    }

    /// The dialog's own buttons: dismiss it, or reload the database list
    /// after a create.
    fn on_create_database_event(
        &mut self,
        _: &Entity<CreateDatabaseView>,
        event: &CreateDatabaseEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CreateDatabaseEvent::Dismissed => {
                self.create_database = None;
                window.close_dialog(cx);
                cx.notify();
            }
            CreateDatabaseEvent::Created => {
                // The new database is on the server now; re-read the list so
                // the picker offers it.
                self.reload_metadata(cx);
            }
        }
    }

    /// The database dropdown, rendered by whoever owns the toolbar.
    ///
    /// Sized and styled here rather than by the caller: `dropdown_menu`
    /// wraps the button in a popover that does not itself implement
    /// [`Sizable`]/[`ButtonVariants`].
    pub(crate) fn render_database_picker(
        session: &Entity<Session>,
        cx: &mut gpui_kit::App,
    ) -> impl IntoElement {
        let this = session.read(cx);
        let current = this.connection.database().to_string();

        // A SQLite connection is one file: there is nothing to switch to, and
        // its "databases" (main, plus attachments) are not separate files.
        if this.connection.config.engine.is_file_based() {
            return Button::new("database")
                .custom(crate::ui::subtle_button(cx))
                .xsmall()
                .max_w(px(160.))
                .icon(gpui_kit::assets::IconName::Database)
                .label(crate::db::file_name(&current))
                .disabled(true)
                .into_any_element();
        }

        let databases = this.databases.clone();
        let weak = session.downgrade();
        // SQLite is one file: there is no second database to create. (The
        // picker itself is a disabled button for a file-based engine, so this
        // is belt and braces should that ever change.)
        let can_create_database = !this.connection.config.engine.is_file_based();

        let label = if this.reconnecting {
            "Reconnecting…".to_string()
        } else if this.switching {
            "Switching…".to_string()
        } else if current.is_empty() {
            "No database".to_string()
        } else {
            current.clone()
        };

        Button::new("database")
            .custom(crate::ui::subtle_button(cx))
            .xsmall()
            .max_w(px(160.))
            .icon(gpui_kit::assets::IconName::Database)
            .label(label)
            .dropdown_menu(move |mut menu, _window, _cx| {
                // First, so it is there however long the list below runs,
                // and even when the list could not be read because the
                // connection is what failed.
                // A click rather than the `Reconnect` action: the picker sits
                // in the workspace's title bar, outside the session's own
                // element, so the action would never reach it.
                let reconnect = weak.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new("Reconnect").on_click(move |_, window, cx| {
                            if let Some(session) = reconnect.upgrade() {
                                session.update(cx, |session, cx| {
                                    session.request_reconnect(window, cx)
                                });
                            }
                        }),
                    )
                    .separator();
                if databases.is_empty() {
                    menu = menu.label("No databases");
                } else {
                    for database in &databases {
                        let name = database.clone();
                        let weak = weak.clone();

                        menu = menu.item(
                            PopupMenuItem::new(database.clone())
                                .checked(*database == current)
                                .on_click(move |_, window, cx| {
                                    let name = name.clone();
                                    if let Some(session) = weak.upgrade() {
                                        session.update(cx, |session, cx| {
                                            session.request_switch_database(name, window, cx)
                                        });
                                    }
                                }),
                        );
                    }
                }

                if can_create_database {
                    let weak = weak.clone();
                    menu = menu
                        .separator()
                        .item(PopupMenuItem::new("Create database…").on_click(
                            move |_, window, cx| {
                                if let Some(session) = weak.upgrade() {
                                    session.update(cx, |session, cx| {
                                        session.open_create_database_dialog(window, cx)
                                    });
                                }
                            },
                        ));
                }

                menu.scrollable(true).max_h(px(420.))
            })
            .into_any_element()
    }
}

/// What re-reading `count` tabs from a new connection throws away, for
/// `doing` ("Switching database", "Reconnecting").
fn unapplied_sentence(count: usize, doing: &str) -> String {
    match count {
        1 => format!(
            "A tab has changes that have not been applied. {doing} reads it again, \
             which discards them."
        ),
        count => format!(
            "{count} tabs have changes that have not been applied. {doing} reads them \
             again, which discards them."
        ),
    }
}
