//! Server configuration — `pg_settings` on Postgres, `SHOW VARIABLES` (read
//! through `performance_schema`) on MySQL — searchable, with a way to see
//! only what has changed from its compiled default. Postgres and MySQL
//! only: SQLite is an embedded engine with no server-side configuration to
//! show this way.
//!
//! Reuses [`DataGrid`] the same way [`crate::ui::console`] and
//! [`crate::ui::process_list`] do, but the filter and the "changed only"
//! toggle are answered client-side rather than by re-querying the server:
//! the whole list is read once per refresh (`all`), and the grid is handed
//! whichever rows of it currently pass, the same shape [`text_filter`]
//! already gives the sidebar's object list and the row panel's column
//! filter.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, Window, div};
use regex::Regex;

use crate::db::query::{Cell, QueryResult};
use crate::db::{Connection, runtime};
use crate::ui::data_grid::DataGrid;
use crate::ui::text_filter;

pub struct ServerVariablesView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    filter_input: Entity<InputState>,
    filter: Option<Regex>,
    changed_only: bool,
    /// Every row read from the server; the grid shows whichever of them the
    /// filter and the toggle currently let through.
    all: QueryResult,
    loading: bool,
    error: Option<String>,
}

impl ServerVariablesView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| DataGrid::new(connection.config.engine, window, cx));
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter variables"));
        cx.subscribe_in(&filter_input, window, Self::on_filter_event)
            .detach();

        let mut this = Self {
            connection,
            grid,
            filter_input,
            filter: None,
            changed_only: false,
            all: QueryResult::default(),
            loading: false,
            error: None,
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
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.server_variables().await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(result)) => {
                        this.all = result;
                        this.apply_filter(cx);
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => {
                        this.error = Some("loading the server variables was cancelled".into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_filter_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let pattern = input.read(cx).value().trim().to_string();
        self.filter = text_filter::compile(&pattern);
        self.apply_filter(cx);
    }

    fn toggle_changed_only(&mut self, cx: &mut Context<Self>) {
        self.changed_only = !self.changed_only;
        self.apply_filter(cx);
    }

    /// Re-derive what the grid shows from `all`.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let result = filter_variables(&self.all, self.filter.as_ref(), self.changed_only);
        self.grid.update(cx, |grid, cx| grid.set_result(result, cx));
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("server-variables-toolbar")
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                Input::new(&self.filter_input)
                    .id("server-variables-filter")
                    .small()
                    .cleanable(true),
            )
            .child(
                Checkbox::new("changed-only")
                    .label("Changed only")
                    .checked(self.changed_only)
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_changed_only(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .text_right()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .when(self.loading, |this| this.child("Loading…")),
            )
            .child(
                Button::new("refresh-server-variables")
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
    }
}

impl Focusable for ServerVariablesView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for ServerVariablesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("server-variables-view")
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

/// `all`'s rows that pass the name filter and, when `changed_only`, whose
/// last column reads `yes`. `VARIABLES_SQL` puts the name first and the
/// `changed` flag last on both engines, so those are the only two positions
/// this needs to know regardless of how the rest of a row is shaped.
fn filter_variables(all: &QueryResult, filter: Option<&Regex>, changed_only: bool) -> QueryResult {
    let changed_index = all.columns.len().saturating_sub(1);
    let rows: Vec<Vec<Cell>> = all
        .rows
        .iter()
        .filter(|row| {
            filter.is_none_or(|filter| {
                row.first()
                    .and_then(|cell| cell.as_deref())
                    .is_some_and(|name| filter.is_match(name))
            })
        })
        .filter(|row| {
            !changed_only || row.get(changed_index).and_then(|cell| cell.as_deref()) == Some("yes")
        })
        .cloned()
        .collect();

    QueryResult {
        columns: all.columns.clone(),
        column_types: all.column_types.clone(),
        rows,
        ..QueryResult::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> QueryResult {
        QueryResult {
            columns: vec!["name".into(), "setting".into(), "changed".into()],
            rows: vec![
                vec![
                    Some("max_connections".into()),
                    Some("100".into()),
                    Some(String::new()),
                ],
                vec![
                    Some("work_mem".into()),
                    Some("64MB".into()),
                    Some("yes".into()),
                ],
                vec![
                    Some("shared_buffers".into()),
                    Some("128MB".into()),
                    Some(String::new()),
                ],
            ],
            ..QueryResult::default()
        }
    }

    #[test]
    fn no_filter_and_not_changed_only_keeps_everything() {
        let result = filter_variables(&fixture(), None, false);
        assert_eq!(result.rows.len(), 3);
    }

    #[test]
    fn the_name_filter_matches_case_insensitively() {
        let filter = text_filter::compile("MEM").unwrap();
        let result = filter_variables(&fixture(), Some(&filter), false);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0].as_deref(), Some("work_mem"));
    }

    #[test]
    fn changed_only_keeps_the_rows_flagged_yes() {
        let result = filter_variables(&fixture(), None, true);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0].as_deref(), Some("work_mem"));
    }

    #[test]
    fn both_filters_combine() {
        let filter = text_filter::compile("shared").unwrap();
        let result = filter_variables(&fixture(), Some(&filter), true);
        assert!(
            result.rows.is_empty(),
            "shared_buffers matches the name but was not changed"
        );
    }
}
