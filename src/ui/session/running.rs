//! Running the user's SQL: single statements, scripts, `EXPLAIN`, the
//! confirmation a careful connection asks for, and cancelling.

use gpui_kit::component::WindowExt;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::{Context, Entity, Window};

use crate::db::{Explained, runtime, statement};

use super::{CancelQuery, Session, SessionEvent, SessionPanel, Status};

impl Session {
    /// Read the active tab's statement plan, for a caller that is not the
    /// editor's own shortcut — the quick switcher's Explain items.
    ///
    /// The editor is asked the way its own buttons ask it, so an `ANALYZE`
    /// still goes through the confirmation a careful connection wants, and the
    /// plan lands in the tab exactly as the shortcut would put it there. A tab
    /// that has no editor, or an empty one, has nothing to explain.
    pub(crate) fn explain_active(&mut self, analyze: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.active_panel() else {
            return;
        };
        let Some(editor) = panel.read(cx).editor() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.emit_explain(analyze, cx));
    }

    /// Run `sql` for `panel`, asking first where the connection says every
    /// write is confirmed.
    pub(super) fn run(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.confirm_write(panel.clone(), sql, false, window, cx);
            return;
        }

        self.send(panel, sql, false, cx);
    }

    /// Run every statement in `sql`, one after another.
    pub(super) fn run_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A script is confirmed as a whole: what the user is being asked
        // about is the buffer they are about to run.
        if self.connection.config.safety.confirms_writes() && statement::first_write(&sql).is_some()
        {
            self.confirm_write(panel.clone(), sql, true, window, cx);
            return;
        }

        self.send(panel, sql, true, cx);
    }

    /// Ask before running a write, on a connection that confirms them.
    ///
    /// The dialog closes over the buffer it is about, so the answer runs
    /// exactly what was on screen when the question was asked.
    pub(super) fn confirm_write(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        script: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.display_target();
        let statements = statement::split(&sql).len();
        let session = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            let description = match statement::first_write(&sql) {
                Some(_) if script => {
                    format!("{statements} statements change data on {target}.")
                }
                Some(word) => format!("The {word} statement changes data on {target}."),
                None => format!("This changes data on {target}."),
            };

            alert
                .title(if script {
                    "Run this script?"
                } else {
                    "Run this statement?"
                })
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Run")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.send(&panel, sql.clone(), script, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Send `sql` for `panel`; its own grid and status follow it.
    ///
    /// A script comes back as one result per statement; a single statement
    /// comes back as one result, so both land in the same place.
    pub(super) fn send(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        script: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, grid)) = panel.read(cx).query_parts() else {
            return;
        };

        // A run is a natural checkpoint for the buffer text.
        cx.emit(SessionEvent::Changed);

        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));

        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            if script {
                connection.run_script(&sql).await
            } else {
                connection
                    .run_query_as_user(&sql)
                    .await
                    .map(|result| vec![result])
            }
        });
        panel.update(cx, |panel, _| panel.set_running(task.abort_handle()));

        let weak = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the query ran.
                let Some(panel) = weak.upgrade() else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));
                panel.update(cx, |panel, cx| {
                    panel.set_running(None);

                    match result {
                        Ok(Ok(results)) => panel.show_results(results, cx),
                        Ok(Err(error)) => {
                            let message = format!("{error:#}");
                            panel.set_status(Status::Error(message.clone()));
                            grid.update(cx, |grid, cx| grid.clear(cx));
                            crate::ui::notify_error(window, cx, format!("Error: {message}"));
                        }
                        // The sender is dropped when the run is given up on,
                        // which is what cancelling does.
                        Err(_) => panel.set_status(Status::Done("Cancelled".into())),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Read one statement's plan for `panel`.
    ///
    /// `ANALYZE` is the only form that runs anything, so it is the only form
    /// a careful connection asks about first.
    pub(super) fn explain(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        analyze: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if analyze
            && !self.connection.config.safety.auto_applies()
            && !self.connection.config.safety.is_read_only()
        {
            self.confirm_explain(panel.clone(), sql, window, cx);
            return;
        }

        self.explain_now(panel, sql, analyze, cx);
    }

    /// Ask before running the query an `ANALYZE` would, the way a write is
    /// asked about: the statement the plan comes from is here, so the answer
    /// is the same either way.
    fn confirm_explain(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            alert
                .title("Explain and run this query?")
                .description(
                    "EXPLAIN ANALYZE runs the statement to report actual times, so the query \
                     really executes.",
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Explain")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.explain_now(&panel, sql.clone(), true, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Send `sql` for its plan; a tree lands in the tab's plan viewer, and a
    /// server that only knows the classic table lands in the grid.
    fn explain_now(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        analyze: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, grid)) = panel.read(cx).query_parts() else {
            return;
        };

        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.explain(&sql, analyze).await });
        panel.update(cx, |panel, _| panel.set_running(task.abort_handle()));

        let weak = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |_this, window, cx| {
                // The tab may have been closed while the plan was read.
                let Some(panel) = weak.upgrade() else {
                    return;
                };

                editor.update(cx, |editor, cx| editor.set_running(false, cx));
                panel.update(cx, |panel, cx| {
                    panel.set_running(None);

                    match result {
                        Ok(Ok(Explained::Plan(plan))) => panel.set_plan(plan, cx),
                        Ok(Ok(Explained::Rows(result))) => {
                            panel.show_results(vec![result], cx);
                            panel.set_status(Status::Done(
                                "Classic plan output; shown in the result grid".into(),
                            ));
                        }
                        Ok(Err(error)) => {
                            let message = format!("{error:#}");
                            panel.set_status(Status::Error(message.clone()));
                            grid.update(cx, |grid, cx| grid.clear(cx));
                            crate::ui::notify_error(window, cx, format!("Error: {message}"));
                        }
                        // The sender is dropped when the read is given up on,
                        // which is what cancelling does.
                        Err(_) => panel.set_status(Status::Done("Cancelled".into())),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Stop the run in the active tab.
    pub(super) fn cancel_query(
        &mut self,
        _: &CancelQuery,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An import in flight is the other long-running thing a session owns,
        // so the same key gives up on it.
        self.cancel_import(cx);
        if let Some(panel) = self.active_panel() {
            panel.update(cx, |panel, cx| {
                panel.cancel_running(cx);
            });
        }
    }
}
