//! SQLite's housekeeping: the integrity checks, `PRAGMA optimize`, `ANALYZE`,
//! `VACUUM`, and a WAL checkpoint, each a button that runs its statement and
//! shows what came back. SQLite only — the other engines have a server to
//! look after these.
//!
//! Reuses [`DataGrid`] the way [`crate::ui::query_digest`] does, so the rows
//! an integrity check reports read like any other result. The list of tasks
//! is closed ([`Maintenance`]); full pragma management is meant to grow out of
//! this tab rather than a second one.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariant};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, SharedString, Window, div};

use crate::db::query::QueryResult;
use crate::db::{Connection, Maintenance, runtime};
use crate::ui::data_grid::DataGrid;

pub struct SqliteMaintenanceView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    /// The task in flight, which also dims every button until it lands.
    running: Option<Maintenance>,
    /// What the last task said about itself, beside the rows it returned.
    notice: Option<String>,
    error: Option<String>,
}

impl SqliteMaintenanceView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| DataGrid::new(connection.config.engine, window, cx));
        Self {
            connection,
            grid,
            running: None,
            notice: None,
            error: None,
        }
    }

    /// Point this view at another connection. A database switch reopens the
    /// pool, so what was last reported no longer describes it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.notice = None;
        self.error = None;
        self.grid
            .update(cx, |grid, cx| grid.set_result(QueryResult::default(), cx));
        cx.notify();
    }

    /// Run `task`, asking first when the connection holds writes for
    /// confirmation and the task is one.
    fn request(&mut self, task: Maintenance, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        if !(task.writes() && self.connection.config.safety.confirms_writes()) {
            self.run(task, cx);
            return;
        }

        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title(format!("Run {}?", task.label()))
                .description(format!("{}.\n\n{}", task.description(), task.sql()))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Run")
                        .ok_variant(ButtonVariant::Primary)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.run(task, cx));
                    }
                    true
                })
        });
    }

    fn run(&mut self, task: Maintenance, cx: &mut Context<Self>) {
        self.running = Some(task);
        self.notice = None;
        self.error = None;
        cx.notify();

        let connection = self.connection.clone();
        let handle = runtime::spawn(async move { connection.run_maintenance(task).await });

        cx.spawn(async move |this, cx| {
            let outcome = handle.await;
            this.update(cx, |this, cx| {
                this.running = None;
                match outcome {
                    Ok(Ok(result)) => {
                        this.notice = Some(if result.rows.is_empty() {
                            format!(
                                "{} finished in {} ms",
                                task.label(),
                                result.elapsed.as_millis()
                            )
                        } else {
                            format!("{}: {}", task.label(), result.summary())
                        });
                        this.grid.update(cx, |grid, cx| grid.set_result(result, cx));
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => this.error = Some(format!("{} was cancelled", task.label())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub(crate) fn grid_for_test(&self) -> Entity<DataGrid> {
        self.grid.clone()
    }

    #[cfg(test)]
    pub(crate) fn run_for_test(&mut self, task: Maintenance, cx: &mut Context<Self>) {
        self.run(task, cx);
    }

    #[cfg(test)]
    pub(crate) fn report_for_test(&self) -> (Option<&str>, Option<&str>) {
        (self.notice.as_deref(), self.error.as_deref())
    }

    fn render_task(&self, task: Maintenance, cx: &mut Context<Self>) -> Button {
        let read_only = self.connection.config.safety.is_read_only();
        let blocked = task.writes() && read_only;
        let tooltip = if blocked {
            format!("{} — not available on a read-only connection", task.sql())
        } else {
            format!("{}\n{}", task.description(), task.sql())
        };

        Button::new(SharedString::from(format!("sqlite-maintenance-{task:?}")))
            .outline()
            .small()
            .label(task.label())
            .loading(self.running == Some(task))
            .disabled(self.running.is_some() || blocked)
            .tooltip(SharedString::from(tooltip))
            .on_click(cx.listener(move |this, _, window, cx| this.request(task, window, cx)))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tasks: Vec<_> = Maintenance::ALL
            .into_iter()
            .map(|task| self.render_task(task, cx))
            .collect();

        h_flex()
            .id("sqlite-maintenance-toolbar")
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .children(tasks)
    }
}

impl Focusable for SqliteMaintenanceView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for SqliteMaintenanceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("sqlite-maintenance-view")
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
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}
