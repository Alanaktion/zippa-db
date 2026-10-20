//! `.sql` files and SQL dumps: open, save, save as, and the import dialog.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, WeakEntity, Window, px};

use crate::ui::import_dialog::{ImportEvent, ImportView};
use crate::ui::sql_file;

use super::save_dialog::{OnSaved, SaveQueryDialog};
use super::{
    ImportSqlDump, OpenFile, SaveFile, SaveFileAs, SaveQuery, SaveQueryAs, Session, SessionEvent,
    SessionPanel, Status,
};
use crate::saved_queries::{self, SavedQueries};

impl Session {
    pub(super) fn on_open_file(
        &mut self,
        _: &OpenFile,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(cx);
    }

    pub(super) fn on_save_file(
        &mut self,
        _: &SaveFile,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel() {
            self.save(&panel, false, cx);
        }
    }

    pub(super) fn on_save_query(
        &mut self,
        _: &SaveQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel() {
            self.save_query(&panel, false, window, cx);
        }
    }

    pub(super) fn on_save_query_as(
        &mut self,
        _: &SaveQueryAs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel() {
            self.save_query(&panel, true, window, cx);
        }
    }

    /// Keep `panel`'s buffer in the sidebar's Saved tab. A tab that belongs
    /// to a saved query still in the list updates it in place, with nothing
    /// to answer; otherwise — or with `as_new` — the dialog asks for a name,
    /// and the tab then belongs to the query saved under it.
    pub(crate) fn save_query(
        &mut self,
        panel: &Entity<SessionPanel>,
        as_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((sql, default_name)) = panel.read(cx).query_to_save(cx) else {
            return;
        };
        let linked = panel
            .read(cx)
            .saved_as()
            .map(str::to_string)
            .filter(|name| {
                SavedQueries::get(cx)
                    .iter()
                    .any(|query| &query.name == name)
            });

        if let (Some(name), false) = (linked, as_new) {
            saved_queries::upsert(&name, &sql, cx);
            panel.update(cx, |panel, cx| {
                panel.set_status(Status::Done(format!("Updated saved query “{name}”")));
                cx.notify();
            });
            return;
        }

        let weak = panel.downgrade();
        let on_saved: OnSaved = Rc::new(move |name, _window, cx| {
            weak.update(cx, |panel, cx| {
                panel.set_status(Status::Done(format!("Saved query “{name}”")));
                panel.set_saved_as(Some(name), cx);
            })
            .ok();
        });
        // Saving a linked tab as new starts from an empty name: its own
        // would only replace the query it is meant to leave alone.
        let default_name = if as_new { String::new() } else { default_name };
        SaveQueryDialog::open(sql, default_name, on_saved, window, cx);
    }

    pub(super) fn on_save_file_as(
        &mut self,
        _: &SaveFileAs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.active_panel() {
            self.save(&panel, true, cx);
        }
    }

    pub(super) fn on_import_dump(
        &mut self,
        _: &ImportSqlDump,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.import_dump(cx);
    }

    /// Ask for a dump file, then open the import dialog over the session.
    pub(crate) fn import_dump(&mut self, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_import(cx);

        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                this.update_in(cx, |this, window, cx| {
                    this.open_import_dialog(path, window, cx)
                })
                .ok();
            }
            Ok(None) => {}
            Err(error) => {
                this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Build the import dialog for `path`, if one is not already open.
    pub(super) fn open_import_dialog(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.import.is_some() {
            return;
        }

        let view = cx.new(|cx| ImportView::new(self.connection.clone(), path, window, cx));
        cx.subscribe_in(&view, window, Self::on_import_event)
            .detach();

        let session = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let session = session.clone();
            dialog
                .title("Import SQL dump")
                .w(px(560.))
                .h(px(460.))
                // The dialog's own keys are off so `enter` cannot dismiss it;
                // `escape` is bound to `CloseImport` on the body instead.
                .keyboard(false)
                // Closed from outside its body (the overlay, the close
                // button): a run still going is stopped rather than left
                // running with nothing on screen to stop it.
                .on_close(move |_, _window, cx| {
                    session
                        .update(cx, |this, cx| {
                            if let Some(view) = this.import.take() {
                                view.update(cx, |view, cx| view.cancel(cx));
                            }
                            cx.notify();
                        })
                        .ok();
                })
                .child(body.clone())
        });

        // Put the keyboard on the body so `escape` reaches `CloseImport`.
        view.update(cx, |view, cx| view.focus(window, cx));
        self.import = Some(view);
    }

    /// The dialog's own buttons: dismiss it, or note that the run changed the
    /// schema.
    fn on_import_event(
        &mut self,
        _: &Entity<ImportView>,
        event: &ImportEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ImportEvent::Dismissed => {
                self.import = None;
                window.close_dialog(cx);
                cx.notify();
            }
            ImportEvent::Finished => {
                // The import wrote objects and rows; the sidebar and any open
                // table are stale.
                self.refresh(cx);
            }
        }
    }

    /// Stop an import in flight, the way [`Self::cancel_query`] gives up
    /// on a query.
    pub(crate) fn cancel_import(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.import.clone() {
            view.update(cx, |view, cx| view.cancel(cx));
        }
    }

    /// Ask for SQL files and give each one its own tab.
    pub(crate) fn open_file(&mut self, cx: &mut Context<Self>) {
        let prompt = sql_file::prompt_for_open(cx);

        cx.spawn(async move |this, cx| {
            let paths = match prompt.await {
                Ok(Some(paths)) => paths,
                Ok(None) => return,
                Err(error) => {
                    this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                        .ok();
                    return;
                }
            };

            for path in paths {
                match cx.background_spawn(sql_file::read(path.clone())).await {
                    Ok(sql) => {
                        this.update_in(cx, |this, window, cx| {
                            this.open_file_tab(path, sql, window, cx)
                        })
                        .ok();
                    }
                    Err(error) => {
                        this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                            .ok();
                    }
                }
            }
        })
        .detach();
    }

    /// Show `sql` read from `path` in a new tab named after the file.
    fn open_file_tab(
        &mut self,
        path: PathBuf,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.open_tab(None, sql, false, window, cx);
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Done(format!("Opened {}", path.display())));
            panel.set_file(path, cx);
        });
    }

    /// Save `panel`'s query tab.
    ///
    /// A tab with no file yet — or "Save As", with `ask` set — asks the
    /// platform for a path first.
    pub(super) fn save(&mut self, panel: &Entity<SessionPanel>, ask: bool, cx: &mut Context<Self>) {
        // A table tab has no buffer to save, so saving it writes its staged
        // edits instead. `TableView` binds the same key itself, which covers
        // the grid having focus; this covers everywhere else in the session.
        if panel.read(cx).table_view().is_some() {
            panel.update(cx, |panel, cx| panel.commit(cx));
            return;
        }

        let Some(editor) = panel.read(cx).editor() else {
            return;
        };
        let file = panel.read(cx).file_path();
        let sql = editor.read(cx).sql(cx);

        if let Some(path) = file.clone().filter(|_| !ask) {
            self.write(panel.downgrade(), path, sql, cx);
            return;
        }

        let title = panel.read(cx).title();
        let prompt = sql_file::prompt_for_save(file.as_deref(), &title, "sql", cx);
        let weak = panel.downgrade();

        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                this.update(cx, |this, cx| this.write(weak, path, sql, cx))
                    .ok();
            }
            Ok(None) => {}
            Err(error) => {
                this.update_in(cx, |this, window, cx| this.report(error, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Write `sql` to `path`, then bind the tab to it.
    pub(super) fn write(
        &mut self,
        panel: WeakEntity<SessionPanel>,
        path: PathBuf,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let task = cx.background_spawn(sql_file::write(path.clone(), sql.clone()));

        cx.spawn(async move |this, cx| {
            let written = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the file was written.
                let Some(panel) = panel.upgrade() else {
                    return;
                };

                let error = match written {
                    Ok(()) => None,
                    Err(error) => Some(format!("{error:#}")),
                };
                panel.update(cx, |panel, cx| {
                    match &error {
                        None => {
                            panel.set_status(Status::Done(format!("Saved {}", path.display())));
                            panel.set_file(path, cx);
                            panel.set_baseline(sql);
                        }
                        Some(error) => panel.set_status(Status::Error(error.clone())),
                    }
                    cx.notify();
                });
                if let Some(error) = error {
                    crate::ui::notify_error(window, cx, format!("Error: {error}"));
                }
                // A save changes the file binding and the clean state, both of
                // which the saved workspace records.
                cx.emit(SessionEvent::Changed);
            })
            .ok();
        })
        .detach();
    }

    /// Put a file error where the user can see it.
    ///
    /// Only query tabs carry a status bar, so an error raised while a table
    /// tab is in front goes to the query tab nearest it.
    pub(super) fn report(
        &mut self,
        error: anyhow::Error,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = format!("{error:#}");
        // The toast is raised whether or not there is a status bar to put the
        // error in too: with no query tab open there is nowhere else to say it.
        if let Some(panel) = self.nearest_query_panel(cx) {
            panel.update(cx, |panel, cx| {
                panel.set_status(Status::Error(message.clone()));
                cx.notify();
            });
        }
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
    }

    /// The active panel if it holds a query, else the next one that does.
    fn nearest_query_panel(&self, cx: &App) -> Option<Entity<SessionPanel>> {
        if self.panels.is_empty() {
            return None;
        }
        let start = self.active_tab_index();
        (0..self.panels.len())
            .map(|offset| (start + offset) % self.panels.len())
            .map(|index| &self.panels[index])
            .find(|panel| panel.read(cx).is_query())
            .cloned()
    }
}
