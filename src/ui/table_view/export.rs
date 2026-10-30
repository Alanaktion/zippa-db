//! Writing the table, or the rows picked out of it, to a file.

use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};

use std::path::Path;

use crate::db::export::{self, Format};
use crate::db::export_file::ExportRequest;
use crate::db::query::QueryResult;
use crate::db::{RowKey, runtime};
use crate::ui::data_grid::Scope;
use crate::ui::sql_file;

use super::TableView;

/// A whole-table export on its way to a file.
pub(super) struct Exporting {
    /// Rows written so far, as the last progress update said.
    pub(super) rows: usize,
    /// Stops the read; `None` under test, where the export has already run.
    abort: Option<tokio::task::AbortHandle>,
}

impl TableView {
    /// Export the whole table, in `format`.
    ///
    /// The filters and the sort are honoured; the page limit is not, since an
    /// export is every row the view is showing, not just the page in hand.
    ///
    /// The rows are written as they are read rather than gathered first
    /// (`db::export_file`), so a table larger than memory can be exported.
    /// The footer counts them as they go and offers to cancel.
    pub(crate) fn export(&mut self, format: Format, cx: &mut Context<Self>) {
        if self.exporting.is_some() {
            return;
        }
        // The statement is built now — that costs nothing — but the table is
        // only read once a file has been chosen, so a cancelled dialog does not
        // cost a full-table read.
        let (sql, params) = self.select_sql(false, cx);
        let connection = self.connection.clone();
        // A table addressed by its engine row id has that id asked for by name
        // (`select rowid, *`), so it has to come back out; a table with a
        // primary key selects only its own columns and needs nothing dropped.
        let drop_row_id = matches!(self.row_key, Some(RowKey::RowId(_)));
        let prompt = sql_file::prompt_for_save(None, &self.object.name, format.extension(), cx);
        let table = self.target();

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            let request = ExportRequest {
                sql,
                params,
                format,
                table,
                path: path.clone(),
                drop_row_id,
            };
            let (progress, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let task =
                runtime::spawn(async move { connection.export_to_file(request, progress).await });

            let started = this.update(cx, |this, cx| {
                this.exporting = Some(Exporting {
                    rows: 0,
                    abort: task.abort_handle(),
                });
                this.error = None;
                this.notice = None;
                cx.notify();

                // The count arrives on its own channel while the export runs,
                // and the channel closes with it.
                cx.spawn(async move |this, cx| {
                    while let Some(rows) = receiver.recv().await {
                        let updated = this.update(cx, |this, cx| {
                            if let Some(exporting) = &mut this.exporting {
                                exporting.rows = rows;
                                cx.notify();
                            }
                        });
                        if updated.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            });
            if started.is_err() {
                return;
            }

            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.exporting = None;
                match result {
                    Ok(Ok(exported)) => {
                        this.error = None;
                        this.notice = Some(export_notice(exported.rows, &path, exported.skipped));
                        cx.notify();
                    }
                    Ok(Err(error)) => this.fail_export(format!("{error:#}"), window, cx),
                    // The work was dropped: the half-written file went with it.
                    Err(_) => {
                        this.notice = Some("Export cancelled; nothing was written".to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Stop the export under way. The footer says so once the work has
    /// wound down.
    pub(super) fn cancel_export(&mut self, cx: &mut Context<Self>) {
        if let Some(abort) = self
            .exporting
            .as_mut()
            .and_then(|exporting| exporting.abort.take())
        {
            abort.abort();
            cx.notify();
        }
    }

    /// Whether an export is being written.
    pub(super) fn is_exporting(&self) -> bool {
        self.exporting.is_some()
    }

    /// Export the rows picked out in the grid, in `format`.
    ///
    /// Cells come through the grid, so a staged edit is exported as it stands.
    /// They are already held, so they are laid out whole rather than streamed.
    pub(crate) fn export_picked(&mut self, format: Format, cx: &mut Context<Self>) {
        let snapshot = self.grid.read(cx).snapshot(Scope::Picked, cx);
        if snapshot.rows.is_empty() {
            return;
        }

        let result = QueryResult {
            columns: snapshot.columns,
            column_types: snapshot.types,
            rows: snapshot.rows,
            ..QueryResult::default()
        };
        self.export_held(format, result, cx);
    }

    /// Ask where to put an export of rows already in hand, then lay them
    /// out and write them there.
    fn export_held(&mut self, format: Format, result: QueryResult, cx: &mut Context<Self>) {
        let engine = self.connection.config.engine;
        // The file is named after the table; an `INSERT` names it with its
        // schema, as every other statement this view writes does.
        let prompt = sql_file::prompt_for_save(None, &self.object.name, format.extension(), cx);
        let table = self.target();

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            // Laying the rows out is O(rows×cols) string work; it runs on the
            // background executor so a big export does not stall the UI.
            let rows = result.row_count();
            let rendered = cx
                .background_spawn(async move {
                    export::render(
                        format,
                        engine,
                        &table,
                        &result.columns,
                        &result.column_types,
                        &result.rows,
                    )
                })
                .await;
            let export::Rendered { text, skipped } = rendered;

            let written = cx
                .background_spawn(sql_file::write(path.clone(), text))
                .await;
            this.update_in(cx, |this, window, cx| {
                match written {
                    Ok(()) => {
                        this.error = None;
                        this.notice = Some(export_notice(rows, &path, skipped));
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        this.error = Some(message.clone());
                        crate::ui::notify_error(window, cx, format!("Error: {message}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put an export failure where the status line and the toasts can see it.
    fn fail_export(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.error = Some(message.clone());
        self.notice = None;
        crate::ui::notify_error(window, cx, format!("Error: {message}"));
        cx.notify();
    }
}

/// What the footer says after an export: where it went, and — when the driver
/// had only a description of some values — how many were written as `NULL`.
fn export_notice(rows: usize, path: &Path, skipped: usize) -> String {
    let unit = if rows == 1 { "row" } else { "rows" };
    let mut notice = format!("Exported {rows} {unit} to {}", path.display());
    match skipped {
        0 => {}
        1 => notice.push_str(" (1 value could not be read back and was written as NULL)"),
        skipped => notice.push_str(&format!(
            " ({skipped} values could not be read back and were written as NULL)"
        )),
    }
    notice
}
