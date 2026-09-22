//! The Import SQL Dump dialog.
//!
//! Opened from the session's sidebar, the File menu, or `Cmd`/`Ctrl`+`Shift`+`I`.
//! It runs in three states: a pre-flight where the user picks what a failure
//! should do, a running state with a progress bar, and a summary with the
//! errors that were collected.
//!
//! The view owns the import task and its abort handle, so cancelling from
//! inside the dialog or from the session's `Cmd`/`Ctrl`+`.` both reach it. The
//! work itself runs on [`runtime`](crate::db::runtime) against a single
//! dedicated connection; this view only reflects it.
//!
//! The dialog is `gpui_kit`'s [`Dialog`](gpui_kit::component::dialog::Dialog),
//! opened by the session, and this view is its body. It emits
//! [`ImportEvent::Dismissed`] when the user is done with it and
//! [`ImportEvent::Finished`] when a run ends, so the session can reload the
//! sidebar.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{ClipboardItem, Context, EventEmitter, SharedString, Window, actions, div};

use crate::db::import::{self, Preflight};
use crate::db::{
    Connection, Engine, ImportProgress, ImportRequest, ImportSummary, OnError, runtime,
};

actions!(zippa_db, [CloseImport]);

/// What the dialog tells its owner.
pub enum ImportEvent {
    /// The user is done with the dialog; the session takes it off screen.
    Dismissed,
    /// A run ended, whether it succeeded or failed; the sidebar is stale.
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Done,
}

pub struct ImportView {
    connection: Arc<Connection>,
    path: PathBuf,
    name: SharedString,
    preflight: Preflight,
    on_error: OnError,
    state: State,
    progress: ImportProgress,
    summary: Option<ImportSummary>,
    failure: Option<String>,
    abort: Option<tokio::task::AbortHandle>,
    /// The dialog body's own focus, so `escape` reaches `CloseImport`.
    focus: gpui_kit::FocusHandle,
}

impl EventEmitter<ImportEvent> for ImportView {}

impl ImportView {
    pub fn new(
        connection: Arc<Connection>,
        path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preflight = import::preflight(&path, connection.config.engine);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
            .into();

        Self {
            connection,
            path,
            name,
            preflight,
            on_error: OnError::default(),
            state: State::Ready,
            progress: ImportProgress::default(),
            summary: None,
            failure: None,
            abort: None,
            focus: cx.focus_handle(),
        }
    }

    /// Put the keyboard on the dialog body, where `escape` is handled.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// The reason the import cannot start, if there is one.
    fn blocked_reason(&self) -> Option<String> {
        if let Some(error) = &self.preflight.error {
            return Some(error.clone());
        }
        if self.connection.config.safety.is_read_only() {
            return Some(format!(
                "This connection is {} — importing would write to it.",
                self.connection.config.safety.label().to_lowercase()
            ));
        }
        None
    }

    /// The database a run writes into, named on the button.
    fn target(&self) -> String {
        if self.connection.config.engine.is_file_based() {
            crate::db::file_name(self.connection.database())
        } else {
            self.connection.database().to_string()
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.state != State::Ready || self.blocked_reason().is_some() {
            return;
        }

        let connection = self.connection.clone();
        let request = ImportRequest {
            path: self.path.clone(),
            on_error: self.on_error,
        };
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let task = runtime::spawn(async move { connection.import_dump(request, sender).await });

        self.abort = task.abort_handle();
        self.state = State::Running;
        self.progress = ImportProgress::default();
        self.summary = None;
        self.failure = None;
        cx.notify();

        // Progress arrives on its own channel while the run is in flight.
        cx.spawn(async move |this, cx| {
            while let Some(update) = receiver.recv().await {
                this.update(cx, |this, cx| {
                    this.progress = update;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.finish(result, cx)).ok();
        })
        .detach();
    }

    fn finish(
        &mut self,
        result: Result<
            Result<ImportSummary, anyhow::Error>,
            tokio::sync::oneshot::error::RecvError,
        >,
        cx: &mut Context<Self>,
    ) {
        self.abort = None;
        self.state = State::Done;

        match result {
            Ok(Ok(summary)) => self.summary = Some(summary),
            Ok(Err(error)) => self.failure = Some(format!("{error:#}")),
            // The sender is dropped when the run is given up on.
            Err(_) => self.failure = Some("Cancelled".to_string()),
        }
        // Whatever the outcome, statements may already have been applied — a
        // stopped run keeps what ran, and so does a cancelled one — so the
        // schema and any open table are stale either way.
        cx.emit(ImportEvent::Finished);
        cx.notify();
    }

    /// Give up on the run in flight, the way `Cmd`/`Ctrl`+`.` does.
    pub(crate) fn cancel(&mut self, _cx: &mut Context<Self>) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(ImportEvent::Dismissed);
    }

    fn on_close(&mut self, _: &CloseImport, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn copy_log(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.log()));
    }

    /// The whole run as text, for the clipboard.
    fn log(&self) -> String {
        let size = self
            .summary
            .as_ref()
            .map(|summary| summary.total_bytes)
            .filter(|bytes| *bytes > 0)
            .unwrap_or(self.preflight.total_bytes);
        let mut text = format!("Import of {} ({})\n", self.path.display(), bytes(size));
        if let Some(failure) = &self.failure {
            text.push_str(&format!("Failed: {failure}\n"));
        }
        if let Some(summary) = &self.summary {
            text.push_str(&format!("{}\n", summary.summary()));
            if summary.dump_transaction {
                text.push_str("The dump carried its own transaction; the import managed it.\n");
            }
            for error in &summary.errors {
                text.push_str(&format!(
                    "line {}: {}\n  {}\n",
                    error.line, error.statement, error.message
                ));
            }
            if summary.dropped_errors > 0 {
                text.push_str(&format!(
                    "…and {} more errors not listed.\n",
                    summary.dropped_errors
                ));
            }
        }
        text
    }

    fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let safety = self.connection.config.safety;
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_sm().child(self.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                bytes(self.preflight.total_bytes),
                                self.preflight.compression.label()
                            )),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "Into {} on {} · {}",
                        self.target(),
                        self.connection.config.engine.label(),
                        safety.label()
                    )),
            )
    }

    fn render_warnings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mismatch = self.preflight.dialect_mismatch.clone();
        let destructive = self.preflight.destructive;
        let fallback = self.connection.config.engine == Engine::MySql;

        v_flex()
            .gap_1()
            .when_some(mismatch, |this, warning| {
                this.child(warning_row(warning, cx.theme().warning))
            })
            .when(destructive > 0, |this| {
                this.child(warning_row(
                    format!(
                        "This dump contains {destructive} statement{} that drop or clear data.",
                        if destructive == 1 { "" } else { "s" }
                    ),
                    cx.theme().warning,
                ))
            })
            .when(fallback, |this| {
                this.child(warning_row(
                    "MySQL commits DDL itself, so rolling back is not possible; it falls back to stopping."
                        .to_string(),
                    cx.theme().muted_foreground,
                ))
            })
    }

    fn render_policy(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = self.connection.config.engine;
        let selected = OnError::ALL
            .iter()
            .position(|policy| *policy == self.on_error)
            .unwrap_or(0);

        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("If a statement fails"),
            )
            .child(
                RadioGroup::new("import-policy")
                    .selected_index(Some(selected))
                    .on_change(cx.listener(|this, index, _window, cx| {
                        if let Some(policy) = OnError::ALL.get(*index) {
                            this.on_error = *policy;
                            cx.notify();
                        }
                    }))
                    .children(OnError::ALL.iter().enumerate().map(|(ix, policy)| {
                        Radio::new(SharedString::from(format!("import-policy-{ix}")))
                            .label(SharedString::from(policy.label()))
                            // MySQL cannot honour a rollback; the option stays
                            // visible but disabled, which is how the reason is
                            // discoverable without pretending it works.
                            .disabled(engine == Engine::MySql && *policy == OnError::Rollback)
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.on_error.description()),
            )
    }

    fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let blocked = self.blocked_reason();
        let blocked_for_button = blocked.clone();

        v_flex()
            .flex_1()
            .gap_3()
            .child(self.render_details(cx))
            .when_some(blocked, |this, reason| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(format!("Cannot import: {reason}")),
                )
            })
            .child(self.render_warnings(cx))
            .child(self.render_policy(cx))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("import-cancel")
                            .ghost()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("import-start")
                            .primary()
                            .small()
                            .label(format!("Import into {}", self.target()))
                            .disabled(blocked_for_button.is_some())
                            .when_some(blocked_for_button, |button, reason| {
                                button.tooltip(SharedString::from(reason))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }

    fn render_running(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let percent = self.progress.percent();

        v_flex()
            .flex_1()
            .gap_3()
            .child(self.render_details(cx))
            .child(
                Progress::new("import-progress")
                    .small()
                    .value(percent)
                    .accessibility_label(SharedString::from(format!(
                        "Import progress: {:.0}%",
                        percent
                    ))),
            )
            .child(
                // The bar alone says nothing to a screen reader or a
                // colour-blind reader, so the numbers are always spoken out.
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{:.0}% · {} · {} · line {}",
                        percent,
                        plural(self.progress.statements, "statement"),
                        plural(self.progress.errors, "error"),
                        self.progress.line
                    )),
            )
            .child(
                h_flex().gap_2().justify_end().child(
                    Button::new("import-stop")
                        .outline()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                ),
            )
    }

    fn render_done(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let failures = self
            .summary
            .as_ref()
            .map(|summary| summary.errors.len() + summary.dropped_errors as usize)
            .unwrap_or(0);

        v_flex()
            .flex_1()
            .gap_3()
            .child(self.render_details(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(if self.failure.is_some() {
                        cx.theme().danger
                    } else if failures > 0 {
                        cx.theme().warning
                    } else {
                        cx.theme().success
                    })
                    .child(match (&self.failure, &self.summary) {
                        (Some(failure), _) => failure.clone(),
                        (None, Some(summary)) => summary.summary(),
                        (None, None) => "Done".to_string(),
                    }),
            )
            .when_some(self.render_errors(cx), |this, errors| this.child(errors))
            .when_some(self.notes(), |this, notes| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notes),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("import-copy")
                            .ghost()
                            .small()
                            .label("Copy log")
                            .accessibility_label("Copy the import log to the clipboard")
                            .on_click(cx.listener(|this, _, _, cx| this.copy_log(cx))),
                    )
                    .child(
                        Button::new("import-close")
                            .primary()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
    }

    /// A line of small print for the ways a run's shape differed from what
    /// happened: a dump that carried its own transaction, or a rollback that
    /// had to become a stop.
    fn notes(&self) -> Option<String> {
        let summary = self.summary.as_ref()?;
        let mut notes = Vec::new();
        if summary.dump_transaction {
            notes.push(
                "The dump carried its own BEGIN/COMMIT; the import managed them.".to_string(),
            );
        }
        if summary.mysql_rollback_fallback {
            notes.push(
                "MySQL cannot roll a dump back, so the run stopped at the first error.".to_string(),
            );
        }
        (!notes.is_empty()).then(|| notes.join(" "))
    }

    fn render_errors(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let summary = self.summary.as_ref()?;
        if summary.errors.is_empty() {
            return None;
        }

        let rows = summary.errors.iter().map(|error| {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error at line {}: {}", error.line, error.message)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(error.statement.clone()),
                )
        });

        Some(
            div()
                .id("import-errors")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .p_2()
                .child(v_flex().gap_2().children(rows)),
        )
    }
}

impl Render for ImportView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("import-dialog")
            .test_support()
            .size_full()
            .gap_3()
            .track_focus(&self.focus)
            .key_context("ImportDialog")
            .on_action(cx.listener(Self::on_close))
            .child(match self.state {
                State::Ready => self.render_ready(cx).into_any_element(),
                State::Running => self.render_running(cx).into_any_element(),
                State::Done => self.render_done(cx).into_any_element(),
            })
    }
}

/// A dim warning line, in words rather than colour alone.
fn warning_row(message: String, color: gpui_kit::Hsla) -> impl IntoElement {
    div().text_xs().text_color(color).child(message)
}

/// `"1 statement"` / `"3 statements"`.
fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// A file size, in units a person reads.
fn bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
impl ImportView {
    pub(crate) fn is_ready_for_test(&self) -> bool {
        self.state == State::Ready
    }

    pub(crate) fn is_done_for_test(&self) -> bool {
        self.state == State::Done
    }

    pub(crate) fn blocked_reason_for_test(&self) -> Option<String> {
        self.blocked_reason()
    }

    pub(crate) fn set_on_error_for_test(&mut self, policy: OnError, cx: &mut Context<Self>) {
        self.on_error = policy;
        cx.notify();
    }

    pub(crate) fn start_for_test(&mut self, cx: &mut Context<Self>) {
        self.start(cx);
    }

    pub(crate) fn summary_for_test(&self) -> Option<ImportSummary> {
        self.summary.clone()
    }

    pub(crate) fn copy_log_for_test(&self, cx: &mut Context<Self>) {
        self.copy_log(cx);
    }

    pub(crate) fn dismiss_for_test(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
    }
}
