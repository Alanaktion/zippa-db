//! Charting one query result: the points its rows make, and the pane that
//! draws them.
//!
//! A chart is a view of a result, not a stored thing: the tab keeps which
//! columns it charts and which kind, and the points are rebuilt from the
//! result whenever the pane draws. A column charts when at least one of its
//! values reads as a finite number; rows whose value does not read are
//! skipped and counted, never plotted as zero — a gap is honest, a zero
//! is a lie.

use gpui_kit::SharedString;
use gpui_kit::component::button::Button;
use gpui_kit::component::chart::{BarChart, LineChart};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, div};

use crate::db::query::QueryResult;

use super::panel::SessionPanel;
use super::tab::TabContent;

/// Points are capped: a bar chart of ten thousand bars is mush, and a line
/// chart of them is a paint cost the grid does not pay.
const MAX_CHART_POINTS: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChartKind {
    #[default]
    Line,
    Bar,
}

/// Which columns a tab charts. `None` is "not picked yet", and resolves to
/// the defaults below: the first numeric column for y, the first other
/// column for x — or the row number, when the numeric column stands alone.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChartConfig {
    pub(crate) kind: ChartKind,
    pub(crate) x: Option<usize>,
    pub(crate) y: Option<usize>,
}

/// One plotted row.
#[derive(Debug, Clone)]
pub(crate) struct ChartPoint {
    pub(crate) label: SharedString,
    pub(crate) value: f64,
}

/// The points a result makes, and how many of its rows could not be plotted.
#[derive(Debug, Default)]
pub(crate) struct ChartData {
    pub(crate) points: Vec<ChartPoint>,
    pub(crate) skipped: usize,
}

/// The columns with at least one value that reads as a finite number.
/// "NaN" and "inf" parse as floats but are not plottable, so they do not
/// qualify a column.
pub(crate) fn numeric_columns(result: &QueryResult) -> Vec<usize> {
    (0..result.columns.len())
        .filter(|&column| {
            result
                .rows
                .iter()
                .any(|row| row.get(column).is_some_and(is_number))
        })
        .collect()
}

fn is_number(cell: &Option<String>) -> bool {
    cell.as_deref()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .is_some_and(|value| value.is_finite())
}

/// The picks a result gets when the tab has none, or its picks no longer
/// fit: the first numeric column for y, and the first other column — or
/// the row number — for x.
pub(crate) fn default_picks(result: &QueryResult) -> Option<(Option<usize>, usize)> {
    let y = numeric_columns(result).into_iter().next()?;
    let x = (0..result.columns.len()).find(|&column| column != y);
    Some((x, y))
}

/// Whether a result has anything to chart: rows, and a column that reads
/// as numbers.
pub(crate) fn chartable(result: &QueryResult) -> bool {
    !result.rows.is_empty() && !numeric_columns(result).is_empty()
}

/// The points one result makes for an x and a y column, with the rows
/// whose y value does not read counted rather than plotted.
pub(crate) fn chart_points(result: &QueryResult, x: Option<usize>, y: usize) -> ChartData {
    let mut points = Vec::new();
    let mut skipped = 0;
    for (index, row) in result.rows.iter().enumerate() {
        let value = row
            .get(y)
            .and_then(|cell| cell.as_deref())
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite());
        match value {
            Some(value) => {
                let label = match x {
                    Some(column) => row
                        .get(column)
                        .and_then(|cell| cell.clone())
                        .unwrap_or_default()
                        .into(),
                    // A lone numeric column charts against its own order.
                    None => (index + 1).to_string().into(),
                };
                points.push(ChartPoint { label, value });
            }
            None => skipped += 1,
        }
    }
    ChartData { points, skipped }
}

/// The chart a result actually draws with: the tab's picks where they
/// still fit, defaults where they do not.
pub(crate) fn resolve(
    result: &QueryResult,
    config: &ChartConfig,
) -> Option<(ChartKind, Option<usize>, usize, ChartData)> {
    let (default_x, default_y) = default_picks(result)?;
    let numeric = numeric_columns(result);
    let y = config
        .y
        .filter(|&y| y < result.columns.len() && numeric.contains(&y))
        .unwrap_or(default_y);
    let x = config.x.filter(|&x| x < result.columns.len()).or(default_x);
    Some((config.kind, x, y, chart_points(result, x, y)))
}

impl SessionPanel {
    /// Whether the pane is showing the chart rather than the result grid.
    pub(crate) fn showing_chart(&self) -> bool {
        matches!(
            &self.content,
            TabContent::Query {
                show_chart: true,
                ..
            }
        )
    }

    /// Show the chart pane or the result grid. The chart and the plan are
    /// two views of the same pane, so showing one hides the other.
    pub(crate) fn set_show_chart(&mut self, show: bool, cx: &mut Context<Self>) {
        if let TabContent::Query {
            show_chart,
            show_plan,
            ..
        } = &mut self.content
        {
            *show_chart = show;
            *show_plan = false;
            cx.notify();
        }
    }

    /// The chart kind a tab draws with.
    pub(crate) fn set_chart_kind(&mut self, kind: ChartKind, cx: &mut Context<Self>) {
        if let TabContent::Query { chart, .. } = &mut self.content {
            chart.kind = kind;
            cx.notify();
        }
    }
    /// The column a tab charts on x, or `None` for the row number.
    pub(crate) fn set_chart_x(&mut self, x: Option<usize>, cx: &mut Context<Self>) {
        if let TabContent::Query { chart, .. } = &mut self.content {
            chart.x = x;
            cx.notify();
        }
    }

    /// The column a tab charts on y.
    pub(crate) fn set_chart_y(&mut self, y: usize, cx: &mut Context<Self>) {
        if let TabContent::Query { chart, .. } = &mut self.content {
            chart.y = Some(y);
            cx.notify();
        }
    }

    /// The chart the active tab would draw, for tests: its kind, its x
    /// column (or the row number), its y column, and how many points and
    /// skipped rows — or `None` when the shown result has nothing to chart.
    #[cfg(test)]
    pub(crate) fn chart_summary_for_test(
        &self,
    ) -> Option<(String, Option<String>, String, usize, usize)> {
        let TabContent::Query {
            results,
            result,
            chart,
            ..
        } = &self.content
        else {
            return None;
        };
        let shown = results.get(*result)?;
        let (kind, x, y, data) = resolve(shown, chart)?;
        let kind = match kind {
            ChartKind::Line => "Line",
            ChartKind::Bar => "Bar",
        };
        Some((
            kind.to_string(),
            x.and_then(|x| shown.columns.get(x).cloned()),
            shown.columns.get(y).cloned().unwrap_or_default(),
            data.points.len(),
            data.skipped,
        ))
    }

    /// The chart pane for a result: a bar with the kind switch and the
    /// column picks, and the chart itself.
    pub(crate) fn render_chart(&self, result: &QueryResult, cx: &mut Context<Self>) -> AnyElement {
        let TabContent::Query { chart, .. } = &self.content else {
            return div().into_any_element();
        };
        let Some((kind, x, y, data)) = resolve(result, chart) else {
            return div()
                .flex_1()
                .p_4()
                .child("Nothing in this result charts: it needs a column with numbers.")
                .into_any_element();
        };
        let total = data.points.len() + data.skipped;
        let truncated = data.points.len() > MAX_CHART_POINTS;
        let points = if truncated {
            data.points[..MAX_CHART_POINTS].to_vec()
        } else {
            data.points
        };
        let mut note = String::new();
        if data.skipped > 0 {
            note.push_str(&format!("{} rows skipped (not numeric)", data.skipped));
        }
        if truncated {
            if !note.is_empty() {
                note.push_str(" · ");
            }
            note.push_str(&format!("showing first {MAX_CHART_POINTS} of {total} rows"));
        }

        v_flex()
            .size_full()
            .child(self.render_chart_bar(result, kind, x, y, &note, cx))
            .child(
                div().flex_1().min_h_0().p_2().child(match kind {
                    ChartKind::Line => LineChart::new(points)
                        .id("result-chart")
                        .x(|point: &ChartPoint| point.label.clone())
                        .y(|point: &ChartPoint| point.value)
                        .into_any_element(),
                    ChartKind::Bar => BarChart::new(points)
                        .id("result-chart")
                        .band(|point: &ChartPoint| point.label.clone())
                        .value(|point: &ChartPoint| point.value)
                        .into_any_element(),
                }),
            )
            .into_any_element()
    }

    /// The bar above a chart: the kind switch, the column picks, and the
    /// note about rows that could not be plotted.
    fn render_chart_bar(
        &self,
        result: &QueryResult,
        kind: ChartKind,
        x: Option<usize>,
        y: usize,
        note: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let x_label = match x {
            Some(column) => result.columns.get(column).cloned().unwrap_or_default(),
            None => "Row".to_string(),
        };
        let y_label = result.columns.get(y).cloned().unwrap_or_default();
        let columns = result.columns.clone();
        let x_columns = columns.clone();
        let numeric = numeric_columns(result);
        let note = note.to_string();

        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                TabBar::new("chart-kind")
                    .segmented()
                    .xsmall()
                    .selected_index(kind as usize)
                    .on_click(cx.listener(|this, index: &usize, _window, cx| {
                        let kind = if *index == 1 {
                            ChartKind::Bar
                        } else {
                            ChartKind::Line
                        };
                        this.set_chart_kind(kind, cx);
                    }))
                    .child(Tab::new().aria_label("Line").label("Line"))
                    .child(Tab::new().aria_label("Bar").label("Bar")),
            )
            .child(
                Button::new("chart-x")
                    .outline()
                    .xsmall()
                    .label(format!("X: {x_label}"))
                    .dropdown_caret(true)
                    .dropdown_menu({
                        let weak = weak.clone();
                        move |mut menu, _window, _cx| {
                            for (column, name) in x_columns.iter().enumerate() {
                                let weak = weak.clone();
                                let checked = x == Some(column);
                                let name = name.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone()).checked(checked).on_click(
                                        move |_, _window, cx| {
                                            if let Some(view) = weak.upgrade() {
                                                view.update(cx, |view, cx| {
                                                    view.set_chart_x(Some(column), cx)
                                                });
                                            }
                                        },
                                    ),
                                );
                            }
                            menu
                        }
                    }),
            )
            .child(
                Button::new("chart-y")
                    .outline()
                    .xsmall()
                    .label(format!("Y: {y_label}"))
                    .dropdown_caret(true)
                    .dropdown_menu({
                        let weak = weak.clone();
                        move |mut menu, _window, _cx| {
                            for column in numeric.iter() {
                                let column = *column;
                                let weak = weak.clone();
                                let checked = y == column;
                                let name = columns.get(column).cloned().unwrap_or_default();
                                menu =
                                    menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                                        move |_, _window, cx| {
                                            if let Some(view) = weak.upgrade() {
                                                view.update(cx, |view, cx| {
                                                    view.set_chart_y(column, cx)
                                                });
                                            }
                                        },
                                    ));
                            }
                            menu
                        }
                    }),
            )
            .when(!note.is_empty(), |this| {
                this.child(div().text_xs().child(note))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::QueryResult;

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
    fn only_columns_with_a_number_chart() {
        let result = result(
            &["name", "score", "note"],
            vec![
                vec![Some("ada"), Some("10"), Some("x")],
                vec![Some("grace"), Some("9"), Some("y")],
            ],
        );
        assert_eq!(numeric_columns(&result), vec![1]);
    }

    #[test]
    fn nan_and_infinity_do_not_qualify_a_column() {
        let result = result(
            &["value"],
            vec![vec![Some("NaN")], vec![Some("inf")], vec![None]],
        );
        assert!(numeric_columns(&result).is_empty());
        assert!(!chartable(&result));
    }

    #[test]
    fn points_skip_rows_that_do_not_read_and_count_them() {
        let result = result(
            &["name", "score"],
            vec![
                vec![Some("ada"), Some("10")],
                vec![Some("grace"), None],
                vec![Some("alan"), Some("not a number")],
                vec![Some("lin"), Some(" 7.5 ")],
            ],
        );
        let data = chart_points(&result, Some(0), 1);
        assert_eq!(data.skipped, 2);
        assert_eq!(data.points.len(), 2);
        assert_eq!(data.points[0].label.as_ref(), "ada");
        assert_eq!(data.points[0].value, 10.0);
        // Whitespace around a number still reads.
        assert_eq!(data.points[1].value, 7.5);
    }

    #[test]
    fn a_lone_numeric_column_charts_against_its_order() {
        let result = result(&["score"], vec![vec![Some("3")], vec![Some("1")]]);
        let (x, y) = default_picks(&result).expect("a numeric column");
        assert_eq!(x, None);
        assert_eq!(y, 0);
        let data = chart_points(&result, x, y);
        assert_eq!(data.points[0].label.as_ref(), "1");
        assert_eq!(data.points[1].label.as_ref(), "2");
    }

    #[test]
    fn defaults_pick_the_first_numeric_column_and_the_first_other() {
        let result = result(
            &["name", "id", "score"],
            vec![vec![Some("ada"), Some("1"), Some("10")]],
        );
        let (x, y) = default_picks(&result).expect("a numeric column");
        assert_eq!(x, Some(0));
        assert_eq!(y, 1);
    }

    #[test]
    fn resolve_falls_back_when_picks_no_longer_fit() {
        let result = result(&["name", "score"], vec![vec![Some("ada"), Some("10")]]);
        let config = ChartConfig {
            kind: ChartKind::Bar,
            x: Some(9),
            y: Some(9),
        };
        let (kind, x, y, data) = resolve(&result, &config).expect("chartable");
        assert_eq!(kind, ChartKind::Bar);
        assert_eq!(x, Some(0));
        assert_eq!(y, 1);
        assert_eq!(data.points.len(), 1);
    }

    #[test]
    fn an_empty_result_has_nothing_to_chart() {
        let result = result(&["score"], vec![]);
        assert!(!chartable(&result));
        assert!(resolve(&result, &ChartConfig::default()).is_none());
    }
}
