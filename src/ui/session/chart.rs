//! Charting one query result: the series its rows make, and the pane that
//! draws them.
//!
//! A chart is a view of a result, not a stored thing: the tab keeps which
//! columns it charts and which kind, and the series are rebuilt from the
//! result whenever the pane draws. A column charts when at least one of its
//! values reads as a finite number. A row is plotted only when every picked
//! y column reads on it — the series share one x domain, so a row one series
//! is missing would shift every later point of that series — and rows that
//! do not read are skipped and counted, never plotted as zero: a gap is
//! honest, a zero is a lie. Lines and areas chart any number of series;
//! bars chart one, so picking a second y column turns a bar chart into
//! lines.

use gpui_kit::SharedString;
use gpui_kit::component::button::Button;
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Hsla, div};

use crate::db::query::QueryResult;

use super::panel::SessionPanel;
use super::tab::TabContent;

/// Kept rows are capped: a bar chart of ten thousand bars is mush, and a
/// line chart of them is a paint cost the grid does not pay.
const MAX_CHART_POINTS: usize = 5_000;

/// How many x labels a chart aims to show: with more points than this,
/// every n-th label is kept and the rest of the data still plots, the way
/// the toolkit's own Visitors chart thins a quarter of daily values.
const TARGET_X_LABELS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChartKind {
    #[default]
    Line,
    Bar,
    Area,
}

/// Which columns a tab charts. `x` is `None` for "not picked yet", and
/// resolves to the defaults below: the first numeric column for y, the
/// first other column for x — or the row number, when the numeric column
/// stands alone. `y` is the picked set in pick order; empty is "not picked
/// yet" and resolves to the first numeric column alone.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChartConfig {
    pub(crate) kind: ChartKind,
    pub(crate) x: Option<usize>,
    pub(crate) y: Vec<usize>,
}

/// One plotted row.
#[derive(Debug, Clone)]
pub(crate) struct ChartPoint {
    pub(crate) label: SharedString,
    pub(crate) value: f64,
}

/// One datum of a multi-series chart: the shared x label and one value per
/// series, in the order the series were picked. A row is a datum only when
/// every series has a value on it, so the series never drift apart.
#[derive(Debug, Clone)]
pub(crate) struct ChartRow {
    pub(crate) label: SharedString,
    pub(crate) values: Vec<f64>,
}

/// One picked y column: its name and its points, one per kept row.
#[derive(Debug, Default)]
pub(crate) struct ChartSeries {
    pub(crate) name: String,
    pub(crate) points: Vec<ChartPoint>,
}

/// The series a result makes, and how many of its rows could not be plotted.
#[derive(Debug, Default)]
pub(crate) struct ChartData {
    pub(crate) series: Vec<ChartSeries>,
    pub(crate) skipped: usize,
}

/// The chart the active tab would draw, for tests: its kind, its x column
/// (or the row number), its y columns, and how many points and skipped
/// rows.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChartSummary {
    pub(crate) kind: String,
    pub(crate) x: Option<String>,
    pub(crate) y: Vec<String>,
    pub(crate) points: usize,
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

/// The series one result makes for an x column and a set of y columns.
/// Every series gets one point per kept row, so the series share their x
/// domain; a row where any y column does not read is skipped for all of
/// them and counted once.
pub(crate) fn chart_series(result: &QueryResult, x: Option<usize>, ys: &[usize]) -> ChartData {
    let mut series: Vec<ChartSeries> = ys
        .iter()
        .map(|&column| ChartSeries {
            name: result.columns.get(column).cloned().unwrap_or_default(),
            points: Vec::new(),
        })
        .collect();
    let mut skipped = 0;
    for (index, row) in result.rows.iter().enumerate() {
        let values: Option<Vec<f64>> = ys
            .iter()
            .map(|&column| {
                row.get(column)
                    .and_then(|cell| cell.as_deref())
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .filter(|value| value.is_finite())
            })
            .collect();
        match values {
            Some(values) => {
                let label: SharedString = match x {
                    Some(column) => row
                        .get(column)
                        .and_then(|cell| cell.clone())
                        .unwrap_or_default()
                        .into(),
                    // A lone numeric column charts against its own order.
                    None => (index + 1).to_string().into(),
                };
                for (series, value) in series.iter_mut().zip(values) {
                    series.points.push(ChartPoint {
                        label: label.clone(),
                        value,
                    });
                }
            }
            None => skipped += 1,
        }
    }
    ChartData { series, skipped }
}

/// The rows behind a multi-series chart: one datum per kept row, with one
/// value per series. The chart element reads every series off the same
/// datums, which is what keeps the series aligned.
pub(crate) fn chart_rows(data: &ChartData) -> Vec<ChartRow> {
    let count = data
        .series
        .iter()
        .map(|series| series.points.len())
        .min()
        .unwrap_or(0);
    (0..count)
        .map(|index| ChartRow {
            label: data.series[0].points[index].label.clone(),
            values: data
                .series
                .iter()
                .map(|series| series.points[index].value)
                .collect(),
        })
        .collect()
}

/// Toggle a column in a picked y set, keeping pick order. Unpicking the
/// last one is a no-op, so the chart never falls back to a default behind
/// the picker's back.
pub(crate) fn toggle_series(ys: &mut Vec<usize>, column: usize) {
    if let Some(position) = ys.iter().position(|&picked| picked == column) {
        if ys.len() > 1 {
            ys.remove(position);
        }
    } else {
        ys.push(column);
    }
}

/// The x-label stride for a chart of `points` points: every n-th label is
/// kept, the full data still plots. One label per point up to the target,
/// then the stride grows so about that many labels survive.
pub(crate) fn tick_margin_for(points: usize) -> usize {
    points.div_ceil(TARGET_X_LABELS).max(1)
}

/// The color of a series: the theme's chart colors in order, cycling when
/// there are more series than colors.
pub(crate) fn series_color(index: usize, palette: &[Hsla]) -> Hsla {
    palette
        .get(index % palette.len().max(1))
        .copied()
        .unwrap_or_default()
}

/// The chart a result actually draws with: the tab's picks where they
/// still fit, defaults where they do not — and the picked y columns in
/// series order, so the bar can check them.
pub(crate) fn resolve(
    result: &QueryResult,
    config: &ChartConfig,
) -> Option<(ChartKind, Option<usize>, Vec<usize>, ChartData)> {
    let (default_x, default_y) = default_picks(result)?;
    let numeric = numeric_columns(result);
    let mut ys: Vec<usize> = Vec::new();
    for &column in &config.y {
        if column < result.columns.len() && numeric.contains(&column) && !ys.contains(&column) {
            ys.push(column);
        }
    }
    if ys.is_empty() {
        ys.push(default_y);
    }
    let x = config.x.filter(|&x| x < result.columns.len()).or(default_x);
    Some((config.kind, x, ys.clone(), chart_series(result, x, &ys)))
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

    /// The chart kind a tab draws with. Bars chart one column, so asking
    /// for bars while several series are picked leaves the kind alone —
    /// the switch never claims bars while lines are drawn.
    pub(crate) fn set_chart_kind(&mut self, kind: ChartKind, cx: &mut Context<Self>) {
        if let TabContent::Query {
            results,
            result,
            chart,
            ..
        } = &mut self.content
        {
            let multi = results
                .get(*result)
                .and_then(|shown| resolve(shown, chart))
                .is_some_and(|(_, _, _, data)| data.series.len() > 1);
            if !(kind == ChartKind::Bar && multi) {
                chart.kind = kind;
            }
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

    /// Toggle a column in the tab's y set. The toggle runs against the
    /// set the chart actually draws — the stored picks where they fit,
    /// defaults where they do not — so toggling a second column adds to
    /// the default series rather than replacing it. Bars chart one column,
    /// so picking a second one while bars are showing turns the chart into
    /// lines; an area chart keeps its kind.
    pub(crate) fn toggle_chart_y(&mut self, column: usize, cx: &mut Context<Self>) {
        if let TabContent::Query {
            results,
            result,
            chart,
            ..
        } = &mut self.content
        {
            if let Some((_, _, resolved, _)) =
                results.get(*result).and_then(|shown| resolve(shown, chart))
            {
                chart.y = resolved;
            }
            toggle_series(&mut chart.y, column);
            if chart.y.len() > 1 && chart.kind == ChartKind::Bar {
                chart.kind = ChartKind::Line;
            }
            cx.notify();
        }
    }

    /// The chart the active tab would draw, for tests: its kind, its x
    /// column (or the row number), its y columns, and how many points and
    /// skipped rows — or `None` when the shown result has nothing to chart.
    #[cfg(test)]
    pub(crate) fn chart_summary_for_test(&self) -> Option<ChartSummary> {
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
        let (kind, x, _ys, data) = resolve(shown, chart)?;
        let kind = match kind {
            ChartKind::Line => "Line",
            ChartKind::Bar => "Bar",
            ChartKind::Area => "Area",
        };
        let points = data
            .series
            .first()
            .map(|series| series.points.len())
            .unwrap_or(0);
        Some(ChartSummary {
            kind: kind.to_string(),
            x: x.and_then(|x| shown.columns.get(x).cloned()),
            y: data
                .series
                .iter()
                .map(|series| series.name.clone())
                .collect(),
            points,
            skipped: data.skipped,
        })
    }

    /// The chart pane for a result: a bar with the kind switch and the
    /// column picks, a legend when several series are drawn, and the chart
    /// itself.
    pub(crate) fn render_chart(&self, result: &QueryResult, cx: &mut Context<Self>) -> AnyElement {
        let TabContent::Query { chart, .. } = &self.content else {
            return div().into_any_element();
        };
        let Some((kind, x, ys, mut data)) = resolve(result, chart) else {
            return div()
                .flex_1()
                .p_4()
                .child("Nothing in this result charts: it needs a column with numbers.")
                .into_any_element();
        };
        let kept = data
            .series
            .first()
            .map(|series| series.points.len())
            .unwrap_or(0);
        let total = kept + data.skipped;
        let truncated = kept > MAX_CHART_POINTS;
        if truncated {
            for series in &mut data.series {
                series.points.truncate(MAX_CHART_POINTS);
            }
        }
        let plotted = kept.min(MAX_CHART_POINTS);
        let margin = tick_margin_for(plotted);
        let palette = [
            cx.theme().chart_1,
            cx.theme().chart_2,
            cx.theme().chart_3,
            cx.theme().chart_4,
            cx.theme().chart_5,
        ];
        let mut note = String::new();
        if data.skipped > 0 {
            note.push_str(&format!(
                "{} rows skipped (a value missing or not numeric)",
                data.skipped
            ));
        }
        if truncated {
            if !note.is_empty() {
                note.push_str(" · ");
            }
            note.push_str(&format!("showing first {MAX_CHART_POINTS} of {total} rows"));
        }
        let multi = data.series.len() > 1;

        v_flex()
            .size_full()
            .child(self.render_chart_bar(result, kind, x, &ys, &note, cx))
            .when(multi, |this| {
                this.child(self.render_chart_legend(&data, &palette))
            })
            .child(div().flex_1().min_h_0().p_2().child(match (kind, multi) {
                (ChartKind::Bar, false) => {
                    let points = data
                        .series
                        .first()
                        .map(|series| series.points.clone())
                        .unwrap_or_default();
                    BarChart::new(points)
                        .id("result-chart")
                        .band(|point: &ChartPoint| point.label.clone())
                        .value(|point: &ChartPoint| point.value)
                        .tick_margin(margin)
                        .into_any_element()
                }
                (ChartKind::Line, false) => {
                    let points = data
                        .series
                        .first()
                        .map(|series| series.points.clone())
                        .unwrap_or_default();
                    LineChart::new(points)
                        .id("result-chart")
                        .x(|point: &ChartPoint| point.label.clone())
                        .y(|point: &ChartPoint| point.value)
                        .tick_margin(margin)
                        .into_any_element()
                }
                // Areas, and lines of several series, share the
                // toolkit's multi-series element: AreaChart holds every
                // series off one datum list, with a tooltip row per
                // series. Lines are its strokes with the fill turned
                // transparent; areas keep the translucent fill.
                _ => {
                    let fill_opacity = match kind {
                        ChartKind::Area => 0.4,
                        _ => 0.,
                    };
                    let rows = chart_rows(&data);
                    let mut element = AreaChart::new(rows)
                        .id("result-chart")
                        .x(|row: &ChartRow| row.label.clone())
                        .tick_margin(margin);
                    for (index, series) in data.series.iter().enumerate() {
                        let color = series_color(index, &palette);
                        element = element
                            .y(move |row: &ChartRow| row.values[index])
                            .stroke(color)
                            .fill(color.opacity(fill_opacity))
                            .name(series.name.clone());
                    }
                    element.into_any_element()
                }
            }))
            .into_any_element()
    }

    /// The legend for a multi-series chart: one swatch and name per series,
    /// in the series' colors.
    fn render_chart_legend(&self, data: &ChartData, palette: &[Hsla]) -> impl IntoElement {
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_3()
            .children(data.series.iter().enumerate().map(|(index, series)| {
                let color = series_color(index, palette);
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(div().size_2().bg(color))
                    .child(div().text_xs().child(series.name.clone()))
            }))
    }

    /// The bar above a chart: the kind switch, the column picks, and the
    /// note about rows that could not be plotted.
    fn render_chart_bar(
        &self,
        result: &QueryResult,
        kind: ChartKind,
        x: Option<usize>,
        ys: &[usize],
        note: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let x_label = match x {
            Some(column) => result.columns.get(column).cloned().unwrap_or_default(),
            None => "Row".to_string(),
        };
        let y_names: Vec<String> = ys
            .iter()
            .map(|&column| result.columns.get(column).cloned().unwrap_or_default())
            .collect();
        let y_label = match y_names.as_slice() {
            [] => String::new(),
            [name] => name.clone(),
            [first, rest @ ..] => format!("{first} +{} more", rest.len()),
        };
        let columns = result.columns.clone();
        let x_columns = columns.clone();
        let numeric = numeric_columns(result);
        let picked = ys.to_vec();
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
                        let kind = match *index {
                            1 => ChartKind::Bar,
                            2 => ChartKind::Area,
                            _ => ChartKind::Line,
                        };
                        this.set_chart_kind(kind, cx);
                    }))
                    .child(Tab::new().aria_label("Line").label("Line"))
                    .child(Tab::new().aria_label("Bar").label("Bar"))
                    .child(Tab::new().aria_label("Area").label("Area")),
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
                                let checked = picked.contains(&column);
                                let name = columns.get(column).cloned().unwrap_or_default();
                                menu =
                                    menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                                        move |_, _window, cx| {
                                            if let Some(view) = weak.upgrade() {
                                                view.update(cx, |view, cx| {
                                                    view.toggle_chart_y(column, cx)
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
    fn series_share_one_x_domain_and_skip_incomplete_rows() {
        let result = result(
            &["day", "desktop", "mobile"],
            vec![
                vec![Some("mon"), Some("10"), Some("4")],
                vec![Some("tue"), Some("12"), None],
                vec![Some("wed"), Some("not a number"), Some("6")],
                vec![Some("thu"), Some(" 7.5 "), Some("8")],
            ],
        );
        let data = chart_series(&result, Some(0), &[1, 2]);
        // One row is missing mobile, one has a non-numeric desktop: both
        // rows are skipped for both series, and counted once each.
        assert_eq!(data.skipped, 2);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "desktop");
        assert_eq!(data.series[1].name, "mobile");
        for series in &data.series {
            assert_eq!(series.points.len(), 2);
            assert_eq!(series.points[0].label.as_ref(), "mon");
            assert_eq!(series.points[1].label.as_ref(), "thu");
        }
        assert_eq!(data.series[0].points[0].value, 10.0);
        assert_eq!(data.series[1].points[1].value, 8.0);
        // Whitespace around a number still reads.
        assert_eq!(data.series[0].points[1].value, 7.5);
    }

    #[test]
    fn a_lone_numeric_column_charts_against_its_order() {
        let result = result(&["score"], vec![vec![Some("3")], vec![Some("1")]]);
        let (x, y) = default_picks(&result).expect("a numeric column");
        assert_eq!(x, None);
        assert_eq!(y, 0);
        let data = chart_series(&result, x, &[y]);
        assert_eq!(data.series[0].points[0].label.as_ref(), "1");
        assert_eq!(data.series[0].points[1].label.as_ref(), "2");
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
            y: vec![9],
        };
        let (kind, x, ys, data) = resolve(&result, &config).expect("chartable");
        assert_eq!(kind, ChartKind::Bar);
        assert_eq!(x, Some(0));
        assert_eq!(ys, vec![1]);
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].points.len(), 1);
    }

    #[test]
    fn resolve_dedupes_and_drops_non_numeric_picks() {
        let result = result(&["name", "score"], vec![vec![Some("ada"), Some("10")]]);
        let config = ChartConfig {
            kind: ChartKind::Line,
            x: None,
            y: vec![0, 1, 1, 9],
        };
        let (_kind, _x, ys, data) = resolve(&result, &config).expect("chartable");
        assert_eq!(ys, vec![1]);
        assert_eq!(data.series.len(), 1);
    }

    #[test]
    fn toggling_a_series_keeps_order_and_never_empties_the_set() {
        let mut ys = vec![1];
        toggle_series(&mut ys, 1);
        assert_eq!(ys, vec![1]);
        toggle_series(&mut ys, 2);
        assert_eq!(ys, vec![1, 2]);
        toggle_series(&mut ys, 3);
        assert_eq!(ys, vec![1, 2, 3]);
        toggle_series(&mut ys, 2);
        assert_eq!(ys, vec![1, 3]);
    }

    #[test]
    fn x_labels_thin_as_points_grow_but_the_data_does_not() {
        assert_eq!(tick_margin_for(0), 1);
        assert_eq!(tick_margin_for(1), 1);
        assert_eq!(tick_margin_for(12), 1);
        assert_eq!(tick_margin_for(13), 2);
        assert_eq!(tick_margin_for(90), 8);
        assert_eq!(tick_margin_for(500), 42);
    }

    #[test]
    fn series_colors_cycle_the_palette() {
        use gpui_kit::Hsla;
        let palette = [Hsla::red(), Hsla::green(), Hsla::blue()];
        assert_eq!(series_color(0, &palette), Hsla::red());
        assert_eq!(series_color(2, &palette), Hsla::blue());
        assert_eq!(series_color(3, &palette), Hsla::red());
    }

    #[test]
    fn chart_rows_align_series_on_shared_datums() {
        let result = result(
            &["day", "a", "b"],
            vec![
                vec![Some("mon"), Some("1"), Some("2")],
                vec![Some("tue"), Some("3"), Some("4")],
            ],
        );
        let data = chart_series(&result, Some(0), &[1, 2]);
        let rows = chart_rows(&data);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label.as_ref(), "mon");
        assert_eq!(rows[0].values, vec![1.0, 2.0]);
        assert_eq!(rows[1].values, vec![3.0, 4.0]);
    }

    #[test]
    fn an_empty_result_has_nothing_to_chart() {
        let result = result(&["score"], vec![]);
        assert!(!chartable(&result));
        assert!(resolve(&result, &ChartConfig::default()).is_none());
    }
}
