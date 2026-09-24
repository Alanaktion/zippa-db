//! The slow/frequent query digest — `pg_stat_statements` on Postgres,
//! `performance_schema.events_statements_summary_by_digest` on MySQL —
//! ranked by mean time per call. Postgres and MySQL only: SQLite has no
//! server and no query instrumentation to read this way.
//!
//! Reuses [`DataGrid`] the same way [`crate::ui::process_list`] and
//! [`crate::ui::server_variables`] do. Both engines' instrumentation is
//! opt-in rather than always running, so `Connection::query_digest` answers
//! [`crate::db::QueryDigest::Unavailable`] with a plain reason instead of a
//! bare query error when it is off; this view shows that reason as a banner
//! rather than treating it as a failure.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, SharedString, Window, div};

use crate::db::query::QueryResult;
use crate::db::{Connection, QueryDigest, runtime};
use crate::ui::data_grid::DataGrid;

pub struct QueryDigestView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    loading: bool,
    error: Option<String>,
    /// Why the digest has nothing to show — the extension isn't installed,
    /// or the instrumentation is off — as opposed to `error`, which is an
    /// actual query failure.
    unavailable: Option<String>,
}

impl QueryDigestView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| DataGrid::new(connection.config.engine, window, cx));
        let mut this = Self {
            connection,
            grid,
            loading: false,
            error: None,
            unavailable: None,
        };
        this.refresh(cx);
        this
    }

    /// Point this view at another connection — a database switch reopens the
    /// pool, so the server it reads moves with it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.unavailable = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.query_digest().await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(QueryDigest::Available(result))) => {
                        this.grid.update(cx, |grid, cx| grid.set_result(result, cx));
                    }
                    Ok(Ok(QueryDigest::Unavailable(reason))) => {
                        this.unavailable = Some(reason);
                        this.grid
                            .update(cx, |grid, cx| grid.set_result(QueryResult::default(), cx));
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => this.error = Some("loading the query digest was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("query-digest-toolbar")
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .when(self.loading, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Loading…"),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new(SharedString::from("refresh-query-digest"))
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
    }
}

impl Focusable for QueryDigestView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for QueryDigestView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("query-digest-view")
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
            .when_some(self.unavailable.clone(), |this, reason| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason),
                )
            })
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}
