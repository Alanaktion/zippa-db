//! What the session knows about the server: the database list, the sidebar's
//! objects, the schema-search catalog, and switching database.

use std::sync::Arc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, px};

use crate::db::{Catalog, CatalogEntry, runtime};

use super::{Session, SessionEvent, Status};

/// A ghost button whose text is the theme's muted colour: it reads as
/// chrome until hovered, for controls that should not compete with content.
fn subtle_variant(cx: &gpui_kit::App) -> ButtonCustomVariant {
    let theme = cx.theme();
    ButtonCustomVariant::new(cx)
        .foreground(theme.muted_foreground)
        .hover(theme.accent)
        .active(theme.secondary_active)
}

impl Session {
    /// Read the database list and the current database's tables and views.
    pub(crate) fn reload_metadata(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
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
                self.catalog = Arc::new(catalog);
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
                self.catalog = Arc::new(fallback);
                self.catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    /// The whole schema, for [`SearchSchema`].
    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        self.catalog.clone()
    }

    pub(crate) fn catalog_loading(&self) -> bool {
        self.catalog_loading
    }

    pub(crate) fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
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
        if self.switching || database == self.connection.database() {
            return;
        }

        self.switching = true;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.with_database(&database).await });

        cx.spawn(async move |this, cx| {
            let opened = task.await;
            this.update_in(cx, |this, window, cx| {
                this.switching = false;
                match opened {
                    Ok(Ok(connection)) => {
                        let previous =
                            std::mem::replace(&mut this.connection, Arc::new(connection));
                        runtime::spawn(async move { previous.close().await });

                        this.objects.clear();
                        this.stored.clear();
                        this.catalog = Arc::new(Catalog::default());
                        this.catalog_loading = true;
                        this.catalog_error = None;
                        this.rebuild_tree(cx);
                        let connection = this.connection.clone();
                        for panel in this.panels.clone() {
                            let connection = connection.clone();
                            panel.update(cx, |panel, cx| panel.set_connection(connection, cx));
                        }
                        this.reload_metadata(cx);
                        cx.emit(SessionEvent::Changed);
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
                        let message = "switching database was cancelled";
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
                .custom(subtle_variant(cx))
                .xsmall()
                .max_w(px(160.))
                .icon(gpui_kit::assets::IconName::Database)
                .label(crate::db::file_name(&current))
                .disabled(true)
                .into_any_element();
        }

        let databases = this.databases.clone();
        let weak = session.downgrade();

        let label = if this.switching {
            "Switching…".to_string()
        } else if current.is_empty() {
            "No database".to_string()
        } else {
            current.clone()
        };

        Button::new("database")
            .custom(subtle_variant(cx))
            .xsmall()
            .max_w(px(160.))
            .icon(gpui_kit::assets::IconName::Database)
            .label(label)
            .dropdown_menu(move |mut menu, _window, _cx| {
                if databases.is_empty() {
                    return menu.label("No databases");
                }

                for database in &databases {
                    let name = database.clone();
                    let weak = weak.clone();

                    menu = menu.item(
                        PopupMenuItem::new(database.clone())
                            .checked(*database == current)
                            .on_click(move |_, _window, cx| {
                                let name = name.clone();
                                if let Some(session) = weak.upgrade() {
                                    session.update(cx, |session, cx| {
                                        session.switch_database(name, cx)
                                    });
                                }
                            }),
                    );
                }

                menu.scrollable(true).max_h(px(420.))
            })
            .into_any_element()
    }
}
