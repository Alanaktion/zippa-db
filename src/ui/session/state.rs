//! A session as `workspace.json` records it, and rebuilding one from that.

use std::path::PathBuf;

use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window};

use crate::workspace_state::{PanelState, SessionState};

use super::{ObjectViewMode, Session, SessionPanel, Status};

impl Session {
    /// This session as it would be restored: the connection it belongs to, the
    /// database it is on, and every tab in creation order.
    pub(crate) fn snapshot(&self, cx: &App) -> SessionState {
        SessionState {
            connection: self.connection.config.id,
            database: Some(self.connection.database().to_string()),
            active: self.active_tab_index(),
            panels: self
                .panels
                .iter()
                .map(|panel| panel.read(cx).snapshot(cx))
                .collect(),
        }
    }

    /// Rebuild the tabs this session had, in order.
    ///
    /// Restoring goes through the same constructors a manual open does, so
    /// panel keys, `opened` numbering, and the dock all stay consistent. A
    /// restored buffer is not run; a restored table loads its first page the
    /// way opening it from the sidebar does.
    pub(crate) fn restore(
        &mut self,
        state: SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `Session::new` always opens one empty editor; it is closed again
        // below once there is something restored to take its place.
        let placeholder = self.panels.first().cloned();
        let mut restored = 0;
        // Restored query tabs whose file is read to decide whether the buffer is
        // dirty. The reads happen after the loop, off the UI thread.
        let mut file_backed: Vec<(Entity<SessionPanel>, PathBuf)> = Vec::new();

        for panel in state.panels {
            match panel {
                PanelState::Query { title, sql, file } => {
                    let panel = self.open_tab(Some(title), sql.clone(), false, window, cx);
                    if let Some(path) = file {
                        panel.update(cx, |panel, cx| panel.set_file(path.clone(), cx));
                        file_backed.push((panel, path));
                    }
                    restored += 1;
                }
                PanelState::Table { object } => {
                    self.open_object(&object, ObjectViewMode::Data, window, cx);
                    restored += 1;
                }
                PanelState::Schema { object } => {
                    self.open_object(&object, ObjectViewMode::Schema, window, cx);
                    restored += 1;
                }
                PanelState::Console => {
                    self.open_console(window, cx);
                    restored += 1;
                }
                // Written by a newer build; leave it out rather than guess.
                PanelState::Unknown => {}
            }
        }

        // The saved buffer wins: re-reading the file could replace unsaved text.
        // The file is only read to decide whether the restored buffer matches it,
        // so an unsaved buffer shows as dirty and is asked about before closing.
        // Reading it is blocking I/O, so it goes to the background executor and
        // folds in when it lands; a file that cannot be read is reported rather
        // than left to look like an empty one.
        if !file_backed.is_empty() {
            let paths: Vec<PathBuf> = file_backed.iter().map(|(_, path)| path.clone()).collect();
            let reads = cx.background_spawn(async move {
                paths
                    .into_iter()
                    .map(|path| std::fs::read_to_string(&path).ok())
                    .collect::<Vec<_>>()
            });
            cx.spawn(async move |_this, cx| {
                let texts = reads.await;
                for ((panel, path), text) in file_backed.into_iter().zip(texts) {
                    panel.update(cx, |panel, _cx| match text {
                        Some(text) => panel.set_baseline(text),
                        None => {
                            panel.set_baseline(String::new());
                            panel.set_status(Status::Error(format!(
                                "could not read {}",
                                path.display()
                            )));
                        }
                    });
                }
            })
            .detach();
        }

        if restored > 0
            && let Some(placeholder) = placeholder
        {
            // Opening the restored tabs first and closing the placeholder
            // after keeps the "a session always shows one editor" rule. That
            // shifts the restored tabs down one, which is the order they were
            // saved in, so the saved active index applies directly.
            self.close_tab_now(&placeholder, window, cx);
        }

        self.activate_tab(state.active, window, cx);

        if let Some(database) = state.database
            && database != self.connection.database()
        {
            self.switch_database(database, cx);
        }
    }
}
