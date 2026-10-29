//! One connection's basic facts, in one panel: the engine, where it lives,
//! the server version, the database it is bound to, and that database's
//! size where the engine can report it — the facts that otherwise sit
//! scattered across the server variables list and the sidebar. Uptime and
//! the connection count ride along only where [`Connection::connection_info`]
//! reads them cheaply; a fact an engine cannot report stays out of the
//! panel rather than being filled in with a slow query.
//!
//! A plain label/value panel rather than a [`DataGrid`]: these are facts
//! about one connection, not rows to search or pick. It refreshes on open
//! and on a database switch, like the other server tools.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, FocusHandle, Focusable, Window, div};

use crate::db::{Connection, ConnectionInfo, Engine, runtime};
use crate::ui::busy::busy_label;

pub struct ConnectionInfoView {
    connection: Arc<Connection>,
    focus: FocusHandle,
    info: Option<ConnectionInfo>,
    loading: bool,
    error: Option<String>,
}

impl ConnectionInfoView {
    pub fn new(connection: Arc<Connection>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            connection,
            focus: cx.focus_handle(),
            info: None,
            loading: false,
            error: None,
        };
        this.refresh(cx);
        this
    }

    /// Point this view at another connection — a database switch reopens the
    /// pool, so the facts it reads move with it.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.connection_info().await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(info)) => {
                        this.info = Some(info);
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("{error:#}"));
                        this.info = None;
                    }
                    Err(_) => {
                        this.error = Some("loading the connection information was cancelled".into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The facts the panel last loaded, for tests.
    #[cfg(test)]
    pub(crate) fn info_for_test(&self) -> Option<ConnectionInfo> {
        self.info.clone()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("connection-info-toolbar")
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
                    .when(self.loading, |this| {
                        this.child(busy_label("Loading…", cx.theme().muted_foreground))
                    }),
            )
            .child(
                Button::new("refresh-connection-info")
                    .ghost()
                    .xsmall()
                    .disabled(self.loading)
                    .label("Refresh")
                    .tooltip("Refresh the connection information")
                    .on_click(cx.listener(|this, _, _window, cx| this.refresh(cx))),
            )
    }

    fn render_rows(&self, info: &ConnectionInfo, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .px_2()
            .py_2()
            .gap_1()
            .children(rows(info).into_iter().map(|(label, value)| {
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .w_32()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(div().flex_1().text_sm().child(value))
            }))
    }
}

impl Focusable for ConnectionInfoView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConnectionInfoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("connection-info-view")
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
            .when_some(self.info.clone(), |this, info| {
                this.child(self.render_rows(&info, cx))
            })
    }
}

/// The panel's rows, in order: every fact the info carries, with the label
/// the panel shows it under. A fact that is `None` stays out — SQLite has
/// no host, user, database name, uptime, or connection count to show, and
/// an in-memory database has no file size.
fn rows(info: &ConnectionInfo) -> Vec<(&'static str, String)> {
    let mut rows = vec![
        ("Engine", info.engine.label().to_string()),
        (
            if info.engine == Engine::Sqlite {
                "File"
            } else {
                "Host"
            },
            info.server.clone(),
        ),
    ];
    if let Some(username) = &info.username {
        rows.push(("User", username.clone()));
    }
    rows.push(("Server version", info.version.clone()));
    if let Some(database) = &info.database {
        rows.push(("Database", database.clone()));
    }
    if let Some(size) = info.size_bytes {
        rows.push(("Database size", format_bytes(size)));
    }
    if let Some(uptime) = info.uptime_secs {
        rows.push(("Uptime", format_duration(uptime)));
    }
    if let Some(connections) = info.connections {
        rows.push(("Connections", connections.to_string()));
    }
    rows
}

/// A byte count in the unit it reads best in: whole bytes below a kilobyte,
/// one decimal above it.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A duration in seconds in its largest units: days, hours, minutes, and
/// whatever seconds are left — `90` reads as `1m 30s`, not `90s`.
fn format_duration(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = secs % 86_400 / 3_600;
    let minutes = secs % 3_600 / 60;
    let seconds = secs % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_info() -> ConnectionInfo {
        ConnectionInfo {
            engine: Engine::Sqlite,
            server: "/tmp/test.db".to_string(),
            username: None,
            version: "3.45.0".to_string(),
            database: None,
            size_bytes: Some(12_345),
            uptime_secs: None,
            connections: None,
        }
    }

    fn postgres_info() -> ConnectionInfo {
        ConnectionInfo {
            engine: Engine::Postgres,
            server: "db.example.com:5432".to_string(),
            username: Some("alan".to_string()),
            version: "PostgreSQL 16.4".to_string(),
            database: Some("app".to_string()),
            size_bytes: Some(12_345_678),
            uptime_secs: Some(90_061),
            connections: Some(7),
        }
    }

    #[test]
    fn sqlite_rows_show_the_file_and_skip_server_facts() {
        let rows = rows(&sqlite_info());
        let labels: Vec<&str> = rows.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            ["Engine", "File", "Server version", "Database size"],
            "SQLite's database is its file, and it has no host, user, uptime, or connections"
        );
        assert_eq!(rows[0].1, "SQLite");
        assert_eq!(rows[1].1, "/tmp/test.db");
    }

    #[test]
    fn server_rows_show_every_fact_in_order() {
        let rows = rows(&postgres_info());
        let labels: Vec<&str> = rows.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            [
                "Engine",
                "Host",
                "User",
                "Server version",
                "Database",
                "Database size",
                "Uptime",
                "Connections",
            ]
        );
        assert_eq!(rows[4].1, "app");
        assert_eq!(rows[7].1, "7");
    }

    #[test]
    fn a_missing_size_or_uptime_leaves_its_row_out() {
        let mut info = postgres_info();
        info.size_bytes = None;
        info.uptime_secs = None;
        let labels: Vec<&str> = rows(&info).iter().map(|(label, _)| *label).collect();
        assert!(!labels.contains(&"Database size"));
        assert!(!labels.contains(&"Uptime"));
        assert!(labels.contains(&"Connections"));
    }

    #[test]
    fn byte_counts_read_in_their_best_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_536), "1.5 KB");
        assert_eq!(format_bytes(12_345_678), "11.8 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn durations_read_in_their_largest_units() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3_600), "1h");
        assert_eq!(format_duration(90_061), "1d 1h 1m 1s");
    }
}
