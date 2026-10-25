//! A dashboard tab: a named set of widgets, each running its own SQL and
//! showing its result as a chart or a data grid.
//!
//! A dashboard is its own snapshot, not a view over saved queries: each
//! widget carries its SQL in the dashboard itself, and the dashboard is
//! re-run — on open, and on the Refresh button — rather than restored from
//! stored results. Variables are dashboard-global: one value per name,
//! shared by every widget that references it, persisted with the dashboard
//! and editable from the header. A widget that fails, or whose columns
//! moved, is its own error cell; the rest of the dashboard still renders.
//!
//! The run path is the query tab's, one widget at a time: substitute the
//! dashboard's variables, apply the implicit limit, refuse writes the
//! session refuses, and run through [`Connection::run_query_with`] on the
//! runtime thread. A refresh in flight is cancelled by any edit — a
//! generation counter, checked between widgets and before the epilogue,
//! keeps a cancelled driver's results from landing on edited widgets.

use std::sync::Arc;

use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Window, actions, div, px,
};

use crate::dashboards::{self, Dashboard, Widget, WidgetDisplay};
use crate::db::params::Variable;
use crate::db::query::QueryResult;
use crate::db::runtime;
use crate::db::{Connection, Engine, params, statement};
use crate::settings::Settings;
use crate::ui::completion::SharedCatalog;
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::{QueryEditor, QueryEditorEvent};
use crate::ui::variables_dialog::{VariablesEvent, VariablesView};

#[cfg(test)]
use super::chart::default_picks;
use super::chart::{
    ChartConfig, ChartData, ChartKind, MAX_CHART_POINTS, chart_element, chartable, resolve,
    resolve_names, series_color, tick_margin_for,
};

actions!(zippa_db, [CloseWidgetDialog]);

/// What a widget shows while it has no result: nothing yet, running, done,
/// or its own failure, said in words.
#[derive(Clone, PartialEq)]
enum WidgetStatus {
    Idle,
    Running,
    Done,
    Error(String),
}

/// One widget's live state. The grid exists for every widget, chart or
/// grid, so flipping a widget's display never has to build one mid-render;
/// it just stops being shown.
struct WidgetState {
    grid: Entity<DataGrid>,
    status: WidgetStatus,
    result: Option<QueryResult>,
    applied_limit: Option<usize>,
}

pub(crate) struct DashboardView {
    name: String,
    dashboard: Dashboard,
    connection: Arc<Connection>,
    engine: Engine,
    catalog: SharedCatalog,
    focus: FocusHandle,
    widgets: Vec<WidgetState>,
    /// A refresh driver is running. Edits cancel it first; the Refresh
    /// button while one runs is a no-op, so two drivers never interleave.
    refreshing: bool,
    /// Bumped on every cancel. The driver captures its generation at start
    /// and checks it between widgets and before its epilogue, so a
    /// superseded driver cannot land results on edited widgets.
    generation: u64,
    running: Option<tokio::task::AbortHandle>,
    variables_dialog: Option<Entity<VariablesView>>,
}

impl DashboardView {
    pub(crate) fn new(
        dashboard: Dashboard,
        connection: Arc<Connection>,
        catalog: SharedCatalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = connection.config.engine;
        let name = dashboard.name.clone();
        let widgets = dashboard
            .widgets
            .iter()
            .map(|_| WidgetState {
                grid: cx.new(|cx| DataGrid::new(engine, window, cx)),
                status: WidgetStatus::Idle,
                result: None,
                applied_limit: None,
            })
            .collect();
        Self {
            name,
            dashboard,
            connection,
            engine,
            catalog,
            focus: cx.focus_handle(),
            widgets,
            refreshing: false,
            generation: 0,
            running: None,
            variables_dialog: None,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// A new connection (or database) invalidates every result, the way a
    /// query tab's results are invalidated; it does not re-run anything,
    /// because a run could repeat a write the user did not ask for.
    pub(crate) fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.cancel_refresh(cx);
        self.connection = connection.clone();
        self.engine = connection.config.engine;
        for widget in &mut self.widgets {
            widget.status = WidgetStatus::Idle;
            widget.result = None;
            widget.applied_limit = None;
            let grid = widget.grid.clone();
            grid.update(cx, |grid, cx| grid.clear(cx));
        }
        cx.notify();
    }

    /// Replace the whole snapshot, as when the dashboard is recreated
    /// under the same name from the sidebar: the widget states are rebuilt
    /// and results cleared, so an edit in the tab cannot resurrect the
    /// replaced dashboard's widgets.
    pub(crate) fn set_dashboard(
        &mut self,
        dashboard: Dashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_refresh(cx);
        self.name = dashboard.name.clone();
        self.dashboard = dashboard;
        let engine = self.engine;
        self.widgets = self
            .dashboard
            .widgets
            .iter()
            .map(|_| WidgetState {
                grid: cx.new(|cx| DataGrid::new(engine, window, cx)),
                status: WidgetStatus::Idle,
                result: None,
                applied_limit: None,
            })
            .collect();
        cx.notify();
    }

    /// Stop a refresh in flight, from the panel's abort path and from any
    /// edit: results stay, running cells go back to idle.
    pub(crate) fn abort_running(&mut self, cx: &mut Context<Self>) {
        self.cancel_refresh(cx);
    }

    fn cancel_refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.refreshing = false;
        if let Some(running) = self.running.take() {
            running.abort();
        }
        for widget in &mut self.widgets {
            if widget.status == WidgetStatus::Running {
                widget.status = WidgetStatus::Idle;
            }
        }
        cx.notify();
    }

    /// The Refresh button's run path: one confirmation for the whole
    /// refresh when any widget writes, the way a script is confirmed as a
    /// whole, and per-widget refusals for the rest.
    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        if self.connection.config.safety.confirms_writes()
            && self
                .dashboard
                .widgets
                .iter()
                .any(|widget| statement::first_write(&widget.sql, self.engine).is_some())
        {
            // One question at a time: a second Refresh while the dialog is
            // open must not stack another, or run the writes unconfirmed.
            if window.has_active_dialog(cx) {
                return;
            }
            let view = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                // The builder can run more than once, so it clones the
                // weak handle for the confirmation rather than moving its
                // own capture into it.
                let view = view.clone();
                alert
                    .title("Run this dashboard?")
                    .description("This dashboard runs queries that change data. Run it anyway?")
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text("Run dashboard")
                            .cancel_text("Cancel")
                            .show_cancel(true),
                    )
                    .on_ok(move |_, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |view, cx| view.start_refresh_all(cx));
                        }
                        true
                    })
            });
            return;
        }
        self.start_refresh_all(cx);
    }

    fn start_refresh_all(&mut self, cx: &mut Context<Self>) {
        let indices = (0..self.widgets.len()).collect();
        self.start_refresh(indices, cx);
    }

    /// Run a subset of widgets, one at a time, on the runtime thread. The
    /// SQL and variables are snapshotted up front, so an edit mid-refresh
    /// cannot change what a running widget runs — and the generation check
    /// between widgets stops the driver once an edit has cancelled it.
    fn start_refresh(&mut self, indices: Vec<usize>, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.refreshing = true;
        for &index in &indices {
            if let Some(widget) = self.widgets.get_mut(index) {
                widget.status = WidgetStatus::Running;
            }
        }
        cx.notify();
        let (limit_on, limit_rows) = {
            let settings = Settings::global(cx);
            (settings.implicit_limit, settings.implicit_limit_rows)
        };
        let limit = limit_on.then_some(limit_rows);
        let engine = self.engine;
        let connection = self.connection.clone();
        let jobs: Vec<(usize, String, Vec<Variable>)> = indices
            .iter()
            .filter_map(|&index| {
                self.dashboard.widgets.get(index).map(|widget| {
                    (
                        index,
                        widget.sql.clone(),
                        self.dashboard
                            .variables
                            .iter()
                            .map(|(name, value)| Variable {
                                name: name.clone(),
                                value: value.clone(),
                            })
                            .collect(),
                    )
                })
            })
            .collect();
        cx.spawn(async move |this, cx| {
            for (index, sql, variables) in jobs {
                let alive = this.update_in(cx, |view, _, _| view.generation == generation);
                if !matches!(alive, Ok(true)) {
                    break;
                }
                let substituted = params::substitute(&sql, &variables, engine);
                let (sql, params, applied) = match substituted {
                    Ok((sql, params)) => {
                        let limited =
                            limit.and_then(|rows| statement::with_limit(&sql, rows, engine));
                        let applied = limit.filter(|_| limited.is_some());
                        (limited.unwrap_or(sql), params, applied)
                    }
                    Err(error) => {
                        this.update_in(cx, |view, _, cx| {
                            view.set_widget_error(index, format!("{error:#}"), cx)
                        })
                        .ok();
                        continue;
                    }
                };
                let connection = connection.clone();
                let task =
                    runtime::spawn(async move { connection.run_query_with(&sql, params).await });
                this.update_in(cx, |view, _, _| view.running = task.abort_handle())
                    .ok();
                let result = task.await;
                let landed = this.update_in(cx, |view, _, cx| {
                    view.running = None;
                    match result {
                        Ok(Ok(result)) => {
                            view.set_widget_result(index, result, applied, cx);
                            true
                        }
                        Ok(Err(error)) => {
                            view.set_widget_error(index, format!("{error:#}"), cx);
                            true
                        }
                        // Aborted: the canceller already reset the cells.
                        Err(_) => false,
                    }
                });
                if !matches!(landed, Ok(true)) {
                    break;
                }
            }
            this.update_in(cx, |view, _, cx| {
                if view.generation == generation {
                    view.refreshing = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-run one widget after an edit, with its own confirmation when it
    /// writes. Cancels a full refresh first, so the full refresh's stale
    /// snapshot cannot overwrite the edited widget.
    fn refresh_one(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            self.cancel_refresh(cx);
        }
        let Some(widget) = self.dashboard.widgets.get(index) else {
            return;
        };
        if self.connection.config.safety.confirms_writes()
            && statement::first_write(&widget.sql, self.engine).is_some()
        {
            // One question at a time, as in [`DashboardView::refresh`].
            if window.has_active_dialog(cx) {
                return;
            }
            let view = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                // The builder can run more than once, so it clones the
                // weak handle for the confirmation rather than moving its
                // own capture into it.
                let view = view.clone();
                alert
                    .title("Run this widget?")
                    .description("This widget runs a query that changes data. Run it anyway?")
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text("Run widget")
                            .cancel_text("Cancel")
                            .show_cancel(true),
                    )
                    .on_ok(move |_, _, cx| {
                        if let Some(view) = view.upgrade() {
                            view.update(cx, |view, cx| view.start_refresh(vec![index], cx));
                        }
                        true
                    })
            });
            return;
        }
        self.start_refresh(vec![index], cx);
    }

    fn set_widget_result(
        &mut self,
        index: usize,
        result: QueryResult,
        applied: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(widget) = self.widgets.get_mut(index) else {
            return;
        };
        widget.status = WidgetStatus::Done;
        widget.applied_limit = applied;
        let grid = widget.grid.clone();
        grid.update(cx, |grid, cx| grid.set_result(result.clone(), cx));
        widget.result = Some(result);
        cx.notify();
    }

    fn set_widget_error(&mut self, index: usize, message: String, cx: &mut Context<Self>) {
        let Some(widget) = self.widgets.get_mut(index) else {
            return;
        };
        widget.status = WidgetStatus::Error(message);
        widget.result = None;
        widget.applied_limit = None;
        let grid = widget.grid.clone();
        grid.update(cx, |grid, cx| grid.clear(cx));
        cx.notify();
    }

    /// The dashboard's variables, pre-filled with the stored values: one
    /// value per name, shared by every widget that references it.
    pub(crate) fn open_variables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.variables_dialog.is_some() {
            return;
        }
        let variables: Vec<Variable> = self
            .dashboard
            .variables
            .iter()
            .map(|(name, value)| Variable {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        let view = cx.new(|cx| VariablesView::new(variables, window, cx));
        cx.subscribe_in(&view, window, Self::on_variables_event)
            .detach();
        let weak = cx.entity().downgrade();
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Dashboard variables")
                .w(px(460.))
                .keyboard(false)
                .on_close(move |_, _, cx| {
                    weak.update(cx, |view, cx| {
                        view.variables_dialog = None;
                        cx.notify();
                    })
                    .ok();
                })
                .child(body.clone())
        });
        view.update(cx, |view, cx| view.focus(window, cx));
        self.variables_dialog = Some(view);
    }

    fn on_variables_event(
        &mut self,
        _: &Entity<VariablesView>,
        event: &VariablesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            VariablesEvent::Dismissed => {
                self.variables_dialog = None;
                window.close_dialog(cx);
                cx.notify();
            }
            VariablesEvent::Saved(variables) => {
                self.dashboard.variables = variables
                    .iter()
                    .map(|variable| (variable.name.clone(), variable.value.clone()))
                    .collect();
                dashboards::upsert(&self.dashboard, cx);
                self.variables_dialog = None;
                // Close first, so a write confirmation for the re-run can open.
                window.close_dialog(cx);
                self.refresh(window, cx);
                cx.notify();
            }
        }
    }

    fn open_widget_dialog(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let widget = index.and_then(|index| self.dashboard.widgets.get(index).cloned());
        let view =
            WidgetEditDialog::open(widget, index, self.engine, self.catalog.clone(), window, cx);
        cx.subscribe_in(&view, window, Self::on_widget_event)
            .detach();
    }

    /// The widget dialog's answer: a new widget to append, or a replacement
    /// for the widget it was opened over. The dialog echoes the index it
    /// was opened with, so no dialog state is kept here between open and
    /// save — escaping out of it leaves nothing stale behind.
    fn on_widget_event(
        &mut self,
        _: &Entity<WidgetEditDialog>,
        event: &WidgetEditEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WidgetEditEvent::Saved { index, widget } => match index {
                Some(index) => self.replace_widget(*index, widget.clone(), window, cx),
                None => self.add_widget(widget.clone(), window, cx),
            },
        }
    }

    fn add_widget(&mut self, widget: Widget, window: &mut Window, cx: &mut Context<Self>) {
        self.dashboard.widgets.push(widget);
        let engine = self.engine;
        self.widgets.push(WidgetState {
            grid: cx.new(|cx| DataGrid::new(engine, window, cx)),
            status: WidgetStatus::Idle,
            result: None,
            applied_limit: None,
        });
        dashboards::upsert(&self.dashboard, cx);
        cx.notify();
        let index = self.widgets.len() - 1;
        self.refresh_one(index, window, cx);
    }

    fn replace_widget(
        &mut self,
        index: usize,
        widget: Widget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(slot) = self.dashboard.widgets.get_mut(index) else {
            return;
        };
        *slot = widget;
        dashboards::upsert(&self.dashboard, cx);
        cx.notify();
        self.refresh_one(index, window, cx);
    }

    fn delete_widget(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.refreshing {
            self.cancel_refresh(cx);
        }
        if index >= self.dashboard.widgets.len() {
            return;
        }
        self.dashboard.widgets.remove(index);
        self.widgets.remove(index);
        dashboards::upsert(&self.dashboard, cx);
        cx.notify();
    }
}

/// A widget's header line: its number, then the SQL's first non-empty line,
/// cut to sixty characters so a long query never eats the header.
fn widget_header_summary(sql: &str) -> String {
    let first = sql
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut summary: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        summary.push('…');
    }
    summary
}

/// What a chart widget draws with, or why it draws none. Names resolve
/// through [`resolve_names`] first — a stored name that is not in the
/// result is the widget's own error, not a fallback — and the surviving
/// indices go through [`resolve`], so the numeric, dedupe, and default
/// rules there still apply.
enum WidgetChart {
    Draw { kind: ChartKind, data: ChartData },
    Missing(Vec<String>),
    NotChartable,
}

fn widget_chart(result: &QueryResult, display: &WidgetDisplay) -> WidgetChart {
    let WidgetDisplay::Chart { kind, x, y } = display else {
        return WidgetChart::NotChartable;
    };
    if !chartable(result) {
        return WidgetChart::NotChartable;
    }
    let (x, ys) = match resolve_names(result, x.as_deref(), y) {
        Ok(resolved) => resolved,
        Err(missing) => return WidgetChart::Missing(missing),
    };
    let config = ChartConfig {
        kind: *kind,
        x,
        y: ys,
    };
    match resolve(result, &config) {
        Some((kind, _x, _ys, data)) => WidgetChart::Draw { kind, data },
        None => WidgetChart::NotChartable,
    }
}

impl Render for DashboardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("dashboard-view")
            .size_full()
            .gap_4()
            .p_4()
            .track_focus(&self.focus)
            .child(self.render_header(cx))
            .child(self.render_grid(cx))
    }
}

impl DashboardView {
    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let refreshing = self.refreshing;
        h_flex()
            .gap_2()
            .justify_between()
            .child(div().text_lg().child(self.name.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("dashboard-variables")
                            .ghost()
                            .small()
                            .label("Variables")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_variables(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("dashboard-add-widget")
                            .ghost()
                            .small()
                            .label("Add widget")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_widget_dialog(None, window, cx)
                            })),
                    )
                    .child(
                        Button::new("dashboard-refresh")
                            .primary()
                            .small()
                            .label(if refreshing {
                                "Refreshing…"
                            } else {
                                "Refresh"
                            })
                            .disabled(refreshing)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    ),
            )
    }

    fn render_grid(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.widgets.is_empty() {
            return div()
                .text_color(cx.theme().muted_foreground)
                .child("No widgets yet — add one above.")
                .into_any_element();
        }
        v_flex()
            .gap_4()
            .children((0..self.widgets.len().div_ceil(2)).map(|row| {
                let first = row * 2;
                let mut cells = vec![self.render_widget(first, cx)];
                if first + 1 < self.widgets.len() {
                    cells.push(self.render_widget(first + 1, cx));
                } else {
                    // An odd cell still takes half the row, so the grid
                    // stays two columns wide instead of stretching.
                    cells.push(div().flex_1().into_any_element());
                }
                h_flex().gap_4().children(cells).into_any_element()
            }))
            .into_any_element()
    }

    fn render_widget(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let widget = &self.dashboard.widgets[index];
        let state = &self.widgets[index];
        let title = format!(
            "Widget {} · {}",
            index + 1,
            widget_header_summary(&widget.sql)
        );
        let body = match &state.status {
            WidgetStatus::Running => div()
                .flex_1()
                .p_2()
                .text_color(cx.theme().muted_foreground)
                .child("Running…")
                .into_any_element(),
            WidgetStatus::Error(message) => div()
                .id(("dashboard-widget-error", index))
                .flex_1()
                .overflow_y_scroll()
                .p_2()
                .text_color(cx.theme().danger)
                .child(message.clone())
                .into_any_element(),
            WidgetStatus::Idle if state.result.is_none() => div()
                .flex_1()
                .p_2()
                .text_color(cx.theme().muted_foreground)
                .child("Not run yet.")
                .into_any_element(),
            _ => match &widget.display {
                WidgetDisplay::Grid => div()
                    .flex_1()
                    .min_h_0()
                    .child(state.grid.clone())
                    .into_any_element(),
                WidgetDisplay::Chart { .. } => self.render_widget_chart(index, cx),
            },
        };
        div()
            .flex_1()
            .h(px(320.))
            .border_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().flex_1().text_sm().child(title))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new(("dashboard-widget-edit", index))
                                            .ghost()
                                            .xsmall()
                                            .label("Edit")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_widget_dialog(Some(index), window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("dashboard-widget-delete", index))
                                            .ghost()
                                            .xsmall()
                                            .label("Delete")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_widget(index, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    /// A chart widget's cell: the chart itself, a legend when there is more
    /// than one series, and the same notes a query tab's chart carries —
    /// skipped rows, the point cap, the applied limit.
    fn render_widget_chart(&self, index: usize, cx: &App) -> AnyElement {
        let widget = &self.dashboard.widgets[index];
        let state = &self.widgets[index];
        let Some(result) = &state.result else {
            return div()
                .flex_1()
                .p_2()
                .text_color(cx.theme().muted_foreground)
                .child("No result yet.")
                .into_any_element();
        };
        match widget_chart(result, &widget.display) {
            WidgetChart::NotChartable => div()
                .flex_1()
                .p_2()
                .text_color(cx.theme().muted_foreground)
                .child("This result has no numeric column to chart.")
                .into_any_element(),
            WidgetChart::Missing(names) => div()
                .flex_1()
                .p_2()
                .text_color(cx.theme().danger)
                .child(format!(
                    "Column{} not found: {}",
                    if names.len() == 1 { "" } else { "s" },
                    names.join(", ")
                ))
                .into_any_element(),
            WidgetChart::Draw { kind, mut data } => {
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
                if let Some(limit) = state.applied_limit {
                    if !note.is_empty() {
                        note.push_str(" · ");
                    }
                    note.push_str(&format!("limited to {limit} rows"));
                }
                let mut body =
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(div().flex_1().min_h_0().p_2().child(chart_element(
                            format!("dashboard-chart-{index}"),
                            kind,
                            &data,
                            margin,
                            &palette,
                        )));
                if data.series.len() > 1 {
                    body = body.child(h_flex().gap_3().px_2().pb_1().flex_wrap().children(
                        data.series.iter().enumerate().map(|(index, series)| {
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .items_center()
                                .child(div().size_2().bg(series_color(index, &palette)))
                                .child(div().text_xs().child(series.name.clone()))
                                .into_any_element()
                        }),
                    ));
                }
                if !note.is_empty() {
                    body = body.child(
                        div()
                            .px_2()
                            .pb_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(note),
                    );
                }
                body.into_any_element()
            }
        }
    }
}

/// What the widget dialog tells its owner: the widget it saved, and the
/// index it was opened over — none for a new widget, which the owner
/// appends. The dialog echoes the index rather than the owner keeping
/// dialog state between open and save, so escaping out leaves nothing
/// stale behind.
pub(crate) enum WidgetEditEvent {
    Saved {
        index: Option<usize>,
        widget: Widget,
    },
}

impl EventEmitter<WidgetEditEvent> for WidgetEditDialog {}

/// The widget dialog: the widget's SQL in a query editor — `Run` /
/// `Cmd+Enter` saves, the way a query tab runs — plus a display picker
/// (chart or data grid) and, for charts, a kind picker and the X/Y column
/// names as text. Names, not positions, so a query whose output columns
/// change does not silently chart the wrong ones.
pub(crate) struct WidgetEditDialog {
    index: Option<usize>,
    editor: Entity<QueryEditor>,
    chart: bool,
    kind: ChartKind,
    x: Entity<InputState>,
    y: Entity<InputState>,
    focus: FocusHandle,
}

impl WidgetEditDialog {
    pub(crate) fn open(
        widget: Option<Widget>,
        index: Option<usize>,
        engine: Engine,
        catalog: SharedCatalog,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| {
            let (chart, kind, x_value, y_value) = match widget.as_ref().map(|w| &w.display) {
                Some(WidgetDisplay::Chart { kind, x, y }) => {
                    (true, *kind, x.clone().unwrap_or_default(), y.join(", "))
                }
                _ => (false, ChartKind::Line, String::new(), String::new()),
            };
            let sql = widget.as_ref().map(|w| w.sql.clone()).unwrap_or_default();
            let editor = cx.new(|cx| QueryEditor::with_text(sql, engine, catalog, window, cx));
            cx.subscribe_in(&editor, window, Self::on_editor_event)
                .detach();
            let x = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(x_value, window, cx);
                state.set_placeholder("x column (optional)", window, cx);
                state
            });
            let y = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(y_value, window, cx);
                state.set_placeholder("y columns, comma-separated", window, cx);
                state
            });
            Self {
                index,
                editor,
                chart,
                kind,
                x,
                y,
                focus: cx.focus_handle(),
            }
        });
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            // The builder can run more than once, so the Save button gets
            // its own clone rather than moving the builder's capture.
            let save = body.clone();
            dialog
                .title(if index.is_none() {
                    "New widget"
                } else {
                    "Edit widget"
                })
                .w(px(640.))
                .keyboard(false)
                .on_close(|_, _, _| {})
                .child(body.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("widget-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("widget-save")
                                .primary()
                                .small()
                                .label("Save")
                                .on_click(move |_, window, cx| {
                                    save.update(cx, |this, cx| this.save_and_close(window, cx));
                                }),
                        ),
                )
        });
        view.update(cx, |this, cx| {
            this.editor.read(cx).focus_handle(cx).focus(window, cx);
        });
        view
    }

    fn collect_widget(&self, cx: &App) -> Widget {
        let sql = self.editor.read(cx).sql(cx);
        let display = if self.chart {
            WidgetDisplay::Chart {
                kind: self.kind,
                x: {
                    let name = self.x.read(cx).value().trim().to_string();
                    if name.is_empty() { None } else { Some(name) }
                },
                y: self
                    .y
                    .read(cx)
                    .value()
                    .split(',')
                    .map(|part| part.trim().to_string())
                    .filter(|part| !part.is_empty())
                    .collect(),
            }
        } else {
            WidgetDisplay::Grid
        };
        Widget { sql, display }
    }

    /// Save, then close: saving runs the widget, and a write needs its own
    /// confirmation dialog — which cannot open while this one is up.
    fn save_and_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let widget = self.collect_widget(cx);
        if widget.sql.trim().is_empty() {
            return;
        }
        window.close_dialog(cx);
        cx.emit(WidgetEditEvent::Saved {
            index: self.index,
            widget,
        });
    }

    /// `Run` in the editor saves, the way `Cmd+Enter` runs a query tab.
    fn on_editor_event(
        &mut self,
        _: &Entity<QueryEditor>,
        event: &QueryEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, QueryEditorEvent::Run(_)) {
            self.save_and_close(window, cx);
        }
    }

    /// `Escape` closes the dialog without saving.
    fn on_close(&mut self, _: &CloseWidgetDialog, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
    }
}

impl Render for WidgetEditDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chart = self.chart;
        let selected_kind = self.kind;
        v_flex()
            .id("widget-dialog")
            .size_full()
            .gap_2()
            .track_focus(&self.focus)
            .key_context("WidgetDialog")
            .on_action(cx.listener(Self::on_close))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Each widget runs its own SQL when the dashboard refreshes."),
            )
            .child(div().h(px(240.)).child(self.editor.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("widget-display-chart")
                            .small()
                            .label("Chart")
                            .when(chart, |button| button.primary())
                            .when(!chart, |button| button.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chart = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("widget-display-grid")
                            .small()
                            .label("Data grid")
                            .when(!chart, |button| button.primary())
                            .when(chart, |button| button.ghost())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chart = false;
                                cx.notify();
                            })),
                    ),
            )
            .when(chart, |this| {
                this.child(
                    h_flex().gap_2().children(
                        [ChartKind::Line, ChartKind::Bar, ChartKind::Area]
                            .iter()
                            .enumerate()
                            .map(|(index, kind)| {
                                let kind = *kind;
                                let label = match kind {
                                    ChartKind::Line => "Line",
                                    ChartKind::Bar => "Bar",
                                    ChartKind::Area => "Area",
                                };
                                Button::new(("widget-chart-kind", index))
                                    .small()
                                    .label(label)
                                    .when(kind == selected_kind, |button| button.primary())
                                    .when(kind != selected_kind, |button| button.ghost())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.kind = kind;
                                        cx.notify();
                                    }))
                            }),
                    ),
                )
            })
            .when(chart, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div().w(px(160.)).child(
                                Input::new(&self.x)
                                    .id("widget-x")
                                    .aria_label("X column")
                                    .small(),
                            ),
                        )
                        .child(
                            div().flex_1().child(
                                Input::new(&self.y)
                                    .id("widget-y")
                                    .aria_label("Y columns")
                                    .small(),
                            ),
                        ),
                )
            })
    }
}

#[cfg(test)]
pub(crate) struct WidgetSummary {
    pub(crate) status: String,
    pub(crate) chart: Option<super::chart::ChartSummary>,
    /// The chart column names that were not in the result, if the widget
    /// failed that way: a done widget with no chart and names here is the
    /// column-not-found state, not a result with nothing to chart.
    pub(crate) chart_missing: Vec<String>,
    pub(crate) rows: Option<usize>,
    pub(crate) applied_limit: Option<usize>,
}

#[cfg(test)]
impl DashboardView {
    pub(crate) fn widget_summaries(&self) -> Vec<WidgetSummary> {
        self.widgets
            .iter()
            .enumerate()
            .map(|(index, state)| {
                let status = match &state.status {
                    WidgetStatus::Idle => "idle".to_string(),
                    WidgetStatus::Running => "running".to_string(),
                    WidgetStatus::Done => "done".to_string(),
                    WidgetStatus::Error(message) => format!("error: {message}"),
                };
                let (chart, chart_missing) =
                    match (&state.result, &self.dashboard.widgets[index].display) {
                        (Some(result), display @ WidgetDisplay::Chart { x: stored_x, .. }) => {
                            match widget_chart(result, display) {
                                WidgetChart::Draw { kind, data } => {
                                    let kind = match kind {
                                        ChartKind::Line => "Line",
                                        ChartKind::Bar => "Bar",
                                        ChartKind::Area => "Area",
                                    };
                                    // The effective x: the stored name when
                                    // there is one, the default pick otherwise —
                                    // the same default `resolve` applies.
                                    let x = match stored_x {
                                        Some(name) => Some(name.clone()),
                                        None => default_picks(result)
                                            .and_then(|(x, _)| x)
                                            .and_then(|column| result.columns.get(column).cloned()),
                                    };
                                    (
                                        Some(super::chart::ChartSummary {
                                            kind: kind.to_string(),
                                            x,
                                            y: data
                                                .series
                                                .iter()
                                                .map(|series| series.name.clone())
                                                .collect(),
                                            points: data
                                                .series
                                                .first()
                                                .map(|series| series.points.len())
                                                .unwrap_or(0),
                                            skipped: data.skipped,
                                        }),
                                        Vec::new(),
                                    )
                                }
                                WidgetChart::Missing(names) => (None, names),
                                WidgetChart::NotChartable => (None, Vec::new()),
                            }
                        }
                        _ => (None, Vec::new()),
                    };
                WidgetSummary {
                    status,
                    chart,
                    chart_missing,
                    rows: state.result.as_ref().map(|result| result.rows.len()),
                    applied_limit: state.applied_limit,
                }
            })
            .collect()
    }

    pub(crate) fn variables_dialog_for_test(&self) -> Option<Entity<VariablesView>> {
        self.variables_dialog.clone()
    }
}

/// What the new-dashboard dialog tells its owner: the name it saved under.
/// The owner stores the empty dashboard and opens it; the dialog itself
/// stores nothing, the way the save-query dialog stores its query before
/// telling its caller the name.
pub(crate) enum NewDashboardEvent {
    Saved(String),
}

impl EventEmitter<NewDashboardEvent> for NewDashboardDialog {}

/// The "New dashboard" dialog: one name box, the save-query dialog's shape.
/// Saving under an existing name replaces that dashboard, said before it
/// happens; the dashboard itself starts empty, and widgets are added from
/// the tab.
pub(crate) struct NewDashboardDialog {
    name: Entity<InputState>,
}

impl NewDashboardDialog {
    pub(crate) fn open(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let view = cx.new(|cx| {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("Dashboard name"));
            cx.observe(&name, |_, _, cx| cx.notify()).detach();
            Self { name }
        });
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            let can_save = body.read(cx).can_save(cx);
            let cancel = body.clone();
            let confirm = body.clone();
            let ok = body.clone();
            dialog
                .title("New dashboard")
                .w(px(360.))
                .overlay_closable(true)
                // `Enter` in the name box is the dialog's Confirm, which
                // lands here; an empty name keeps the dialog open.
                .on_ok(move |_, _, cx| ok.update(cx, |this, cx| this.save(cx)))
                .child(body.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("new-dashboard-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    cancel.update(cx, |_, cx| window.close_dialog(cx));
                                }),
                        )
                        .child(
                            Button::new("new-dashboard-confirm")
                                .primary()
                                .small()
                                .label("Create")
                                .disabled(!can_save)
                                .on_click(move |_, window, cx| {
                                    if confirm.update(cx, |this, cx| this.save(cx)) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
        view.update(cx, |this, cx| {
            this.name.update(cx, |state, cx| {
                state.focus(window, cx);
            });
        });
        view
    }

    fn typed_name(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_string()
    }

    fn can_save(&self, cx: &App) -> bool {
        !self.typed_name(cx).is_empty()
    }

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let name = self.typed_name(cx);
        if name.is_empty() {
            return false;
        }
        cx.emit(NewDashboardEvent::Saved(name));
        true
    }
}

impl Render for NewDashboardDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.typed_name(cx);
        // Saving under a name already in the list replaces that dashboard,
        // which is worth saying before it happens.
        let replaces = !name.is_empty() && dashboards::Dashboards::find(cx, &name).is_some();

        v_flex()
            .gap_2()
            .child(
                Input::new(&self.name)
                    .id("new-dashboard-name")
                    .aria_label("Dashboard name")
                    .small(),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if replaces {
                        "Replaces the dashboard with this name."
                    } else {
                        "A dashboard to fill with widgets."
                    }),
            )
    }
}
