//! Writing the table, or the rows picked out of it, to a file.

use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};

use std::path::Path;

use crate::db::export::{self, Format};
use crate::db::query::QueryResult;
use crate::db::{RowKey, runtime};
use crate::ui::data_grid::Scope;
use crate::ui::sql_file;

use super::TableView;
use super::sql;

impl TableView {
    /// Export the whole table, in `format`.
    ///
    /// The filters and the sort are honoured; the page limit is not, since an
    /// export is every row the view is showing, not just the page in hand.
    pub(crate) fn export(&mut self, format: Format, cx: &mut Context<Self>) {
        // The statement is built now — that costs nothing — but the table is
        // only read once a file has been chosen, so a cancelled dialog does not
        // cost a full-table read.
        let (sql, params) = self.select_sql(false, cx);
        let table = self.object.name.clone();
        let connection = self.connection.clone();
        // A table addressed by its engine row id has that id asked for by name
        // (`select rowid, *`), so it has to come back out; a table with a
        // primary key selects only its own columns and needs nothing dropped.
        let by_row_id = matches!(self.row_key, Some(RowKey::RowId(_)));

        let query = async move {
            match runtime::spawn(async move { connection.run_query_with(&sql, params).await }).await
            {
                Ok(Ok(mut result)) => {
                    if by_row_id {
                        sql::take_row_id(&mut result);
                    }
                    Ok(result)
                }
                Ok(Err(error)) => Err(format!("{error:#}")),
                Err(_) => Err("exporting the table was cancelled".to_string()),
            }
        };

        self.export_result(format, table, query, cx);
    }

    /// Export the rows picked out in the grid, in `format`.
    ///
    /// Cells come through the grid, so a staged edit is exported as it stands.
    pub(crate) fn export_picked(&mut self, format: Format, cx: &mut Context<Self>) {
        let snapshot = self.grid.read(cx).snapshot(Scope::Picked, cx);
        if snapshot.rows.is_empty() {
            return;
        }

        let table = self.object.name.clone();
        let result = QueryResult {
            columns: snapshot.columns,
            column_types: snapshot.types,
            rows: snapshot.rows,
            ..QueryResult::default()
        };
        let query = async move { Ok(result) };
        self.export_result(format, table, query, cx);
    }

    /// Ask where to put an export, then run `query`, lay the rows out, and
    /// write them there.
    fn export_result(
        &mut self,
        format: Format,
        table: String,
        query: impl Future<Output = Result<QueryResult, String>> + 'static,
        cx: &mut Context<Self>,
    ) {
        let engine = self.connection.config.engine;
        let prompt = sql_file::prompt_for_save(None, &table, format.extension(), cx);

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

            let result = match query.await {
                Ok(result) => result,
                Err(message) => {
                    this.update_in(cx, |this, window, cx| this.fail_export(message, window, cx))
                        .ok();
                    return;
                }
            };

            let rendered = export::render(
                format,
                engine,
                &table,
                &result.columns,
                &result.column_types,
                &result.rows,
            );
            let rows = result.row_count();
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
