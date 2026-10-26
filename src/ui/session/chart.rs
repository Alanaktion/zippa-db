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
//! lines. A stacked bar chart is the exception: it stacks any number of
//! series, either one per picked y column or — when a group column is
//! picked — one per distinct value of that column, pivoted out of rows of
//! (x, group, value). A group with no row at an x stacks nothing there,
//! which is a zero because the query counted nothing.

use gpui_kit::SharedString;
use gpui_kit::component::button::Button;
use gpui_kit::component::chart::{AreaChart, BarChart, LineChart};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::plot::{
    AxisText, Grid, IntoPlot, Plot, PlotAxis, axis_gutter,
    label::TEXT_SIZE,
    scale::{Scale, ScaleBand, ScaleLinear},
    shape::{Bar, Stack, StackSeries},
    tooltip::{CrossLine, Tooltip as PlotTooltip, TooltipState},
};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Context, ElementId, Hsla, Pixels, Point, TextAlign, Window, div,
    point, px,
};

use serde::{Deserialize, Serialize};

use crate::db::query::QueryResult;

use super::panel::SessionPanel;
use super::tab::TabContent;

/// Kept rows are capped: a bar chart of ten thousand bars is mush, and a
/// line chart of them is a paint cost the grid does not pay.
pub(crate) const MAX_CHART_POINTS: usize = 5_000;

/// How many x labels a chart aims to show: with more points than this,
/// every n-th label is kept and the rest of the data still plots, the way
/// the toolkit's own Visitors chart thins a quarter of daily values.
const TARGET_X_LABELS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum ChartKind {
    #[default]
    Line,
    Bar,
    Area,
    StackedBar,
}

/// Which columns a tab charts. `x` is `None` for "not picked yet", and
/// resolves to the defaults below: the first numeric column for y, the
/// first other column for x — or the row number, when the numeric column
/// stands alone. `y` is the picked set in pick order; empty is "not picked
/// yet" and resolves to the first numeric column alone. `group` is the
/// column a stacked bar chart splits its first y column by; other kinds
/// ignore it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChartConfig {
    pub(crate) kind: ChartKind,
    pub(crate) x: Option<usize>,
    pub(crate) y: Vec<usize>,
    pub(crate) group: Option<usize>,
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

/// The series a stacked bar chart makes when it splits `y` by `group`:
/// one series per distinct group value (in first-seen order), all over the
/// same x labels (also in first-seen order), with zero where a group has no
/// row at an x. Rows whose `y` does not read are skipped and counted; a row
/// repeating an (x, group) pair adds to it.
pub(crate) fn grouped_series(
    result: &QueryResult,
    x: Option<usize>,
    y: usize,
    group: usize,
) -> ChartData {
    let mut labels: Vec<SharedString> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut cells: std::collections::HashMap<(usize, usize), f64> = Default::default();
    let mut skipped = 0;
    for (index, row) in result.rows.iter().enumerate() {
        let value = row
            .get(y)
            .and_then(|cell| cell.as_deref())
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite());
        let Some(value) = value else {
            skipped += 1;
            continue;
        };
        let label: SharedString = match x {
            Some(column) => row
                .get(column)
                .and_then(|cell| cell.clone())
                .unwrap_or_default()
                .into(),
            None => (index + 1).to_string().into(),
        };
        let name = row
            .get(group)
            .and_then(|cell| cell.clone())
            .unwrap_or_else(|| "NULL".to_string());
        let at = labels
            .iter()
            .position(|seen| *seen == label)
            .unwrap_or_else(|| {
                labels.push(label);
                labels.len() - 1
            });
        let series = names
            .iter()
            .position(|seen| *seen == name)
            .unwrap_or_else(|| {
                names.push(name);
                names.len() - 1
            });
        *cells.entry((series, at)).or_insert(0.) += value;
    }
    let series = names
        .into_iter()
        .enumerate()
        .map(|(series, name)| ChartSeries {
            name,
            points: labels
                .iter()
                .enumerate()
                .map(|(at, label)| ChartPoint {
                    label: label.clone(),
                    value: cells.get(&(series, at)).copied().unwrap_or(0.),
                })
                .collect(),
        })
        .collect();
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
    let group = config
        .group
        .filter(|&group| config.kind == ChartKind::StackedBar && group < result.columns.len())
        .filter(|group| !ys.contains(group));
    let data = match group {
        // Splitting by a group stacks the first y alone.
        Some(group) => {
            ys.truncate(1);
            grouped_series(result, x, ys[0], group)
        }
        None => chart_series(result, x, &ys),
    };
    Some((config.kind, x, ys, data))
}

/// Map a dashboard widget's stored column *names* to the result's indices,
/// or name the ones that are not there. Names are the dashboard's stable
/// reference — indices would break silently when a query's output columns
/// change — so a missing name is the widget's own error state, not a quiet
/// fallback: any missing name poisons the whole resolution. An empty pick
/// set still means "not picked yet" and comes back empty, for the caller to
/// fill from [`default_picks`] the way [`resolve`] does. The indices that
/// do come back go straight into [`resolve`], so the numeric, dedupe, and
/// default rules there still apply.
pub(crate) fn resolve_names(
    result: &QueryResult,
    x: Option<&str>,
    ys: &[String],
) -> Result<(Option<usize>, Vec<usize>), Vec<String>> {
    let mut missing = Vec::new();
    let x = match x {
        None => None,
        Some(name) => match result.columns.iter().position(|column| column == name) {
            Some(index) => Some(index),
            None => {
                missing.push(name.to_string());
                None
            }
        },
    };
    let mut indices = Vec::new();
    for name in ys {
        match result.columns.iter().position(|column| column == name) {
            Some(index) => indices.push(index),
            None => missing.push(name.clone()),
        }
    }
    if missing.is_empty() {
        Ok((x, indices))
    } else {
        Err(missing)
    }
}

/// The chart element itself, without the bar above it: bars for one series,
/// lines for one series, the multi-series element otherwise — lines are its
/// strokes with the fill turned transparent, areas keep the translucent
/// fill. The caller truncates `data` to [`MAX_CHART_POINTS`] first and
/// derives the label stride from what survived. The id must be unique
/// across the window: a dashboard holds one chart per widget, where a
/// query tab holds one.
pub(crate) fn chart_element(
    id: String,
    kind: ChartKind,
    data: &ChartData,
    margin: usize,
    palette: &[Hsla],
) -> AnyElement {
    let multi = data.series.len() > 1;
    match (kind, multi) {
        (ChartKind::StackedBar, _) => {
            StackedBarChart::new(id, data, margin, palette).into_any_element()
        }
        (ChartKind::Bar, false) => {
            let points = data
                .series
                .first()
                .map(|series| series.points.clone())
                .unwrap_or_default();
            BarChart::new(points)
                .id(id)
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
                .id(id)
                .x(|point: &ChartPoint| point.label.clone())
                .y(|point: &ChartPoint| point.value)
                .tick_margin(margin)
                .into_any_element()
        }
        _ => {
            let fill_opacity = match kind {
                ChartKind::Area => 0.4,
                _ => 0.,
            };
            let rows = chart_rows(data);
            let mut element = AreaChart::new(rows)
                .id(id)
                .x(|row: &ChartRow| row.label.clone())
                .tick_margin(margin);
            for (index, series) in data.series.iter().enumerate() {
                let color = series_color(index, palette);
                element = element
                    .y(move |row: &ChartRow| row.values[index])
                    .stroke(color)
                    .fill(color.opacity(fill_opacity))
                    .name(series.name.clone());
            }
            element.into_any_element()
        }
    }
}

/// The room under the plot kept for the x-axis labels.
fn axis_gap() -> f32 {
    axis_gutter(px(TEXT_SIZE))
}

/// A band scale over the chart's columns, shared by painting and the
/// tooltip so a hover lands on the bar it is over.
fn column_scale(count: usize, width: f32) -> ScaleBand<usize> {
    ScaleBand::new(0..count, [0., width])
        .max_band_width(30.)
        .padding_inner(0.4)
        .padding_outer(0.2)
}

/// Stacked bars, drawn the way the toolkit's own stacked-bar story does
/// (it has no stacking chart of its own): a band per x label, a [`Stack`]
/// of the series over it, dashed grid lines, a column highlight and a
/// tooltip with one row per series and a total. Negatives stack as zero.
#[derive(IntoPlot)]
struct StackedBarChart {
    id: ElementId,
    rows: Vec<ChartRow>,
    names: Vec<String>,
    colors: Vec<Hsla>,
    margin: usize,
    series: Vec<StackSeries<usize>>,
}

impl StackedBarChart {
    fn new(id: String, data: &ChartData, margin: usize, palette: &[Hsla]) -> Self {
        let rows = chart_rows(data);
        let names: Vec<String> = data
            .series
            .iter()
            .map(|series| series.name.clone())
            .collect();
        let colors = (0..names.len())
            .map(|index| series_color(index, palette))
            .collect();
        let values: Vec<Vec<f32>> = rows
            .iter()
            .map(|row| {
                row.values
                    .iter()
                    .map(|value| value.max(0.) as f32)
                    .collect()
            })
            .collect();
        let series = Stack::new()
            .data(0..rows.len())
            .keys((0..names.len()).map(|index| index.to_string()))
            .value(move |&row: &usize, key: &str| {
                values[row].get(key.parse::<usize>().ok()?).copied()
            })
            .series();
        Self {
            id: ElementId::Name(id.into()),
            rows,
            names,
            colors,
            margin: margin.max(1),
            series,
        }
    }
}

impl Plot for StackedBarChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32() - axis_gap();
        let x = column_scale(self.rows.len(), width);
        let band_width = x.band_width();
        let max = self
            .series
            .iter()
            .flat_map(|series| series.points.iter().map(|point| point.y1))
            .fold(0., f32::max) as f64;
        let y = ScaleLinear::new(vec![0., max], [height, 10.]);

        let margin = self.margin;
        let labels = self.rows.iter().enumerate().filter_map(|(at, row)| {
            if at % margin != 0 {
                return None;
            }
            x.tick(&at).map(|tick| {
                AxisText::new(
                    row.label.clone(),
                    tick + band_width / 2.,
                    cx.theme().muted_foreground,
                )
                .align(TextAlign::Center)
            })
        });
        PlotAxis::new()
            .x(height)
            .x_label(labels)
            .stroke(cx.theme().border)
            .paint(&bounds, window, cx);

        Grid::new()
            .y((0..=3).map(|i| height * i as f32 / 4.0))
            .stroke(cx.theme().border)
            .dash_array(&[px(4.), px(2.)])
            .paint(&bounds, window);

        for series in &self.series {
            let (x, y0, y1) = (x.clone(), y.clone(), y.clone());
            let fill = self.colors.get(series.index).copied().unwrap_or_default();
            Bar::new()
                .data(&series.points)
                .band_width(band_width)
                .cross(move |point| x.tick(&point.data))
                .base(move |point| y0.tick(&(point.y0 as f64)).unwrap_or(height))
                .value(move |point| y1.tick(&(point.y1 as f64)))
                .fill(move |_, _, _| fill)
                .paint(&bounds, window, cx);
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        // Hovering the label gutter shows nothing.
        if position.y.as_f32() > bounds.size.height.as_f32() - axis_gap() {
            return None;
        }
        let x = column_scale(self.rows.len(), bounds.size.width.as_f32());
        let index = x.nearest_index(position.x.as_f32());
        self.rows.get(index)?;
        let center = x.tick(&index)? + x.band_width() / 2.;
        Some(TooltipState::new(
            index,
            point(px(center), position.y),
            vec![],
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let row = self.rows.get(state.index)?;
        let band = column_scale(self.rows.len(), bounds.size.width.as_f32()).band_width();
        let mut tooltip = PlotTooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(
                CrossLine::new(state.cross_line)
                    .height(bounds.size.height.as_f32() - axis_gap())
                    .band(px(band)),
            )
            .title(row.label.clone());
        let mut total = 0.;
        for (index, name) in self.names.iter().enumerate() {
            let value = row.values.get(index).copied().unwrap_or(0.);
            total += value.max(0.);
            let color = self.colors.get(index).copied().unwrap_or_default();
            tooltip = tooltip.row(color, name.clone(), value.to_string());
        }
        Some(
            tooltip
                .plain_row("total", total.to_string())
                .into_any_element(),
        )
    }
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

    /// Show the chart pane or the result grid. The chart, the plan, and
    /// the messages are views of the same pane, so showing one hides the
    /// others.
    pub(crate) fn set_show_chart(&mut self, show: bool, cx: &mut Context<Self>) {
        if let TabContent::Query {
            show_chart,
            show_plan,
            show_messages,
            ..
        } = &mut self.content
        {
            *show_chart = show;
            *show_plan = false;
            *show_messages = false;
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

    /// The column a stacked bar chart splits by, or `None` for one stack
    /// segment per picked y column.
    pub(crate) fn set_chart_group(&mut self, group: Option<usize>, cx: &mut Context<Self>) {
        if let TabContent::Query { chart, .. } = &mut self.content {
            chart.group = group;
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
            ChartKind::StackedBar => "StackedBar",
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
            .child(div().flex_1().min_h_0().p_2().child(chart_element(
                "result-chart".to_string(),
                kind,
                &data,
                margin,
                &palette,
            )))
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
        let group_columns = columns.clone();
        let weak_group = weak.clone();
        let group = match &self.content {
            TabContent::Query { chart, .. } => chart.group,
            _ => None,
        };
        let group_label = match group {
            Some(column) => result.columns.get(column).cloned().unwrap_or_default(),
            None => "None".to_string(),
        };
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
                            3 => ChartKind::StackedBar,
                            _ => ChartKind::Line,
                        };
                        this.set_chart_kind(kind, cx);
                    }))
                    .child(Tab::new().aria_label("Line").label("Line"))
                    .child(Tab::new().aria_label("Bar").label("Bar"))
                    .child(Tab::new().aria_label("Area").label("Area"))
                    .child(Tab::new().aria_label("Stacked").label("Stacked")),
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
            .when(kind == ChartKind::StackedBar, |this| {
                let group_columns = group_columns.clone();
                this.child(
                    Button::new("chart-group")
                        .outline()
                        .xsmall()
                        .label(format!("Group: {group_label}"))
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut menu, _window, _cx| {
                            let none = {
                                let weak = weak_group.clone();
                                PopupMenuItem::new("None")
                                    .checked(group.is_none())
                                    .on_click(move |_, _window, cx| {
                                        if let Some(view) = weak.upgrade() {
                                            view.update(cx, |view, cx| {
                                                view.set_chart_group(None, cx)
                                            });
                                        }
                                    })
                            };
                            menu = menu.item(none);
                            for (column, name) in group_columns.iter().enumerate() {
                                let weak = weak_group.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(name.clone())
                                        .checked(group == Some(column))
                                        .on_click(move |_, _window, cx| {
                                            if let Some(view) = weak.upgrade() {
                                                view.update(cx, |view, cx| {
                                                    view.set_chart_group(Some(column), cx)
                                                });
                                            }
                                        }),
                                );
                            }
                            menu
                        }),
                )
            })
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
    fn grouping_pivots_rows_into_aligned_series() {
        let result = result(
            &["site", "day", "count"],
            vec![
                vec![Some("re"), Some("mon"), Some("3")],
                vec![Some("rt"), Some("mon"), Some("5")],
                vec![Some("re"), Some("tue"), Some("2")],
                vec![Some("se"), Some("tue"), Some("x")],
            ],
        );
        let data = grouped_series(&result, Some(1), 2, 0);
        assert_eq!(data.skipped, 1);
        let names: Vec<_> = data.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["re", "rt"]);
        let values =
            |i: usize| -> Vec<f64> { data.series[i].points.iter().map(|p| p.value).collect() };
        assert_eq!(values(0), [3., 2.]);
        // rt has nothing on tue: it stacks zero, not a gap.
        assert_eq!(values(1), [5., 0.]);
        assert_eq!(data.series[1].points[1].label.as_ref(), "tue");
    }

    #[test]
    fn only_a_stacked_chart_groups() {
        let result = result(
            &["site", "day", "count"],
            vec![vec![Some("re"), Some("mon"), Some("3")]],
        );
        let mut config = ChartConfig {
            kind: ChartKind::Bar,
            x: Some(1),
            y: vec![2],
            group: Some(0),
        };
        assert_eq!(resolve(&result, &config).unwrap().3.series[0].name, "count");
        config.kind = ChartKind::StackedBar;
        assert_eq!(resolve(&result, &config).unwrap().3.series[0].name, "re");
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
            ..ChartConfig::default()
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
            ..ChartConfig::default()
        };
        let (_kind, _x, ys, data) = resolve(&result, &config).expect("chartable");
        assert_eq!(ys, vec![1]);
        assert_eq!(data.series.len(), 1);
    }

    #[test]
    fn names_resolve_to_indices_and_missing_names_poison_the_widget() {
        let result = result(
            &["day", "desktop", "mobile"],
            vec![vec![Some("mon"), Some("10"), Some("4")]],
        );
        assert_eq!(
            resolve_names(
                &result,
                Some("day"),
                &["desktop".to_string(), "mobile".to_string()]
            ),
            Ok((Some(0), vec![1, 2]))
        );
        assert_eq!(
            resolve_names(&result, Some("gone"), &["desktop".to_string()]),
            Err(vec!["gone".to_string()])
        );
        assert_eq!(
            resolve_names(&result, None, &["desktop".to_string(), "gone".to_string()]),
            Err(vec!["gone".to_string()])
        );
        // Empty picks come back empty, for the caller to fill from defaults.
        assert_eq!(resolve_names(&result, None, &[]), Ok((None, Vec::new())));
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
