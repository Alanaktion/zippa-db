//! The History tab: the statements the user ran from query tabs, kept by
//! [`crate::db::history`] one store per connection.
//!
//! It shows this connection's history by default, and every connection's
//! merged by time with "All connections" ticked. Like the console it is
//! pull-based: the store is read when the tab opens, on Refresh, on a
//! database switch, and after a query tab's run finishes (the session asks).
//! A row's SQL opens in a new query tab through [`HistoryEvent::Open`]; the
//! session does the opening, since a tab cannot reach its siblings.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, Window, div};
use regex::Regex;
use uuid::Uuid;

use crate::db::Connection;
use crate::db::history::{self, HistoryEntry, Keep};
use crate::db::query::{Cell, QueryResult};
use crate::settings::Settings;
use crate::ui::data_grid::{DataGrid, GridEdit};
use crate::ui::text_filter;
use crate::ui::welcome::SavedConnections;

/// What the History tab asks of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryEvent {
    /// Open this SQL in a new query tab.
    Open(String),
}

pub struct HistoryView {
    connection: Arc<Connection>,
    grid: Entity<DataGrid>,
    filter_input: Entity<InputState>,
    filter: Option<Regex>,
    /// Every connection's history rather than this one's.
    all_connections: bool,
    /// What the store held at the last read, newest first; the grid shows
    /// whichever of it the filter lets through.
    entries: Vec<HistoryEntry>,
    /// Saved connections' names by id, for the "All connections" column.
    names: HashMap<Uuid, String>,
    loading: bool,
}

impl EventEmitter<HistoryEvent> for HistoryView {}

impl HistoryView {
    pub fn new(connection: Arc<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let grid = cx.new(|cx| {
            let mut grid = DataGrid::new(connection.config.engine, window, cx);
            grid.set_placeholder("No query history yet", cx);
            grid
        });
        // The Open button follows the keyboard's row.
        cx.subscribe(&grid, |_, _, event: &GridEdit, cx| {
            if matches!(event, GridEdit::RowFocused(_)) {
                cx.notify();
            }
        })
        .detach();
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search history"));
        cx.subscribe_in(&filter_input, window, Self::on_filter_event)
            .detach();

        let mut this = Self {
            connection,
            grid,
            filter_input,
            filter: None,
            all_connections: false,
            entries: Vec::new(),
            names: HashMap::new(),
            loading: false,
        };
        this.refresh(cx);
        this
    }

    /// Follow the session to its new connection. A database switch keeps
    /// the connection's id, so its history stays the same store.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    /// Re-read the store: a file read, so on the background executor.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.names = SavedConnections::get(cx)
            .iter()
            .map(|config| (config.id, config.display_name()))
            .collect();
        self.names
            .entry(self.connection.config.id)
            .or_insert_with(|| self.connection.config.display_name());

        self.loading = true;
        cx.notify();
        let scope = (!self.all_connections).then_some(self.connection.config.id);
        cx.spawn(async move |this, cx| {
            let entries = cx
                .background_spawn(async move {
                    match scope {
                        Some(connection) => history::entries(connection),
                        None => history::all_entries(),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.entries = entries;
                this.apply_filter(cx);
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

    fn toggle_all_connections(&mut self, cx: &mut Context<Self>) {
        self.all_connections = !self.all_connections;
        self.refresh(cx);
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let shown: Vec<&HistoryEntry> =
            history::matching(&self.entries, self.filter.as_ref()).collect();
        let names = self.all_connections.then_some(&self.names);
        let result = as_result(&shown, names);
        self.grid.update(cx, |grid, cx| grid.set_result(result, cx));
        cx.notify();
    }

    /// The SQL of the row the keyboard is on, read from the grid so a sort
    /// the user applied there cannot point it at another row.
    fn focused_sql(&self, cx: &App) -> Option<String> {
        let grid = self.grid.read(cx);
        let row = grid.focused_row(cx)?;
        let column = sql_column(self.all_connections);
        grid.row_cells(row, cx)?.get(column).cloned().flatten()
    }

    fn open_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(sql) = self.focused_sql(cx) {
            cx.emit(HistoryEvent::Open(sql));
        }
    }

    /// Ask before forgetting: what is cleared cannot be brought back.
    fn confirm_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = (!self.all_connections).then_some(self.connection.config.id);
        let (title, description) = match scope {
            Some(_) => (
                format!(
                    "Clear the query history of \"{}\"?",
                    self.connection.config.display_name()
                ),
                "Every statement recorded for this connection is forgotten. Other \
                 connections keep theirs.",
            ),
            None => (
                "Clear all query history?".to_string(),
                "Every statement recorded for every connection is forgotten.",
            ),
        };
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let view = view.clone();
            alert
                .title(title.clone())
                .description(description)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Clear History")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _window, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.clear(scope, cx));
                    }
                    true
                })
        });
    }

    pub(crate) fn clear(&mut self, scope: Option<Uuid>, cx: &mut Context<Self>) {
        history::clear(scope);
        // The removal is queued behind any write still landing, so show the
        // empty list now rather than read back a file about to go.
        self.entries.clear();
        self.apply_filter(cx);
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn grid_for_test(&self) -> Entity<DataGrid> {
        self.grid.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_all_connections_for_test(&mut self, all: bool, cx: &mut Context<Self>) {
        self.all_connections = all;
        self.refresh(cx);
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.entries.len();
        let has_focus = self.focused_sql(cx).is_some();
        h_flex()
            .id("history-toolbar")
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                Input::new(&self.filter_input)
                    .id("history-filter")
                    .small()
                    .cleanable(true),
            )
            .child(
                Checkbox::new("history-all-connections")
                    .label("All connections")
                    .checked(self.all_connections)
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_all_connections(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(status_text(
                        count,
                        self.loading,
                        Settings::global(cx).query_history,
                    )),
            )
            .child(
                Button::new("history-open")
                    .ghost()
                    .xsmall()
                    .disabled(!has_focus)
                    .label("Open in Query Tab")
                    .tooltip("Open the selected statement in a new query tab")
                    .on_click(cx.listener(|this, _, _window, cx| this.open_focused(cx))),
            )
            .child(
                Button::new("history-refresh")
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .tooltip("Read the query history again")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
            .child(
                Button::new("history-clear")
                    .ghost()
                    .xsmall()
                    .disabled(count == 0)
                    .label("Clear…")
                    .tooltip(if self.all_connections {
                        "Forget every connection's query history"
                    } else {
                        "Forget this connection's query history"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.confirm_clear(window, cx))),
            )
    }
}

impl Focusable for HistoryView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.grid.read(cx).focus_handle(cx)
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("history-view")
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
    }
}

/// The count, and a word on where new statements go when that is not the
/// usual place, so an empty list under "Off" does not read as a fault.
fn status_text(count: usize, loading: bool, keep: Keep) -> String {
    if loading {
        return "Loading…".to_string();
    }
    let noun = if count == 1 {
        "statement"
    } else {
        "statements"
    };
    match keep {
        Keep::Disk => format!("{count} {noun}"),
        Keep::Memory => format!("{count} {noun} · kept until the app quits"),
        Keep::Off => format!("{count} {noun} · recording is off in Settings"),
    }
}

/// Where the SQL sits in a row: after the connection column when there is
/// one.
fn sql_column(all_connections: bool) -> usize {
    if all_connections { 3 } else { 2 }
}

/// Lay the entries out as a read-only result, newest first as given.
/// `names` adds a Connection column, for the merged view.
fn as_result(entries: &[&HistoryEntry], names: Option<&HashMap<Uuid, String>>) -> QueryResult {
    let mut columns = vec!["Time".to_string()];
    if names.is_some() {
        columns.push("Connection".to_string());
    }
    columns.extend(["Database", "SQL", "Result", "Ms"].map(str::to_string));
    let column_types = vec!["TEXT".to_string(); columns.len()];

    let rows: Vec<Vec<Cell>> = entries
        .iter()
        .map(|entry| {
            let mut row = vec![Some(format_time(entry.at))];
            if let Some(names) = names {
                row.push(Some(
                    names
                        .get(&entry.connection)
                        .cloned()
                        .unwrap_or_else(|| "Deleted or unsaved connection".to_string()),
                ));
            }
            row.extend([
                Some(entry.database.clone()),
                Some(entry.sql.clone()),
                Some(crate::ui::console::outcome_label(&entry.outcome)),
                Some(entry.elapsed_ms.to_string()),
            ]);
            row
        })
        .collect();

    QueryResult {
        columns,
        column_types,
        rows,
        ..QueryResult::default()
    }
}

fn format_time(at: DateTime<chrono::Utc>) -> String {
    let local: DateTime<Local> = at.into();
    local.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QueryOutcome;
    use std::time::Duration;

    fn entry(connection: Uuid, sql: &str, outcome: QueryOutcome) -> HistoryEntry {
        HistoryEntry::new(connection, "app", sql, outcome, Duration::from_millis(4))
    }

    #[test]
    fn the_merged_view_names_each_connection() {
        let (known, gone) = (Uuid::new_v4(), Uuid::new_v4());
        let names = HashMap::from([(known, "Production".to_string())]);
        let entries = [
            entry(known, "select 1", QueryOutcome::Rows(1)),
            entry(
                gone,
                "select 2",
                QueryOutcome::Error("no such table".into()),
            ),
        ];
        let refs: Vec<&HistoryEntry> = entries.iter().collect();

        let result = as_result(&refs, Some(&names));
        assert_eq!(result.columns[1], "Connection");
        assert_eq!(result.rows[0][1].as_deref(), Some("Production"));
        assert_eq!(
            result.rows[1][1].as_deref(),
            Some("Deleted or unsaved connection")
        );
        assert_eq!(
            result.rows[1][sql_column(true)].as_deref(),
            Some("select 2")
        );
        assert_eq!(
            result.rows[1][4].as_deref(),
            Some("Error: no such table"),
            "a failure says so in words"
        );
    }

    #[test]
    fn one_connection_has_no_connection_column() {
        let entries = [entry(Uuid::new_v4(), "select 1", QueryOutcome::Rows(2))];
        let refs: Vec<&HistoryEntry> = entries.iter().collect();
        let result = as_result(&refs, None);
        assert_eq!(result.columns[sql_column(false)], "SQL");
        assert!(!result.columns.iter().any(|column| column == "Connection"));
    }

    #[test]
    fn the_status_says_when_recording_is_not_on_disk() {
        assert_eq!(status_text(1, false, Keep::Disk), "1 statement");
        assert!(status_text(0, false, Keep::Off).contains("recording is off"));
        assert!(status_text(2, false, Keep::Memory).contains("until the app quits"));
    }
}
