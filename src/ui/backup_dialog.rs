//! The Back Up Database dialog.
//!
//! Opened from the session's sidebar, or the Database menu. It runs in four
//! states: loading the database and table lists, ready with the selection,
//! running with the database and bytes written so far, and done with the
//! output file or the failure.
//!
//! The selection is what the issue asks for: which databases on the server,
//! and — for the current database alone — which tables and views instead of
//! the whole database. Compression is one checkbox, gzip, on by default;
//! the dump itself is written by [`backup`](crate::db::backup), which
//! streams `pg_dump`/`mysqldump` into the file the user picks after pressing
//! Back up, so the extension follows the compression choice.
//!
//! The dialog is `gpui_kit`'s [`Dialog`](gpui_kit::component::dialog::Dialog),
//! opened by the session, and this view is its body. It emits
//! [`BackupEvent::Dismissed`] when the user is done with it. A backup reads
//! but never writes to the server, so a read-only connection may run one;
//! SQLite has no dump tool, so the dialog says so instead of offering one.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, EventEmitter, SharedString, Window, actions, div};

use crate::db::{
    BackupProgress, BackupRequest, BackupSummary, Connection, DatabaseObject, Engine, ObjectKind,
    TableSelection, backup, runtime,
};
use crate::ui::sql_file;

actions!(zippa_db, [CloseBackup]);

/// What loading the database and table lists returns: each list, or the
/// error reading it — plus the channel error when the load was dropped.
type LoadedMetadata = Result<
    (
        Result<Vec<String>, anyhow::Error>,
        Result<Vec<DatabaseObject>, anyhow::Error>,
    ),
    tokio::sync::oneshot::error::RecvError,
>;

/// What the dialog tells its owner.
pub enum BackupEvent {
    /// The user is done with the dialog; the session takes it off screen.
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Loading,
    Ready,
    Running,
    Done,
}

pub struct BackupView {
    connection: Arc<Connection>,
    state: State,
    databases: Vec<String>,
    selected_databases: HashSet<String>,
    /// Tables and views of the current database, for the optional
    /// table-scoped backup.
    objects: Vec<DatabaseObject>,
    selected_tables: Vec<DatabaseObject>,
    /// A list that could not be read is a warning, not a block: the current
    /// database is still offered, and a backup without a table list is
    /// still a whole-database backup.
    list_warning: Option<String>,
    compress: bool,
    output: Option<PathBuf>,
    progress: Option<BackupProgress>,
    result: Option<BackupSummary>,
    failure: Option<String>,
    abort: Option<tokio::task::AbortHandle>,
    /// The dialog body's own focus, so `escape` reaches `CloseBackup`.
    focus: gpui_kit::FocusHandle,
}

impl EventEmitter<BackupEvent> for BackupView {}

impl BackupView {
    pub fn new(connection: Arc<Connection>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = connection.database().to_string();
        let task = {
            let connection = connection.clone();
            runtime::spawn(
                async move { (connection.databases().await, connection.objects().await) },
            )
        };
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                this.loaded(loaded, cx);
            })
            .ok();
        })
        .detach();

        Self {
            connection,
            state: State::Loading,
            databases: Vec::new(),
            selected_databases: HashSet::from([current]),
            objects: Vec::new(),
            selected_tables: Vec::new(),
            list_warning: None,
            compress: true,
            output: None,
            progress: None,
            result: None,
            failure: None,
            abort: None,
            focus: cx.focus_handle(),
        }
    }

    /// Put the keyboard on the dialog body, where `escape` is handled.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    fn current_database(&self) -> String {
        self.connection.database().to_string()
    }

    /// The lists landed: databases first, tables second. A database list
    /// that failed still leaves the current database selectable; a table
    /// list that failed leaves whole-database backups, which need no list.
    fn loaded(&mut self, loaded: LoadedMetadata, cx: &mut Context<Self>) {
        let current = self.current_database();
        match loaded {
            Ok((databases, objects)) => {
                match databases {
                    Ok(mut databases) => {
                        if !databases.contains(&current) {
                            databases.insert(0, current.clone());
                        }
                        self.databases = databases;
                    }
                    Err(error) => {
                        self.databases = vec![current.clone()];
                        self.list_warning = Some(format!(
                            "Could not list the server's databases ({error:#}); only the \
                             current database is offered."
                        ));
                    }
                }
                match objects {
                    Ok(objects) => self.objects = objects,
                    Err(error) => {
                        self.list_warning = Some(format!(
                            "Could not list this database's tables ({error:#}); it can \
                             still be backed up whole."
                        ));
                    }
                }
            }
            Err(_) => {
                self.databases = vec![current.clone()];
                self.list_warning = Some(
                    "The database list was cancelled; only the current database is offered."
                        .to_string(),
                );
            }
        }
        self.selected_databases = HashSet::from([current]);
        self.state = State::Ready;
        cx.notify();
    }

    /// The reason a backup cannot start, if there is one.
    fn blocked_reason(&self) -> Option<String> {
        if self.connection.config.engine == Engine::Sqlite {
            return Some(
                "SQLite has no pg_dump/mysqldump equivalent, so whole-database backups are \
                 not available for SQLite connections."
                    .to_string(),
            );
        }
        if self.selected_databases.is_empty() {
            return Some("Select at least one database to back up.".to_string());
        }
        if !self.selected_tables.is_empty()
            && (self.selected_databases.len() != 1
                || !self.selected_databases.contains(&self.current_database()))
        {
            return Some(format!(
                "Tables belong to {}; back up that database alone with its tables, or clear \
                 the table selection to back up several databases whole.",
                self.current_database()
            ));
        }
        None
    }

    fn can_start(&self) -> bool {
        self.state == State::Ready && self.blocked_reason().is_none()
    }

    fn toggle_database(&mut self, database: String, selected: bool) {
        if selected {
            self.selected_databases.insert(database);
        } else {
            self.selected_databases.remove(&database);
        }
    }

    fn toggle_table(&mut self, object: DatabaseObject, selected: bool) {
        if selected {
            if !self
                .selected_tables
                .iter()
                .any(|item| item.label() == object.label())
            {
                self.selected_tables.push(object);
            }
        } else {
            self.selected_tables
                .retain(|item| item.label() != object.label());
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.state != State::Ready || !self.can_start() {
            return;
        }

        let current = self.current_database();
        let compress = self.compress;
        // The prompt suggests a name from its title and extension; the
        // suggested file name is the one source of truth for both, split
        // back apart here so the two cannot drift.
        let suggested = backup::suggested_file_name(&current, compress);
        let extension = if compress { "sql.gz" } else { "sql" };
        let title = suggested
            .strip_suffix(&format!(".{extension}"))
            .unwrap_or(&suggested);
        let prompt = sql_file::prompt_for_save(None, title, extension, cx);
        let connection = self.connection.clone();
        let databases: Vec<String> = self
            .databases
            .iter()
            .filter(|database| self.selected_databases.contains(*database))
            .cloned()
            .collect();
        let tables: Vec<TableSelection> = self
            .selected_tables
            .iter()
            .map(|object| TableSelection {
                database: current.clone(),
                schema: object.schema.clone(),
                name: object.name.clone(),
            })
            .collect();
        let (host, port) = connection.server_endpoint();

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => backup::normalize_output_path(&path, compress),
                Ok(None) => return,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.failure = Some(format!("{error:#}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let request = BackupRequest {
                engine: connection.config.engine,
                host,
                port,
                username: connection.config.username.clone(),
                password: connection.stored_password().map(str::to_string),
                ssl_mode: connection.config.ssl.mode,
                databases,
                tables,
                compress,
                output: path.clone(),
            };
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let task = runtime::spawn(async move { backup::run(request, sender).await });
            // The completion task removes a cancelled run's partial file,
            // so the path survives the dialog resetting itself first.
            let cleanup_path = path.clone();

            this.update(cx, |this, cx| {
                this.abort = task.abort_handle();
                this.state = State::Running;
                this.output = Some(path);
                this.progress = None;
                this.result = None;
                this.failure = None;
                cx.notify();
            })
            .ok();

            // Progress arrives on its own channel while the run is in flight.
            // This closure runs on the async app, whose `spawn` takes only
            // the app — the view is reached through the weak handle, cloned
            // once per task rather than moved into the first.
            let progress_view = this.clone();
            cx.spawn(async move |cx| {
                while let Some(update) = receiver.recv().await {
                    progress_view
                        .update(cx, |this, cx| {
                            this.progress = Some(update);
                            cx.notify();
                        })
                        .ok();
                }
            })
            .detach();

            cx.spawn(async move |cx| {
                let result = task.await;
                // The run task has settled by now — its file handle is
                // dropped — so removing the partial file here cannot race
                // the run the way removing it from `cancel` did. Only the
                // outer cancellation removes here; a failure the run
                // returned itself already removed its own file.
                if result.is_err() {
                    let _ = std::fs::remove_file(&cleanup_path);
                }
                this.update(cx, |this, cx| this.finish(result, cx)).ok();
            })
            .detach();
        })
        .detach();
    }

    fn finish(
        &mut self,
        result: Result<
            Result<BackupSummary, anyhow::Error>,
            tokio::sync::oneshot::error::RecvError,
        >,
        cx: &mut Context<Self>,
    ) {
        self.abort = None;
        self.state = State::Done;
        match result {
            Ok(Ok(summary)) => self.result = Some(summary),
            Ok(Err(error)) => self.failure = Some(format!("{error:#}")),
            // The sender is dropped when the run is given up on.
            Err(_) => self.failure = Some("Cancelled".to_string()),
        }
        cx.notify();
    }

    /// Stop the run in flight, the way `Cmd`/`Ctrl`+`.` does. The partial
    /// file is removed by the completion task once the run has settled,
    /// not here: removing it now would race the run dropping its file,
    /// and on Windows the removal would fail against the open handle.
    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
        if self.state == State::Running {
            self.state = State::Done;
            self.failure = Some("Cancelled".to_string());
            cx.notify();
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(BackupEvent::Dismissed);
    }

    /// `Escape`: close the dialog, or — while the backup runs — stop it
    /// the way Cancel does, so the run is not left going with nothing on
    /// screen to follow or stop it.
    fn on_close(&mut self, _: &CloseBackup, _window: &mut Window, cx: &mut Context<Self>) {
        if self.state == State::Running {
            self.cancel(cx);
            return;
        }
        self.close(cx);
    }

    fn back_up_again(&mut self, cx: &mut Context<Self>) {
        self.state = State::Ready;
        self.output = None;
        self.progress = None;
        self.result = None;
        self.failure = None;
        cx.notify();
    }

    fn render_loading(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div().text_sm().child("Loading databases and tables…")
    }

    fn render_database_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.current_database();
        let databases = self.databases.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("DATABASES"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("backup-databases-all")
                                    .ghost()
                                    .xsmall()
                                    .label("All")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_databases =
                                            this.databases.iter().cloned().collect();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("backup-databases-none")
                                    .ghost()
                                    .xsmall()
                                    .label("None")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_databases.clear();
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("backup-databases")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .p_2()
                    .child(v_flex().gap_1().children(databases.iter().enumerate().map(
                        |(ix, database)| {
                            let database = database.clone();
                            let checked = self.selected_databases.contains(&database);
                            let label = if database == current {
                                format!("{database} (current)")
                            } else {
                                database.clone()
                            };
                            let for_click = database.clone();
                            h_flex()
                                .gap_1()
                                .child(
                                    Checkbox::new(SharedString::from(format!(
                                        "backup-database-{ix}"
                                    )))
                                    .accessibility_label(format!("Back up database {database}"))
                                    .checked(checked)
                                    .on_click(cx.listener(move |this, selected: &bool, _, cx| {
                                        this.toggle_database(for_click.clone(), *selected);
                                        cx.notify();
                                    })),
                                )
                                .child(div().text_sm().truncate().child(label))
                                .into_any_element()
                        },
                    ))),
            )
    }

    fn render_table_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.current_database();
        let objects = self.objects.clone();
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("TABLES IN {current} — OPTIONAL")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("backup-tables-all")
                                    .ghost()
                                    .xsmall()
                                    .label("All")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_tables = this.objects.clone();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("backup-tables-none")
                                    .ghost()
                                    .xsmall()
                                    .label("None")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_tables.clear();
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Leave every table unchecked to back up the whole database. \
                         Picking tables backs up only those, from this database alone.",
                    ),
            )
            .child(
                v_flex()
                    .id("backup-tables")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .p_2()
                    .when(objects.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No tables or views in this database."),
                        )
                    })
                    .child(v_flex().gap_1().children(objects.iter().enumerate().map(
                        |(ix, object)| {
                            let object = object.clone();
                            let checked = self
                                .selected_tables
                                .iter()
                                .any(|item| item.label() == object.label());
                            let for_click = object.clone();
                            let is_view = object.kind == ObjectKind::View;
                            h_flex()
                                .gap_1()
                                .child(
                                    Checkbox::new(SharedString::from(format!("backup-table-{ix}")))
                                        .accessibility_label(format!(
                                            "Back up table {}",
                                            object.label()
                                        ))
                                        .checked(checked)
                                        .on_click(cx.listener(
                                            move |this, selected: &bool, _, cx| {
                                                this.toggle_table(for_click.clone(), *selected);
                                                cx.notify();
                                            },
                                        )),
                                )
                                .child(div().text_sm().truncate().child(object.label()))
                                .when(is_view, |this| {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("view"),
                                    )
                                })
                                .into_any_element()
                        },
                    ))),
            )
    }

    fn render_ready(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let engine = self.connection.config.engine;
        let blocked = self.blocked_reason();
        let can_start = self.can_start();
        let tool = backup::tool_name(engine).unwrap_or("pg_dump");

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(div().text_sm().child(format!(
                "Back up with {tool} into a file you choose. A backup reads \
                         the server; it changes nothing there."
            )))
            .when_some(self.list_warning.clone(), |this, warning| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(warning),
                )
            })
            .child(self.render_database_list(cx))
            .child(self.render_table_list(cx))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("backup-compress")
                            .accessibility_label("Compress the backup with gzip")
                            .checked(self.compress)
                            .on_click(cx.listener(|this, selected: &bool, _, cx| {
                                this.compress = *selected;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("Compress with gzip (.gz)")),
            )
            .when_some(blocked.clone(), |this, reason| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(reason))
            })
            .when_some(self.failure.clone(), |this, failure| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(failure))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("backup-cancel")
                            .ghost()
                            .small()
                            .label("Close")
                            .tooltip_with_action(
                                "Close the backup dialog",
                                &CloseBackup,
                                Some("BackupDialog"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("backup-start")
                            .primary()
                            .small()
                            .label("Back up…")
                            .disabled(!can_start)
                            .when_some(blocked, |button, reason| {
                                button.tooltip(SharedString::from(reason))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }

    fn render_running(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .gap_3()
            .child(div().text_sm().child(match &self.progress {
                Some(progress) => format!(
                    "Backing up {} · {} written",
                    progress.database,
                    bytes(progress.bytes_written)
                ),
                None => "Starting the backup…".to_string(),
            }))
            .when_some(self.output.clone(), |this, output| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(output.display().to_string()),
                )
            })
            .child(
                h_flex().gap_2().justify_end().child(
                    Button::new("backup-stop")
                        .outline()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                ),
            )
    }

    fn render_done(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(if self.failure.is_some() {
                        cx.theme().danger
                    } else {
                        cx.theme().success
                    })
                    .child(match (&self.failure, &self.result) {
                        (Some(failure), _) => failure.clone(),
                        (None, Some(summary)) => format!(
                            "Backed up {} to {} ({})",
                            summary.databases.join(", "),
                            summary.output.display(),
                            bytes(summary.bytes_written)
                        ),
                        (None, None) => "Done".to_string(),
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("backup-again")
                            .ghost()
                            .small()
                            .label("Back up again")
                            .on_click(cx.listener(|this, _, _, cx| this.back_up_again(cx))),
                    )
                    .child(
                        Button::new("backup-close")
                            .primary()
                            .small()
                            .label("Close")
                            .tooltip_with_action(
                                "Close the backup dialog",
                                &CloseBackup,
                                Some("BackupDialog"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
    }
}

impl Render for BackupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("backup-dialog")
            .test_support()
            .size_full()
            .gap_3()
            .track_focus(&self.focus)
            .key_context("BackupDialog")
            .on_action(cx.listener(Self::on_close))
            .child(match self.state {
                State::Loading => self.render_loading(cx).into_any_element(),
                State::Ready => self.render_ready(cx).into_any_element(),
                State::Running => self.render_running(cx).into_any_element(),
                State::Done => self.render_done(cx).into_any_element(),
            })
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
