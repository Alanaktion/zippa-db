//! Running the user's SQL: single statements, scripts, `EXPLAIN`, the
//! confirmation a careful connection asks for, and cancelling.

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, Context, Entity, Window};

use crate::db::{
    Blocker, Decision, Explained, OnFailure, ScriptFailure, ScriptMode, ScriptOutcome, ScriptRun,
    Step, runtime, statement, transaction_blocker,
};

use super::panel::close_script;
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
        if self.connection.config.safety.confirms_writes()
            && statement::first_write(&sql, self.connection.config.engine).is_some()
        {
            self.confirm_write(panel.clone(), sql, window, cx);
            return;
        }

        self.send(panel, sql, cx);
    }

    /// Run every statement in `sql`, one after another.
    ///
    /// Inside one transaction that pauses on each failure to ask, unless the
    /// buffer holds a statement a transaction cannot hold — then the user is
    /// asked first whether to run it without one. `ignoring_errors` runs it
    /// without a transaction and past every failure, for a migration that
    /// was partly applied before.
    pub(super) fn run_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        ignoring_errors: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let confirm = self.connection.config.safety.confirms_writes()
            && statement::first_write(&sql, self.connection.config.engine).is_some();

        let (mode, on_failure, blocker) = if ignoring_errors {
            (ScriptMode::Autocommit, OnFailure::Skip, None)
        } else {
            match transaction_blocker(
                self.connection.config.engine,
                &statement::split(&sql, self.connection.config.engine),
            ) {
                Some(blocker) => (ScriptMode::Autocommit, OnFailure::Ask, Some(blocker)),
                None => (ScriptMode::Transaction, OnFailure::Ask, None),
            }
        };

        // A script is confirmed as a whole: what the user is being asked
        // about is the buffer they are about to run.
        if confirm || blocker.is_some() {
            self.confirm_script(
                panel.clone(),
                sql,
                mode,
                on_failure,
                blocker,
                confirm,
                window,
                cx,
            );
            return;
        }

        self.send_script(panel, sql, mode, on_failure, cx);
    }

    /// Ask before running a script: because it writes on a connection that
    /// confirms writes, because it cannot run in a transaction, or both —
    /// one question either way.
    #[allow(clippy::too_many_arguments)]
    fn confirm_script(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        mode: ScriptMode,
        on_failure: OnFailure,
        blocker: Option<Blocker>,
        writes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One question at a time, the way a statement is asked about.
        if window.has_active_dialog(cx) {
            return;
        }

        let statements = statement::split(&sql, self.connection.config.engine).len();
        let mut description = Vec::new();
        if let Some(blocker) = &blocker {
            description.push(format!(
                "{}, so each statement will commit as it runs and an error cannot be rolled \
                 back.",
                blocker.message()
            ));
        } else if mode == ScriptMode::Autocommit {
            description.push(
                "Each statement commits as it runs, and a statement that fails is skipped."
                    .to_string(),
            );
        }
        if writes {
            description.push(format!(
                "{statements} statements change data on {}.",
                self.display_target()
            ));
        }
        let description = description.join(" ");
        let title = if mode == ScriptMode::Autocommit {
            "Run this script without a transaction?"
        } else {
            "Run this script?"
        };
        let ok = if blocker.is_some() {
            "Run without transaction"
        } else {
            "Run"
        };

        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            alert
                .title(title)
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(ok)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| {
                            session.send_script(&panel, sql.clone(), mode, on_failure, cx)
                        });
                    }
                    true
                })
        });
    }

    /// Start a script for `panel`; its grid and status follow it.
    fn send_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        mode: ScriptMode,
        on_failure: OnFailure,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, _)) = panel.read(cx).query_parts() else {
            return;
        };

        // One run per tab, paused or not; see `send`.
        if panel.read(cx).has_running_task() {
            return;
        }

        // A run is a natural checkpoint for the buffer text.
        cx.emit(SessionEvent::Changed);

        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));

        // Reads change nothing the sidebar shows, so only a script that
        // writes asks for the schema to be read again afterwards.
        let writes = statement::first_write(&sql, self.connection.config.engine).is_some();
        let Some(pinned) = panel.update(cx, |panel, _| panel.pin(&self.connection)) else {
            return;
        };
        let task =
            runtime::spawn(async move { ScriptRun::start(pinned, &sql, mode, on_failure).await });
        self.follow_script(panel, task, writes, cx);
    }

    /// Wait for the next step of a script, and act on where it ends up.
    fn follow_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        task: runtime::Task<anyhow::Result<Step>>,
        writes: bool,
        cx: &mut Context<Self>,
    ) {
        panel.update(cx, |panel, _| panel.set_running(task.abort_handle()));

        let weak = panel.downgrade();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let Some(panel) = weak.upgrade() else {
                    // The tab was closed while the script ran; a run paused
                    // on a failure still holds a connection to let go of.
                    if let Ok(Ok(Step::Paused(run, _))) = result {
                        close_script(run);
                    }
                    return;
                };
                panel.update(cx, |panel, _| panel.set_running(None));

                match result {
                    Ok(Ok(Step::Paused(run, failure))) => {
                        let mode = run.mode();
                        panel.update(cx, |panel, _| panel.park_script(run));
                        this.ask_about_failure(panel, failure, mode, writes, window, cx);
                    }
                    Ok(Ok(Step::Finished(outcome))) => {
                        this.finish_script(&panel, outcome, writes, cx)
                    }
                    Ok(Err(error)) => this.show_run_error(&panel, error, window, cx),
                    // The sender is dropped when the run is given up on,
                    // which is what cancelling does.
                    Err(_) => panel.update(cx, |panel, cx| {
                        panel.note_cancelled();
                        cx.notify();
                    }),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ask what to do about a statement that failed, with the run paused on
    /// it.
    ///
    /// Closing the question any other way than its buttons — `Escape` — is
    /// taken as the careful answer: roll back, or stop.
    fn ask_about_failure(
        &mut self,
        panel: Entity<SessionPanel>,
        failure: ScriptFailure,
        mode: ScriptMode,
        writes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let transaction = mode == ScriptMode::Transaction;
        let description = format!(
            "{} failed: {}\n\n{}",
            failure.position(),
            failure.message,
            if transaction {
                "Nothing has been committed yet. Roll back to undo the whole script, or skip \
                 this statement and carry on in the same transaction."
            } else {
                "The statements before it have already been applied. Stop here, or skip this \
                 statement and carry on."
            }
        );

        let session = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let answer = |decision: Decision| {
                let session = session.clone();
                let panel = panel.clone();
                move |window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    session
                        .update(cx, |session, cx| {
                            session.resume_script(&panel, decision, writes, cx)
                        })
                        .ok();
                }
            };
            let skip_all = answer(Decision::SkipAll);
            let skip = answer(Decision::Skip);
            let abort = answer(Decision::Abort);
            let cancel = answer(Decision::Abort);
            let close = answer(Decision::Abort);

            alert
                .title("A statement failed")
                .description(description.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("script-skip-all")
                                .label("Skip all errors")
                                .on_click(move |_: &ClickEvent, window, cx| skip_all(window, cx)),
                        )
                        .child(
                            Button::new("script-skip")
                                .label("Skip")
                                .on_click(move |_: &ClickEvent, window, cx| skip(window, cx)),
                        )
                        .child(
                            Button::new("script-abort")
                                .primary()
                                .label(if transaction { "Roll back" } else { "Stop" })
                                .on_click(move |_: &ClickEvent, window, cx| abort(window, cx)),
                        ),
                )
                // `resume_script` takes the paused run, so whichever of these
                // fires second finds nothing left to answer.
                .on_cancel(move |_, window, cx| {
                    cancel(window, cx);
                    true
                })
                .on_close(move |_, window, cx| close(window, cx))
        });
    }

    /// Carry a paused script on with the user's answer.
    fn resume_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        decision: Decision,
        writes: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = panel.update(cx, |panel, _| panel.take_script()) else {
            return;
        };
        let task = runtime::spawn(async move { run.resume(decision).await });
        self.follow_script(panel, task, writes, cx);
    }

    /// Put a finished script's results in its tab, with its failures as a
    /// result of their own after them.
    fn finish_script(
        &mut self,
        panel: &Entity<SessionPanel>,
        outcome: ScriptOutcome,
        writes: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some((editor, _)) = panel.read(cx).query_parts() {
            editor.update(cx, |editor, cx| editor.set_running(false, cx));
        }

        let summary = outcome.summary();
        let failed = outcome.rolled_back || outcome.stopped;
        let refresh = writes && outcome.applied_anything();
        let failures = outcome.failures_result();
        let mut results = outcome.results;
        results.extend(failures);

        panel.update(cx, |panel, cx| {
            panel.show_results(results, cx);
            if let Some(summary) = summary {
                panel.set_status(if failed {
                    Status::Error(summary)
                } else {
                    Status::Done(summary)
                });
            }
            cx.notify();
        });

        // The script may have created, altered, or dropped what the sidebar
        // and any open table show.
        if refresh {
            self.refresh(cx);
        }
    }

    /// A run that failed outright: the status bar and a notification say so,
    /// and the grid lets go of the last run's rows.
    fn show_run_error(
        &mut self,
        panel: &Entity<SessionPanel>,
        error: anyhow::Error,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, grid)) = panel.read(cx).query_parts() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.set_running(false, cx));
        let message = format!("{error:#}");
        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Error(message.clone()));
            cx.notify();
        });
        grid.update(cx, |grid, cx| grid.clear(cx));
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
    }

    /// Ask before running a write, on a connection that confirms them.
    ///
    /// The dialog closes over the buffer it is about, so the answer runs
    /// exactly what was on screen when the question was asked.
    pub(super) fn confirm_write(
        &mut self,
        panel: Entity<SessionPanel>,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One question at a time: a second Cmd+Enter while the dialog is open
        // must not stack another, or confirming both would run the write twice.
        if window.has_active_dialog(cx) {
            return;
        }

        let target = self.display_target();
        let session = cx.entity().downgrade();
        let engine = self.connection.config.engine;

        window.open_alert_dialog(cx, move |alert, _, _| {
            let session = session.clone();
            let panel = panel.clone();
            let sql = sql.clone();

            let description = match statement::first_write(&sql, engine) {
                Some(word) => format!("The {word} statement changes data on {target}."),
                None => format!("This changes data on {target}."),
            };

            alert
                .title("Run this statement?")
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Run")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(session) = session.upgrade() {
                        session.update(cx, |session, cx| session.send(&panel, sql.clone(), cx));
                    }
                    true
                })
        });
    }

    /// Send one statement for `panel`; its own grid and status follow it.
    pub(super) fn send(
        &mut self,
        panel: &Entity<SessionPanel>,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let Some((editor, grid)) = panel.read(cx).query_parts() else {
            return;
        };

        // A run already in flight keeps its abort handle: a second send would
        // overwrite it, orphaning the first query past cancelling. Cancel it
        // (Cmd+.) first to run something else.
        if panel.read(cx).has_running_task() {
            return;
        }

        // A run is a natural checkpoint for the buffer text.
        cx.emit(SessionEvent::Changed);

        panel.update(cx, |panel, cx| {
            panel.set_status(Status::Running);
            cx.notify();
        });
        editor.update(cx, |editor, cx| editor.set_running(true, cx));

        // The tab's own connection, so what one run opens — a transaction,
        // a `SET`, a temporary table — is still there for the next.
        let Some(pinned) = panel.update(cx, |panel, _| panel.pin(&self.connection)) else {
            return;
        };
        let task =
            runtime::spawn(async move { pinned.run_query(&sql).await.map(|result| vec![result]) });
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
                        Err(_) => panel.note_cancelled(),
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    /// Commit or roll back the transaction open in `panel`, for the status
    /// bar's buttons.
    ///
    /// Sent as the statement itself, so it shows in the console and the
    /// status bar like one the user typed; neither asks first, since neither
    /// writes anything a confirmation did not already cover.
    pub(super) fn end_transaction(
        &mut self,
        panel: &Entity<SessionPanel>,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        let sql = if commit { "COMMIT" } else { "ROLLBACK" };
        self.send(panel, sql.to_string(), cx);
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
        // One question at a time, the way a write is asked about.
        if window.has_active_dialog(cx) {
            return;
        }

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

        // The plan read keeps its abort handle the way a run does.
        if panel.read(cx).has_running_task() {
            return;
        }

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
                        Err(_) => panel.note_cancelled(),
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
