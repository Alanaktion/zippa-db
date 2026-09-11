//! A table opened from the sidebar: just the grid, paged, with no editor.
//!
//! TODO.md section 2 ("Configurable row limit & offset pagination") and the
//! sorting half of "Multi-column sorting".

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::table::ColumnSort;
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Window, div, px};

use crate::db::{Connection, DatabaseObject, quote_identifier, runtime};
use crate::settings::{self, Settings};
use crate::ui::data_grid::{DataGrid, SortRequested, Sorting};

pub struct TableView {
    connection: Arc<Connection>,
    object: DatabaseObject,
    grid: Entity<DataGrid>,
    limit_input: Entity<InputState>,
    limit: usize,
    page: usize,
    /// Column and direction of the current `ORDER BY`.
    sort: Option<(String, ColumnSort)>,
    /// Rows the last page returned, used to decide whether there is a next one.
    loaded_rows: usize,
    loading: bool,
    error: Option<String>,
}

impl TableView {
    pub fn new(
        connection: Arc<Connection>,
        object: DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let grid = cx.new(|cx| DataGrid::with_sorting(Sorting::Delegated, window, cx));
        cx.subscribe_in(&grid, window, Self::on_sort_requested)
            .detach();

        // A table takes the page size the settings had when it was opened;
        // changing the setting later leaves open tables where they are.
        let limit = Settings::global(cx)
            .page_size
            .clamp(1, settings::MAX_PAGE_SIZE);

        let limit_input = cx.new(|cx| InputState::new(window, cx).default_value(limit.to_string()));
        cx.subscribe_in(&limit_input, window, Self::on_limit_event)
            .detach();

        let mut view = Self {
            connection,
            object,
            grid,
            limit_input,
            limit,
            page: 0,
            sort: None,
            loaded_rows: 0,
            loading: false,
            error: None,
        };
        view.reload(cx);
        view
    }

    pub fn object(&self) -> &DatabaseObject {
        &self.object
    }

    /// Point the view at a new connection, as when the database is switched.
    pub fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.page = 0;
        self.sort = None;
        self.reload(cx);
    }

    /// The statement this view runs for its current page and sort.
    pub fn query(&self) -> String {
        let engine = self.connection.config.engine;
        let target = match &self.object.schema {
            Some(schema) => format!(
                "{}.{}",
                quote_identifier(schema, engine),
                quote_identifier(&self.object.name, engine)
            ),
            None => quote_identifier(&self.object.name, engine),
        };

        let mut sql = format!("select * from {target}");
        if let Some((column, sort)) = &self.sort {
            let direction = match sort {
                ColumnSort::Descending => "desc",
                _ => "asc",
            };
            sql.push_str(&format!(
                " order by {} {direction}",
                quote_identifier(column, engine)
            ));
        }
        sql.push_str(&format!(
            " limit {} offset {}",
            self.limit,
            self.page * self.limit
        ));
        sql
    }

    /// Rebuild the paging SQL and re-run it. Also the table half of the
    /// session's refresh action.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();

        let sql = self.query();
        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.run_query(&sql).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(query_result)) => {
                        this.loaded_rows = query_result.row_count();
                        // The rows come back already ordered, so the grid is
                        // told what order they are in: it rebuilds its headers
                        // from scratch and would otherwise show the column as
                        // unsorted and restart its cycle on the next click.
                        let sort = this.sort.clone();
                        this.grid.update(cx, |grid, cx| {
                            grid.set_sorted_result(query_result, sort, cx)
                        });
                    }
                    Ok(Err(error)) => {
                        this.loaded_rows = 0;
                        this.error = Some(format!("{error:#}"));
                        this.grid.update(cx, |grid, cx| grid.clear(cx));
                    }
                    Err(_) => this.error = Some("loading the table was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_sort_requested(
        &mut self,
        _: &Entity<DataGrid>,
        event: &SortRequested,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort = match event.sort {
            ColumnSort::Default => None,
            sort => Some((event.column.clone(), sort)),
        };
        // Sorting reorders the whole table, so the old page number is
        // meaningless.
        self.page = 0;
        self.reload(cx);
    }

    fn on_limit_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Applied on Enter rather than per keystroke, so typing "1000" does not
        // fire queries for 1, 10, and 100 on the way.
        if !matches!(event, InputEvent::PressEnter { .. }) {
            return;
        }

        let Ok(limit) = input.read(cx).value().trim().parse::<usize>() else {
            return;
        };
        let limit = limit.clamp(1, settings::MAX_PAGE_SIZE);
        if limit == self.limit {
            return;
        }

        self.limit = limit;
        self.page = 0;
        self.reload(cx);
    }

    fn has_previous(&self) -> bool {
        self.page > 0
    }

    /// A full page suggests there is more; a short one means this is the end.
    fn has_next(&self) -> bool {
        self.loaded_rows >= self.limit
    }

    fn go(&mut self, page: usize, cx: &mut Context<Self>) {
        if page == self.page {
            return;
        }
        self.page = page;
        self.reload(cx);
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_row = self.page * self.limit;
        let range = if self.loaded_rows == 0 {
            "No rows".to_string()
        } else {
            format!("Rows {}–{}", first_row + 1, first_row + self.loaded_rows)
        };

        let message = match (&self.error, self.loading) {
            (Some(error), _) => (error.clone(), cx.theme().danger),
            (None, true) => ("Loading…".to_string(), cx.theme().muted_foreground),
            (None, false) => (range, cx.theme().muted_foreground),
        };

        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_1()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("previous-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .tooltip("Previous page")
                            .disabled(!self.has_previous() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.go(this.page.saturating_sub(1), cx)
                            })),
                    )
                    .child(
                        Button::new("next-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronRight)
                            .tooltip("Next page")
                            .disabled(!self.has_next() || self.loading)
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.go(this.page + 1, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(message.1)
                            .child(message.0.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Limit"),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .child(Input::new(&self.limit_input).id("limit").xsmall()),
                    ),
            )
    }
}

impl Render for TableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.grid.clone()))
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
impl TableView {
    pub(crate) fn page_for_test(&self) -> usize {
        self.page
    }

    pub(crate) fn limit_for_test(&self) -> usize {
        self.limit
    }

    pub(crate) fn go_for_test(&mut self, page: usize, cx: &mut Context<Self>) {
        self.go(page, cx);
    }

    pub(crate) fn sort_for_test(&mut self, column: &str, sort: ColumnSort, cx: &mut Context<Self>) {
        self.sort = match sort {
            ColumnSort::Default => None,
            sort => Some((column.to_string(), sort)),
        };
        self.page = 0;
        self.reload(cx);
    }

    pub(crate) fn grid_for_test(&self) -> Entity<DataGrid> {
        self.grid.clone()
    }

    pub(crate) fn set_loaded_rows_for_test(&mut self, rows: usize) {
        self.loaded_rows = rows;
    }

    pub(crate) fn loaded_rows_for_test(&self) -> usize {
        self.loaded_rows
    }

    pub(crate) fn can_page_for_test(&self) -> (bool, bool) {
        (self.has_previous(), self.has_next())
    }
}
