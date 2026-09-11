//! Read-only result grid.
//!
//! TODO.md section 2. The table virtualizes rows and columns already; staged
//! editing, foreign key jumps, and specialized cell renderers come later.

use std::cmp::Ordering;
use std::rc::Rc;

use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, Entity, EventEmitter, Pixels, SharedString, Stateful, Window, div, px,
};

use crate::db::query::{Cell, QueryResult};
use crate::settings::{self, Settings};

/// Rows read when sizing a column. Values further down are rare enough that
/// paying for them on every result would cost more than the odd clipped cell.
const SAMPLE_ROWS: usize = 100;

/// Rough advance width of one character in the grid's font, which is monospaced
/// and rendered at `text_xs`. Measuring properly would mean shaping every
/// sampled value; this is within a few pixels and costs nothing.
const CHARACTER_WIDTH: f32 = 7.2;

/// Cell padding and borders that sit either side of the text.
const CELL_PADDING: f32 = 18.;

const MIN_COLUMN_WIDTH: f32 = 56.;
const MAX_COLUMN_WIDTH: f32 = 420.;

/// Width of the placeholder shown for SQL `NULL`.
const NULL_WIDTH: usize = 4;

/// Size every column from its header and the first [`SAMPLE_ROWS`] values.
fn measure_columns(result: &QueryResult) -> Vec<Pixels> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut characters = name.chars().count();

            for row in result.rows.iter().take(SAMPLE_ROWS) {
                let width = match row.get(index) {
                    Some(Some(value)) => value.chars().count(),
                    Some(None) => NULL_WIDTH,
                    None => 0,
                };
                characters = characters.max(width);
            }

            let width = characters as f32 * CHARACTER_WIDTH + CELL_PADDING;
            px(width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH))
        })
        .collect()
}

/// How a click on a column header is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sorting {
    /// Reorder the rows already in hand. Used for ad-hoc query results, where
    /// re-running the statement is not ours to do.
    InPlace,
    /// Report the sort to the owner so it can ask the server for ordered rows.
    /// Used by the table view, which pages through a table.
    Delegated,
}

/// Hands a delegated sort back to the grid's owner.
type SortReporter = Rc<dyn Fn(String, ColumnSort, &mut App)>;

/// Emitted when the user clicks a column header on a [`Sorting::Delegated`]
/// grid.
pub struct SortRequested {
    pub column: String,
    pub sort: ColumnSort,
}

struct ResultDelegate {
    result: QueryResult,
    /// Row indices in display order. Sorting reorders this rather than the
    /// rows themselves, so the order the rows arrived in is never lost.
    order: Vec<usize>,
    widths: Vec<Pixels>,
    /// Row under the selected cell, so the whole row can be highlighted while
    /// the selection itself stays on one column.
    selected_row: Option<usize>,
    font: SharedString,
    sorting: Sorting,
    /// Column the rows are sorted by, for the header arrow. Held by name, so
    /// it survives a result whose columns moved.
    sorted_by: Option<(String, ColumnSort)>,
    report_sort: SortReporter,
}

/// Stands in for a cell a short row does not have.
static MISSING: Cell = None;

/// The cell at `row_ix`/`col_ix`, or `NULL` where the row is short.
fn cell_at(rows: &[Vec<Cell>], row_ix: usize, col_ix: usize) -> &Cell {
    rows.get(row_ix)
        .and_then(|row| row.get(col_ix))
        .unwrap_or(&MISSING)
}

/// Compare two cells: numbers numerically, everything else as text, NULLs last.
fn compare(left: &Cell, right: &Cell) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match (left.parse::<f64>(), right.parse::<f64>()) {
            (Ok(left), Ok(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
            _ => left.cmp(right),
        },
    }
}

impl TableDelegate for ResultDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.result.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.order.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let name = self.result.columns.get(col_ix).cloned().unwrap_or_default();
        let width = self
            .widths
            .get(col_ix)
            .copied()
            .unwrap_or(px(MIN_COLUMN_WIDTH));

        let sort = match &self.sorted_by {
            Some((sorted, sort)) if *sorted == name => *sort,
            _ => ColumnSort::Default,
        };

        Column::new(name.clone(), name)
            .width(width)
            .resizable(true)
            .sortable()
            .sort(sort)
    }

    /// Answer a header click.
    ///
    /// The table cycles a column through descending, ascending, and back to
    /// unsorted, and hands the state it settled on here.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(column) = self.result.columns.get(col_ix).cloned() else {
            return;
        };

        self.sorted_by = match sort {
            ColumnSort::Default => None,
            sort => Some((column.clone(), sort)),
        };

        match self.sorting {
            Sorting::InPlace => {
                self.reorder(col_ix, sort);
                cx.notify();
            }
            Sorting::Delegated => {
                let report = self.report_sort.clone();
                cx.defer(move |cx| report(column, sort, cx));
            }
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        div()
            .id(("row", row_ix))
            .when(self.selected_row == Some(row_ix), |this| {
                this.bg(cx.theme().tokens.table_active)
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = div().font_family(self.font.clone()).text_xs();

        match self.value(row_ix, col_ix) {
            Some(value) => cell.child(value.to_string()),
            None => cell.text_color(cx.theme().muted_foreground).child("NULL"),
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.value(row_ix, col_ix).cloned().unwrap_or_default()
    }
}

impl ResultDelegate {
    fn value(&self, row_ix: usize, col_ix: usize) -> Option<&String> {
        let row_ix = *self.order.get(row_ix)?;
        self.result.rows.get(row_ix)?.get(col_ix)?.as_ref()
    }

    /// Put the display order where `sort` asks for; `Default` restores the
    /// order the server sent.
    fn reorder(&mut self, col_ix: usize, sort: ColumnSort) {
        let mut order: Vec<usize> = (0..self.result.rows.len()).collect();

        if sort != ColumnSort::Default {
            let rows = &self.result.rows;
            order.sort_by(|&left, &right| {
                let ordering = compare(cell_at(rows, left, col_ix), cell_at(rows, right, col_ix));
                match sort {
                    ColumnSort::Descending => ordering.reverse(),
                    _ => ordering,
                }
            });
        }

        self.order = order;
    }
}

pub struct DataGrid {
    table: Entity<TableState<ResultDelegate>>,
    has_result: bool,
}

impl EventEmitter<SortRequested> for DataGrid {}

impl DataGrid {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_sorting(Sorting::InPlace, window, cx)
    }

    pub fn with_sorting(sorting: Sorting, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let font = settings::grid_font(cx);

        let grid = cx.entity().downgrade();
        let report_sort: SortReporter = Rc::new(move |column, sort, cx| {
            if let Some(grid) = grid.upgrade() {
                grid.update(cx, |_, cx| cx.emit(SortRequested { column, sort }));
            }
        });

        let table = cx.new(|cx| {
            TableState::new(
                ResultDelegate {
                    result: QueryResult::default(),
                    order: Vec::new(),
                    widths: Vec::new(),
                    selected_row: None,
                    font,
                    sorting,
                    sorted_by: None,
                    report_sort,
                },
                window,
                cx,
            )
            .cell_selectable(true)
            .row_selectable(true)
        });

        // The grid's font is a setting, so it can change under a grid that is
        // already on screen.
        cx.observe_global::<Settings>(|this, cx| {
            let font = settings::grid_font(cx);
            this.table.update(cx, |table, cx| {
                if table.delegate().font != font {
                    table.delegate_mut().font = font;
                    cx.notify();
                }
            });
        })
        .detach();

        // The table highlights the selected cell; the row it sits on is
        // highlighted from here so both are visible at once.
        cx.subscribe(&table, |_this, table, event: &TableEvent, cx| {
            let row = match event {
                TableEvent::SelectCell(row_ix, _) => Some(*row_ix),
                TableEvent::SelectRow(row_ix) => Some(*row_ix),
                _ => return,
            };

            table.update(cx, |table, cx| {
                if table.delegate().selected_row != row {
                    table.delegate_mut().selected_row = row;
                    cx.notify();
                }
            });
        })
        .detach();

        Self {
            table,
            has_result: false,
        }
    }

    pub fn set_result(&mut self, result: QueryResult, cx: &mut Context<Self>) {
        self.set_sorted_result(result, None, cx);
    }

    /// Show `result`, marking its header as sorted by `sort`.
    ///
    /// The table rebuilds its headers from the delegate whenever the rows
    /// change, so an owner that sorts on the server has to hand its sort back.
    /// Without it the header returns to unsorted and the next click on it
    /// starts the cycle over, which makes every click sort descending.
    pub fn set_sorted_result(
        &mut self,
        result: QueryResult,
        sort: Option<(String, ColumnSort)>,
        cx: &mut Context<Self>,
    ) {
        self.has_result = true;
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.widths = measure_columns(&result);
            delegate.order = (0..result.rows.len()).collect();
            delegate.result = result;
            delegate.selected_row = None;
            delegate.sorted_by = sort;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.has_result = false;
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.result = QueryResult::default();
            delegate.order = Vec::new();
            delegate.widths = Vec::new();
            delegate.selected_row = None;
            delegate.sorted_by = None;
            table.clear_selection(cx);
            table.refresh(cx);
        });
        cx.notify();
    }

    fn is_empty(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().result.columns.is_empty()
    }

    /// Select a cell the way clicking one does.
    #[cfg(test)]
    pub(crate) fn select_cell_for_test(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        cx: &mut Context<Self>,
    ) {
        self.table
            .update(cx, |table, cx| table.set_selected_cell(row_ix, col_ix, cx));
    }

    /// The row currently highlighted, and the cell the selection sits on.
    #[cfg(test)]
    pub(crate) fn selection_for_test(&self, cx: &App) -> (Option<usize>, Option<(usize, usize)>) {
        let table = self.table.read(cx);
        (table.delegate().selected_row, table.selected_cell())
    }

    /// Sort by a column the way clicking its header does.
    #[cfg(test)]
    pub(crate) fn sort_for_test(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().perform_sort(col_ix, sort, window, cx)
        });
    }

    /// The column the header is marked as sorted by.
    #[cfg(test)]
    pub(crate) fn sorted_for_test(&self, cx: &App) -> Option<(String, ColumnSort)> {
        self.table.read(cx).delegate().sorted_by.clone()
    }

    /// One column of every row, in display order.
    #[cfg(test)]
    pub(crate) fn column_values_for_test(&self, col_ix: usize, cx: &App) -> Vec<Option<String>> {
        let delegate = self.table.read(cx).delegate();
        (0..delegate.order.len())
            .map(|row_ix| delegate.value(row_ix, col_ix).cloned())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn column_widths_for_test(&self, cx: &App) -> Vec<Pixels> {
        self.table.read(cx).delegate().widths.clone()
    }
}

impl Render for DataGrid {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_empty(cx) {
            let message = if self.has_result {
                "Statement returned no columns"
            } else {
                "Run a query to see results"
            };

            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .into_any_element();
        }

        h_flex()
            .size_full()
            .child(
                DataTable::new(&self.table)
                    .xsmall()
                    .stripe(true)
                    .bordered(false),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(columns: &[&str], rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|name| name.to_string()).collect(),
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| cell.map(|value| value.to_string()))
                        .collect()
                })
                .collect(),
            ..QueryResult::default()
        }
    }

    #[test]
    fn narrow_columns_get_the_minimum_width() {
        let widths = measure_columns(&result(&["id"], vec![vec![Some("1")], vec![Some("2")]]));
        assert_eq!(widths, [px(MIN_COLUMN_WIDTH)]);
    }

    #[test]
    fn wide_values_are_capped() {
        let long = "x".repeat(500);
        let widths = measure_columns(&result(&["blob"], vec![vec![Some(long.as_str())]]));
        assert_eq!(widths, [px(MAX_COLUMN_WIDTH)]);
    }

    #[test]
    fn width_follows_the_longest_sampled_value() {
        let widths = measure_columns(&result(
            &["name"],
            vec![
                vec![Some("ada")],
                vec![Some("a rather longer value")],
                vec![None],
            ],
        ));

        let expected = "a rather longer value".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn the_header_widens_a_column_of_short_values() {
        let widths = measure_columns(&result(
            &["a_column_with_a_long_name"],
            vec![vec![Some("1")]],
        ));

        let expected = "a_column_with_a_long_name".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn only_the_first_rows_are_sampled() {
        let sampled = "a twenty char value.";
        let long = "x".repeat(300);
        let mut rows: Vec<Vec<Option<&str>>> = vec![vec![Some(sampled)]; SAMPLE_ROWS];
        rows.push(vec![Some(long.as_str())]);

        let widths = measure_columns(&result(&["value"], rows));
        assert_eq!(
            widths,
            [px(sampled.len() as f32 * CHARACTER_WIDTH + CELL_PADDING)],
            "a long value past the sample should not widen the column"
        );
    }
}
