//! A session as `workspace.json` records it, and rebuilding one from that.

use std::path::PathBuf;

use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window};

use crate::db::runtime;
use crate::workspace_state::{PanelState, SessionState};

use super::{ObjectViewMode, Session, SessionEvent, SessionPanel, Status};

impl Session {
    /// This session as it would be restored: the connection it belongs to, the
    /// database it is on, and every tab in the order the dock shows them.
    pub(crate) fn snapshot(&self, cx: &App) -> SessionState {
        if let Some(restoring) = &self.restoring {
            return restoring.clone();
        }
        // Tabs are saved in the order the dock shows them, since a drag can
        // reorder the strip; restoring opens them in this order.
        let mut ordered = match self.panels.first() {
            Some(first) => self.strip(first, cx),
            None => Vec::new(),
        };
        for panel in &self.panels {
            if !ordered.contains(panel) {
                ordered.push(panel.clone());
            }
        }
        // Tabs with nothing restorable — a new-table tab mid-design — are
        // left out of the saved state entirely.
        let restorable: Vec<_> = ordered
            .into_iter()
            .filter(|panel| panel.read(cx).snapshot(cx).is_some())
            .collect();
        let active = self
            .active
            .as_ref()
            .and_then(|active| {
                restorable
                    .iter()
                    .position(|panel| panel.downgrade() == *active)
            })
            .unwrap_or(0);
        SessionState {
            connection: self.connection.config.id,
            database: Some(self.connection.database().to_string()),
            active,
            panels: restorable
                .iter()
                .filter_map(|panel| panel.read(cx).snapshot(cx))
                .collect(),
        }
    }

    /// Rebuild the tabs this session had, in order.
    ///
    /// A saved database that differs from the one the connection just opened
    /// on is switched to *first*, before any tab is built: building a table
    /// or query tab against the connection still on the old database would
    /// let its first read run before the switch lands. Usually that is just
    /// a moment of wrong data, quietly corrected once the switch's own
    /// reload replaces it — but a connection opened with no default database
    /// selected at all fails that read outright ("No database selected"
    /// on MySQL) rather than reading anything, and the error reaches the
    /// user as a toast before the correction does.
    pub(crate) fn restore(
        &mut self,
        state: SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match state.database.clone() {
            Some(database) if database != self.connection.database() => {
                self.restore_after_switching(database, state, window, cx);
            }
            _ => self.restore_panels(state, window, cx),
        }
    }

    /// Switch to `database`, then build the tabs against the connection that
    /// is actually on it. A database that cannot be reopened — dropped since
    /// the last run, say — falls back to restoring against the connection's
    /// own default database rather than stranding the tabs.
    fn restore_after_switching(
        &mut self,
        database: String,
        state: SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switching = true;
        self.restoring = Some(state.clone());
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.with_database(&database).await });

        cx.spawn_in(window, async move |this, cx| {
            let opened = task.await;
            this.update_in(cx, |this, window, cx| {
                this.switching = false;
                match opened {
                    Ok(Ok(connection)) => {
                        this.adopt_connection(connection, cx);
                        cx.emit(SessionEvent::Changed);
                    }
                    Ok(Err(error)) => {
                        crate::ui::notify_error(
                            window,
                            cx,
                            format!("Error: could not switch to the saved database: {error:#}"),
                        );
                    }
                    Err(_) => {
                        crate::ui::notify_error(
                            window,
                            cx,
                            "Error: switching to the saved database was cancelled".to_string(),
                        );
                    }
                }
                this.restoring = None;
                this.restore_panels(state, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Build every tab `state` names, against whatever connection the
    /// session currently has. Restoring goes through the same constructors a
    /// manual open does, so panel keys, `opened` numbering, and the dock all
    /// stay consistent. A restored buffer is not run; a restored table loads
    /// its first page the way opening it from the sidebar does.
    fn restore_panels(&mut self, state: SessionState, window: &mut Window, cx: &mut Context<Self>) {
        // `Session::new` always opens one empty editor; it is closed again
        // below once there is something restored to take its place.
        let placeholder = self.panels.first().cloned();
        let mut restored = 0;
        // Restored query tabs whose file is read to decide whether the buffer is
        // dirty. The reads happen after the loop, off the UI thread.
        let mut file_backed: Vec<(Entity<SessionPanel>, PathBuf)> = Vec::new();

        for panel in state.panels {
            match panel {
                PanelState::Query {
                    title,
                    sql,
                    file,
                    variables,
                } => {
                    let panel = self.open_tab(Some(title), sql.clone(), false, window, cx);
                    if !variables.is_empty()
                        && let Some((editor, _)) = panel.read(cx).query_parts()
                    {
                        editor.update(cx, |editor, cx| editor.set_variables(variables, cx));
                    }
                    if let Some(path) = file {
                        panel.update(cx, |panel, cx| panel.set_file(path.clone(), cx));
                        file_backed.push((panel, path));
                    }
                    restored += 1;
                }
                PanelState::Table { object, filters } => {
                    self.open_object(&object, ObjectViewMode::Data, window, cx);
                    self.restore_table_filters(&object, filters, window, cx);
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
                PanelState::Processes => {
                    // A no-op for a SQLite connection, which has no process
                    // list to show; nothing should count as restored then.
                    let before = self.panels.len();
                    self.open_process_list(window, cx);
                    if self.panels.len() > before {
                        restored += 1;
                    }
                }
                PanelState::Variables => {
                    // Also a no-op for SQLite, which has no server-side
                    // configuration to show.
                    let before = self.panels.len();
                    self.open_server_variables(window, cx);
                    if self.panels.len() > before {
                        restored += 1;
                    }
                }
                PanelState::Digest => {
                    // Also a no-op for SQLite, which has no query
                    // instrumentation to read this way.
                    let before = self.panels.len();
                    self.open_query_digest(window, cx);
                    if self.panels.len() > before {
                        restored += 1;
                    }
                }
                PanelState::Maintenance => {
                    // A no-op unless this is a SQLite connection.
                    let before = self.panels.len();
                    self.open_maintenance(window, cx);
                    if self.panels.len() > before {
                        restored += 1;
                    }
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
    }
}
