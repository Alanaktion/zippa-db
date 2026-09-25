//! The server's own process list (`pg_stat_activity` /
//! `information_schema.processlist`), with a way to end one. Postgres and
//! MySQL only — SQLite has no server to ask, so this tab is never offered
//! for one.
//!
//! Reuses [`DataGrid`] the same way [`crate::ui::console`] does — the result
//! is a plain `QueryResult`, so a system view is shown exactly like a table's
//! rows. Ending a connection reuses the grid's own row-picking (the
//! checkboxes, `Space`, `Cmd+A`) rather than a bespoke selection of its own:
//! pick the rows, then "End Selected".

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, SharedString, Window, div};

use crate::db::{Connection, runtime};
use crate::ui::data_grid::{DataGrid, Scope};

pub struct ProcessListView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    loading: bool,
    error: Option<String>,
    notice: Option<String>,
}

impl ProcessListView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| DataGrid::new(connection.config.engine, window, cx));
        let mut this = Self {
            connection,
            grid,
            loading: false,
            error: None,
            notice: None,
        };
        this.refresh(cx);
        this
    }

    /// Point this view at another connection — a database switch reopens the
    /// pool, so the server it asks moves with it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.processes().await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(result)) => {
                        this.grid.update(cx, |grid, cx| grid.set_result(result, cx));
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => this.error = Some("loading the process list was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask before ending the picked connections, the way a destructive table
    /// action already does — there is no staging for this, so it asks every
    /// time rather than following the connection's safety mode.
    fn kill_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = self.grid.read(cx).snapshot(Scope::Picked, cx);
        let ids: Vec<String> = picked
            .rows
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
            .collect();
        if ids.is_empty() {
            return;
        }

        let count = ids.len();
        let view = cx.entity().downgrade();

        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let ids = ids.clone();

            alert
                .title(if count == 1 {
                    "End this connection?".to_string()
                } else {
                    format!("End {count} connections?")
                })
                .description("Whatever it is running stops immediately; this cannot be undone.")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("End")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.run_kill(ids.clone(), cx));
                    }
                    true
                })
        });
    }

    fn run_kill(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let count = ids.len();
        let task = runtime::spawn(async move {
            let mut failed = 0;
            for id in ids {
                if connection.kill_process(&id).await.is_err() {
                    failed += 1;
                }
            }
            failed
        });

        cx.spawn(async move |this, cx| {
            let failed = task.await.unwrap_or(count);
            this.update(cx, |this, cx| {
                this.notice = Some(if failed == 0 {
                    format!(
                        "Ended {count} connection{}",
                        if count == 1 { "" } else { "s" }
                    )
                } else {
                    format!("Ended {}, {failed} failed", count - failed)
                });
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let read_only = self.connection.config.safety.is_read_only();

        h_flex()
            .id("process-list-toolbar")
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading {
                        "Loading…".to_string()
                    } else {
                        self.notice.clone().unwrap_or_default()
                    }),
            )
            .when(!read_only, |this| {
                this.child(
                    Button::new("kill-selected-processes")
                        .ghost()
                        .xsmall()
                        .disabled(self.loading)
                        .label("End Selected")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.kill_selected(window, cx)),
                        ),
                )
            })
            .child(
                Button::new(SharedString::from("refresh-process-list"))
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
    }
}

impl Focusable for ProcessListView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for ProcessListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("process-list-view")
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
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}
