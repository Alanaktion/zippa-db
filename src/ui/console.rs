//! A read-only record of every statement this connection has sent to the
//! server: the user's own runs and the app's own reads and writes alike,
//! read from [`crate::db::Connection::query_log`].
//!
//! Pull-based rather than pushed to: `db/` has no UI to push into, so this
//! reads a fresh snapshot when it is built, on a database switch, and
//! whenever the toolbar's Refresh is pressed — never on its own.

use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, Window, div};

use crate::db::query::{Cell, QueryResult};
use crate::db::{Connection, LoggedQuery, QueryOutcome, QuerySource};
use crate::ui::data_grid::DataGrid;

pub struct ConsoleView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    entries: usize,
}

impl ConsoleView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| DataGrid::new(connection.config.engine, window, cx));
        let mut this = Self {
            connection,
            grid,
            entries: 0,
        };
        this.refresh(cx);
        this
    }

    /// Point this console at another connection's log — a database switch
    /// reopens the pool, so the log this view reads moves with it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    /// Re-read the log and show it, newest first.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let log = self.connection.query_log().snapshot();
        self.entries = log.len();
        let result = as_result(&log);
        self.grid.update(cx, |grid, cx| grid.set_result(result, cx));
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.connection.query_log().clear();
        self.refresh(cx);
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries
    }

    #[cfg(test)]
    pub(crate) fn grid_for_test(&self) -> Entity<DataGrid> {
        self.grid.clone()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.entries;
        h_flex()
            .id("console-toolbar")
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{count} {}",
                        if count == 1 {
                            "statement"
                        } else {
                            "statements"
                        }
                    )),
            )
            .child(div().flex_1())
            .child(
                Button::new("refresh-console")
                    .ghost()
                    .xsmall()
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
            .child(
                Button::new("clear-console")
                    .ghost()
                    .xsmall()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, _window, cx| this.clear(cx))),
            )
    }
}

impl Focusable for ConsoleView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for ConsoleView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("console-view")
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}

/// Lay the log out as a read-only result the grid already knows how to
/// show, newest entry first so the most recent activity needs no scrolling
/// to see.
fn as_result(log: &[LoggedQuery]) -> QueryResult {
    let columns = ["Time", "Source", "Ms", "SQL", "Result"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let column_types = vec!["TEXT".to_string(); 5];

    let rows: Vec<Vec<Cell>> = log
        .iter()
        .rev()
        .map(|entry| {
            vec![
                Some(format_time(entry.at)),
                Some(source_label(entry.source).to_string()),
                Some(entry.elapsed.as_millis().to_string()),
                Some(entry.sql.clone()),
                Some(outcome_label(&entry.outcome)),
            ]
        })
        .collect();

    QueryResult {
        columns,
        column_types,
        rows,
        ..QueryResult::default()
    }
}

fn source_label(source: QuerySource) -> &'static str {
    match source {
        QuerySource::User => "User",
        QuerySource::Internal => "Internal",
    }
}

fn outcome_label(outcome: &QueryOutcome) -> String {
    match outcome {
        QueryOutcome::Rows(count) => format!("{count} row{}", if *count == 1 { "" } else { "s" }),
        QueryOutcome::Affected(count) => {
            format!("{count} row{} affected", if *count == 1 { "" } else { "s" })
        }
        QueryOutcome::Ran => "ran".to_string(),
        QueryOutcome::Error(message) => format!("Error: {message}"),
    }
}

fn format_time(at: std::time::SystemTime) -> String {
    let local: DateTime<Local> = at.into();
    local.format("%H:%M:%S%.3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(source: QuerySource, outcome: QueryOutcome) -> LoggedQuery {
        LoggedQuery {
            sql: "select 1".to_string(),
            source,
            outcome,
            elapsed: Duration::from_millis(5),
            at: std::time::SystemTime::now(),
        }
    }

    #[test]
    fn newest_entry_comes_first() {
        let log = vec![
            entry(QuerySource::Internal, QueryOutcome::Rows(1)),
            entry(QuerySource::User, QueryOutcome::Affected(2)),
        ];
        let result = as_result(&log);
        assert_eq!(result.rows[0][1], Some("User".to_string()));
        assert_eq!(result.rows[1][1], Some("Internal".to_string()));
    }

    #[test]
    fn outcomes_are_labelled() {
        assert_eq!(outcome_label(&QueryOutcome::Rows(1)), "1 row");
        assert_eq!(outcome_label(&QueryOutcome::Rows(2)), "2 rows");
        assert_eq!(outcome_label(&QueryOutcome::Affected(1)), "1 row affected");
        assert_eq!(outcome_label(&QueryOutcome::Ran), "ran");
        assert_eq!(
            outcome_label(&QueryOutcome::Error("boom".into())),
            "Error: boom"
        );
    }
}
