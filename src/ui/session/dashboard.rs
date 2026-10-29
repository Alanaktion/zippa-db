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
//! The run path is the query tab's: substitute the dashboard's variables,
//! apply the implicit limit, refuse writes the session refuses, and run
//! through [`Connection::run_query_with`] on the runtime thread. Widgets
//! run from a queue, each named by a stable id rather than a position.
//! Sequential is the default — one widget at a time — and the
//! `parallel_dashboard_queries` setting runs the queued widgets at once,
//! each on its own pooled connection. Editing a widget queues it behind
//! whatever is running; only that widget's own run in flight is dropped,
//! and a per-widget token keeps a dropped run's rows from landing. Changes
//! that invalidate everything — a new connection, new variables — cancel
//! the whole queue instead.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, FontWeight, SharedString, Window,
    actions, div, px,
};

use crate::dashboards::{self, Dashboard, Widget, WidgetDisplay, WidgetWidth};
use crate::db::params::Variable;
use crate::db::query::QueryResult;
use crate::db::runtime;
use crate::db::{Connection, Engine, params, statement};
use crate::settings::Settings;
use crate::ui::busy::{busy_label, skeleton_chart, skeleton_grid};
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
    /// Waiting its turn behind the widget that is running.
    Queued,
    Running,
    Done,
    Error(String),
}

/// One widget's live state. The grid exists for every widget, chart or
/// grid, so flipping a widget's display never has to build one mid-render;
/// it just stops being shown.
struct WidgetState {
    /// Stable for the widget's life, unlike its position, which a delete or
    /// a move changes under a queued run.
    id: u64,
    grid: Entity<DataGrid>,
    status: WidgetStatus,
    result: Option<QueryResult>,
    applied_limit: Option<usize>,
    /// The token of this widget's run in flight, if any: a run lands its
    /// rows only while it still holds this token, so dropping the run —
    /// by editing or deleting the widget, or cancelling the refresh — is
    /// what keeps stale rows off the screen.
    token: u64,
}

impl WidgetState {
    fn new(id: u64, engine: Engine, window: &mut Window, cx: &mut App) -> Self {
        Self {
            id,
            grid: cx.new(|cx| DataGrid::new(engine, window, cx)),
            status: WidgetStatus::Idle,
            result: None,
            applied_limit: None,
            token: 0,
        }
    }

    /// Forget the result, e.g. when the SQL or connection behind it changed.
    fn reset(&mut self, cx: &mut App) {
        self.status = WidgetStatus::Idle;
        self.result = None;
        self.applied_limit = None;
        self.grid.update(cx, |grid, cx| grid.clear(cx));
    }
}

/// A widget taken off the queue, with what it runs.
struct Job {
    id: u64,
    token: u64,
    sql: String,
    variables: Vec<Variable>,
}

pub(crate) struct DashboardView {
    dashboard: Dashboard,
    connection: Arc<Connection>,
    engine: Engine,
    catalog: SharedCatalog,
    focus: FocusHandle,
    widgets: Vec<WidgetState>,
    /// A driver is working through `queue`. There is at most one, so two
    /// never interleave; the Refresh button while it runs is a no-op.
    refreshing: bool,
    /// Widgets waiting to run, by id, in the order they were asked for.
    queue: VecDeque<u64>,
    /// Widgets whose job left the queue and has not landed or been dropped.
    in_flight: HashSet<u64>,
    /// Bumped whenever a run starts or is dropped, and handed to the widget
    /// as its token. A run lands its rows only while it still holds its
    /// widget's token.
    job: u64,
    /// Bumped by a cancel of everything. The driver exits when it sees a
    /// different epoch than it started in.
    epoch: u64,
    next_id: u64,
    /// Abort handles for the runs in flight, by widget id.
    running: HashMap<u64, tokio::task::AbortHandle>,
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
        let dashboard_len = dashboard.widgets.len() as u64;
        let widgets = (0..dashboard.widgets.len() as u64)
            .map(|id| WidgetState::new(id, engine, window, cx))
            .collect();
        Self {
            dashboard,
            connection,
            engine,
            catalog,
            focus: cx.focus_handle(),
            widgets,
            refreshing: false,
            queue: VecDeque::new(),
            in_flight: HashSet::new(),
            job: 0,
            epoch: 0,
            next_id: dashboard_len,
            running: HashMap::new(),
            variables_dialog: None,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.dashboard.name
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
            widget.reset(cx);
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
        self.dashboard = dashboard;
        let engine = self.engine;
        self.widgets = (0..self.dashboard.widgets.len())
            .map(|_| {
                let id = self.next_id;
                self.next_id += 1;
                WidgetState::new(id, engine, window, cx)
            })
            .collect();
        cx.notify();
    }

    /// Stop a refresh in flight, from the panel's abort path and from any
    /// edit: results stay, running cells go back to idle.
    pub(crate) fn abort_running(&mut self, cx: &mut Context<Self>) {
        self.cancel_refresh(cx);
    }

    /// Drop everything queued and in flight. Results stay; cells that were
    /// waiting or running go back to idle.
    fn cancel_refresh(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        self.refreshing = false;
        self.queue.clear();
        for id in self.in_flight.iter().copied().collect::<Vec<_>>() {
            self.discard(id);
        }
        for widget in &mut self.widgets {
            if matches!(widget.status, WidgetStatus::Running | WidgetStatus::Queued) {
                widget.status = WidgetStatus::Idle;
            }
        }
        cx.notify();
    }

    /// Drop widget `id`'s run in flight, if any, so its rows never land:
    /// its token moves on, so the run fails the landing check, and its
    /// abort handle is fired. The driver carries on with the queue.
    fn discard(&mut self, id: u64) {
        if !self.in_flight.remove(&id) {
            return;
        }
        self.job += 1;
        if let Some(widget) = self.widgets.iter_mut().find(|widget| widget.id == id) {
            widget.token = self.job;
        }
        if let Some(running) = self.running.remove(&id) {
            running.abort();
        }
    }

    /// Whether `token` is still the run widget `id` is waiting on: a run
    /// whose widget was edited, deleted, or cancelled holds a stale token.
    fn token_current(&self, id: u64, token: u64) -> bool {
        self.widgets
            .iter()
            .find(|widget| widget.id == id)
            .is_some_and(|widget| widget.token == token)
    }

    /// Take widget `id` out of the queue and out of flight, as when it is
    /// edited or deleted and whatever it was about to run is stale.
    fn withdraw(&mut self, id: u64) {
        self.queue.retain(|queued| *queued != id);
        self.discard(id);
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.widgets.iter().position(|widget| widget.id == id)
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

    fn variables(&self) -> Vec<Variable> {
        self.dashboard
            .variables
            .iter()
            .map(|(name, value)| Variable {
                name: name.clone(),
                value: value.clone(),
            })
            .collect()
    }

    fn start_refresh_all(&mut self, cx: &mut Context<Self>) {
        let ids = self.widgets.iter().map(|widget| widget.id).collect();
        self.start_refresh(ids, cx);
    }

    /// Queue widgets to run on the runtime thread: one at a time by
    /// default, all at once with `parallel_dashboard_queries`. A widget
    /// already waiting keeps its place; one whose query is in flight has
    /// that query dropped and goes to the back, since its SQL or variables
    /// are what changed. Every other widget's run is left alone.
    fn start_refresh(&mut self, ids: Vec<u64>, cx: &mut Context<Self>) {
        for id in ids {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            self.withdraw_if_running(id);
            if !self.queue.contains(&id) {
                self.queue.push_back(id);
            }
            self.widgets[index].status = WidgetStatus::Queued;
        }
        cx.notify();
        if !self.refreshing && !self.queue.is_empty() {
            self.refreshing = true;
            self.drive(cx);
        }
    }

    fn withdraw_if_running(&mut self, id: u64) {
        self.discard(id);
    }

    /// The next widget to run: its SQL and variables are read now, at its
    /// turn, so an edit made while it waited is what runs. `None` when the
    /// queue is empty or everything was cancelled.
    fn next_job(&mut self, epoch: u64) -> Option<Job> {
        if self.epoch != epoch {
            return None;
        }
        while let Some(id) = self.queue.pop_front() {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            self.job += 1;
            self.in_flight.insert(id);
            self.widgets[index].token = self.job;
            self.widgets[index].status = WidgetStatus::Running;
            return Some(Job {
                id,
                token: self.job,
                sql: self.dashboard.widgets[index].sql.clone(),
                variables: self.variables(),
            });
        }
        None
    }

    /// Work through the queue until it is empty. Each pass takes one batch
    /// — a single widget when sequential, the whole queue when parallel —
    /// and runs the batch's queries together, landing each result as it
    /// finishes. A widget queued while a batch runs is picked up by the
    /// next pass, so a refresh in flight never needs a second driver.
    fn drive(&mut self, cx: &mut Context<Self>) {
        let epoch = self.epoch;
        let (limit_on, limit_rows, parallel) = {
            let settings = Settings::global(cx);
            (
                settings.implicit_limit,
                settings.implicit_limit_rows,
                settings.parallel_dashboard_queries,
            )
        };
        let limit = limit_on.then_some(limit_rows);
        let engine = self.engine;
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let jobs = this.update_in(cx, |view, _, cx| {
                    let mut jobs = Vec::new();
                    while let Some(job) = view.next_job(epoch) {
                        jobs.push(job);
                        if !parallel {
                            break;
                        }
                    }
                    cx.notify();
                    jobs
                });
                let Ok(jobs) = jobs else {
                    return;
                };
                if jobs.is_empty() {
                    break;
                }
                let mut pending = FuturesUnordered::new();
                for Job {
                    id,
                    token,
                    sql,
                    variables,
                } in jobs
                {
                    let (sql, params, applied) = match params::substitute(&sql, &variables, engine)
                    {
                        Ok((sql, params)) => {
                            let limited =
                                limit.and_then(|rows| statement::with_limit(&sql, rows, engine));
                            let applied = limit.filter(|_| limited.is_some());
                            (limited.unwrap_or(sql), params, applied)
                        }
                        Err(error) => {
                            this.update_in(cx, |view, _, cx| {
                                if view.token_current(id, token) {
                                    view.in_flight.remove(&id);
                                    view.set_widget_error(id, format!("{error:#}"), cx);
                                }
                            })
                            .ok();
                            continue;
                        }
                    };
                    let connection = connection.clone();
                    let task =
                        runtime::spawn(
                            async move { connection.run_query_with(&sql, params).await },
                        );
                    // A drop between the spawn and this store must not hand
                    // the stale handle to the next run.
                    this.update_in(cx, |view, _, _| {
                        if view.token_current(id, token) {
                            // Under test there is no Tokio task behind the
                            // run, so there is no handle to store.
                            if let Some(handle) = task.abort_handle() {
                                view.running.insert(id, handle);
                            }
                        }
                    })
                    .ok();
                    pending.push(async move {
                        let result = task.await;
                        (id, token, applied, result)
                    });
                }
                while let Some((id, token, applied, result)) = pending.next().await {
                    let landed = this.update_in(cx, |view, _, cx| {
                        if !view.token_current(id, token) {
                            // Dropped while it ran: whoever dropped it
                            // already reset the cell, and the rows are for
                            // SQL the widget may no longer run.
                            return;
                        }
                        view.in_flight.remove(&id);
                        view.running.remove(&id);
                        match result {
                            Ok(Ok(result)) => view.set_widget_result(id, result, applied, cx),
                            Ok(Err(error)) => view.set_widget_error(id, format!("{error:#}"), cx),
                            Err(_) => view.set_widget_error(id, "Cancelled".to_string(), cx),
                        }
                    });
                    if landed.is_err() {
                        return;
                    }
                }
            }
            this.update_in(cx, |view, _, cx| {
                if view.epoch == epoch {
                    view.refreshing = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run one widget after an edit, with its own confirmation when it
    /// writes. It queues behind whatever is running rather than stopping it.
    fn refresh_one(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(widget) = self.dashboard.widgets.get(index) else {
            return;
        };
        let id = self.widgets[index].id;
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
                            view.update(cx, |view, cx| view.start_refresh(vec![id], cx));
                        }
                        true
                    })
            });
            return;
        }
        self.start_refresh(vec![id], cx);
    }

    fn set_widget_result(
        &mut self,
        id: u64,
        result: QueryResult,
        applied: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(widget) = self.widgets.iter_mut().find(|widget| widget.id == id) else {
            return;
        };
        widget.status = WidgetStatus::Done;
        widget.applied_limit = applied;
        let grid = widget.grid.clone();
        grid.update(cx, |grid, cx| grid.set_result(result.clone(), cx));
        widget.result = Some(result);
        cx.notify();
    }

    fn set_widget_error(&mut self, id: u64, message: String, cx: &mut Context<Self>) {
        let Some(widget) = self.widgets.iter_mut().find(|widget| widget.id == id) else {
            return;
        };
        widget.reset(cx);
        widget.status = WidgetStatus::Error(message);
        cx.notify();
    }

    /// The dashboard's variables, pre-filled with the stored values: one
    /// value per name, shared by every widget that references it.
    pub(crate) fn open_variables(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.variables_dialog.is_some() {
            return;
        }
        let variables = self.variables();
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
                // A refresh in flight holds the old snapshot, and
                // `refresh` below returns early while one runs — so cancel
                // first, the way an edit does, or the new values would sit
                // unrun until the next manual refresh.
                if self.refreshing {
                    self.cancel_refresh(cx);
                }
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
        let id = self.next_id;
        self.next_id += 1;
        self.widgets.push(WidgetState::new(id, engine, window, cx));
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
        // The old rows belong to the old SQL; if the re-run is declined
        // they must not stay on screen under the new one.
        // Whatever it had queued or in flight was for the old SQL, and if
        // the re-run is declined it must not run unasked.
        let id = self.widgets[index].id;
        self.withdraw(id);
        self.widgets[index].reset(cx);
        dashboards::upsert(&self.dashboard, cx);
        cx.notify();
        self.refresh_one(index, window, cx);
    }

    /// Ask first, since the widget's SQL and picks go with it.
    fn confirm_delete_widget(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(widget) = self.dashboard.widgets.get(index) else {
            return;
        };
        if window.has_active_dialog(cx) {
            return;
        }
        let title = widget_title(widget, index);
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("Delete this widget?")
                .description(format!("“{title}” will be removed from the dashboard."))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    if let Some(view) = view.upgrade() {
                        view.update(cx, |view, cx| view.delete_widget(index, cx));
                    }
                    true
                })
        });
    }

    /// Swap the widget with its neighbour, `later` or earlier in the list.
    /// Results travel with their widgets, and a run in flight is left alone,
    /// since the queue addresses widgets by id.
    fn move_widget(&mut self, index: usize, later: bool, cx: &mut Context<Self>) {
        let other = if later {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(other) => other,
                None => return,
            }
        };
        if other >= self.dashboard.widgets.len() {
            return;
        }
        self.dashboard.widgets.swap(index, other);
        self.widgets.swap(index, other);
        dashboards::upsert(&self.dashboard, cx);
        cx.notify();
    }

    fn delete_widget(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.dashboard.widgets.len() {
            return;
        }
        self.withdraw(self.widgets[index].id);
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

/// A widget's title: the name the user gave it, else its number and the
/// SQL's first line.
fn widget_title(widget: &Widget, index: usize) -> String {
    match widget.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!(
            "Widget {} · {}",
            index + 1,
            widget_header_summary(&widget.sql)
        ),
    }
}

/// Which widgets share a row. A full-width widget takes a whole row; half-
/// width ones pair up two to a row, in list order, and a full-width one ends
/// the pair in progress.
fn layout_rows(widgets: &[Widget]) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut pending: Vec<usize> = Vec::new();
    for (index, widget) in widgets.iter().enumerate() {
        if widget.effective_width() == WidgetWidth::Full {
            rows.extend((!pending.is_empty()).then(|| std::mem::take(&mut pending)));
            rows.push(vec![index]);
        } else {
            pending.push(index);
            if pending.len() == 2 {
                rows.push(std::mem::take(&mut pending));
            }
        }
    }
    rows.extend((!pending.is_empty()).then_some(pending));
    rows
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
    let WidgetDisplay::Chart { kind, x, y, group } = display else {
        return WidgetChart::NotChartable;
    };
    if !chartable(result) {
        return WidgetChart::NotChartable;
    }
    let names = resolve_names(result, x.as_deref(), y);
    let group_index = group.as_deref().map(|name| {
        result
            .columns
            .iter()
            .position(|column| column == name)
            .ok_or(name)
    });
    let (x, ys) = match (names, group_index) {
        (Ok(resolved), None | Some(Ok(_))) => resolved,
        (names, group_index) => {
            let mut missing = names.err().unwrap_or_default();
            if let Some(Err(name)) = group_index {
                missing.push(name.to_string());
            }
            return WidgetChart::Missing(missing);
        }
    };
    let config = ChartConfig {
        kind: *kind,
        x,
        y: ys,
        group: group_index.and_then(Result::ok),
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
            .child(div().text_lg().child(self.dashboard.name.clone()))
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
                            .loading(refreshing)
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
        let rows = layout_rows(&self.dashboard.widgets);

        // The dashboard scrolls under its header, so widgets past the
        // window's height are reachable.
        v_flex()
            .id("dashboard-widgets")
            .flex_1()
            .min_h_0()
            .gap_4()
            .overflow_y_scroll()
            .children(rows.into_iter().map(|row| {
                let alone = row.len() == 1;
                let wide =
                    alone && self.dashboard.widgets[row[0]].effective_width() == WidgetWidth::Full;
                let mut cells: Vec<AnyElement> = row
                    .into_iter()
                    .map(|index| self.render_widget(index, cx))
                    .collect();
                if alone && !wide {
                    // A lone half-width widget still takes half the row,
                    // so it keeps one width instead of stretching.
                    cells.push(div().flex_1().min_w_0().into_any_element());
                }
                h_flex()
                    .flex_none()
                    .gap_4()
                    .children(cells)
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_widget(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let widget = &self.dashboard.widgets[index];
        let state = &self.widgets[index];
        let title = widget_title(widget, index);
        let count = self.widgets.len();
        let body = match &state.status {
            // The placeholder is shaped like what will replace it: bars for
            // a chart, header and rows for a grid. A queued widget gets the
            // same shape as a running one, with a still label in place of
            // the busy one.
            WidgetStatus::Queued | WidgetStatus::Running => {
                let label = if matches!(state.status, WidgetStatus::Queued) {
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("Waiting for another widget…")
                        .into_any_element()
                } else {
                    busy_label("Running…", cx.theme().muted_foreground).into_any_element()
                };
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_3()
                    .child(label)
                    .child(div().flex_1().min_h_0().child(match &widget.display {
                        WidgetDisplay::Grid => skeleton_grid(8, 4).into_any_element(),
                        WidgetDisplay::Chart { .. } => skeleton_chart().into_any_element(),
                    }))
                    .into_any_element()
            }
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
                    .min_w_0()
                    .w_full()
                    .child(state.grid.clone())
                    .into_any_element(),
                WidgetDisplay::Chart { .. } => self.render_widget_chart(index, cx),
            },
        };
        // `min_w_0` and `overflow_hidden` keep a wide result inside its cell,
        // where the grid's own scrollbars take over, rather than stretching
        // the cell past the window.
        div()
            .flex_1()
            .min_w_0()
            .h(px(320.))
            .overflow_hidden()
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
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(title.clone()),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new(("dashboard-widget-earlier", index))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::ChevronUp)
                                            .disabled(index == 0)
                                            .tooltip("Move earlier")
                                            .accessibility_label(format!("Move {title} earlier"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.move_widget(index, false, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("dashboard-widget-later", index))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::ChevronDown)
                                            .disabled(index + 1 >= count)
                                            .tooltip("Move later")
                                            .accessibility_label(format!("Move {title} later"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.move_widget(index, true, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("dashboard-widget-edit", index))
                                            .ghost()
                                            .xsmall()
                                            .label("Edit")
                                            .accessibility_label(format!("Edit {title}"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_widget_dialog(Some(index), window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("dashboard-widget-delete", index))
                                            .ghost()
                                            .xsmall()
                                            .label("Delete")
                                            .accessibility_label(format!("Delete {title}"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.confirm_delete_widget(index, window, cx)
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
                // Only a result that reached the limit was cut short by it; a
                // smaller one is whole, so it needs no note.
                let cut_short = state.applied_limit.filter(|&limit| {
                    state
                        .result
                        .as_ref()
                        .is_some_and(|result| result.rows.len() >= limit)
                });
                if let Some(limit) = cut_short {
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
    name: Entity<InputState>,
    editor: Entity<QueryEditor>,
    chart: bool,
    width: WidgetWidth,
    kind: ChartKind,
    x: Entity<InputState>,
    y: Entity<InputState>,
    group: Entity<InputState>,
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
            let (chart, kind, x_value, y_value, group_value) =
                match widget.as_ref().map(|w| &w.display) {
                    Some(WidgetDisplay::Chart { kind, x, y, group }) => (
                        true,
                        *kind,
                        x.clone().unwrap_or_default(),
                        y.join(", "),
                        group.clone().unwrap_or_default(),
                    ),
                    _ => (
                        false,
                        ChartKind::Line,
                        String::new(),
                        String::new(),
                        String::new(),
                    ),
                };
            let width = widget
                .as_ref()
                .map_or(WidgetWidth::Full, Widget::effective_width);
            let sql = widget.as_ref().map(|w| w.sql.clone()).unwrap_or_default();
            let editor = cx.new(|cx| {
                QueryEditor::with_text(sql, engine, catalog, window, cx).without_toolbar()
            });
            cx.subscribe_in(&editor, window, Self::on_editor_event)
                .detach();
            let name_value = widget
                .as_ref()
                .and_then(|w| w.name.clone())
                .unwrap_or_default();
            let name = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(name_value, window, cx);
                state.set_placeholder("Name (optional)", window, cx);
                state
            });
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
            let group = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(group_value, window, cx);
                state.set_placeholder("group column", window, cx);
                state
            });
            Self {
                index,
                name,
                editor,
                chart,
                width,
                kind,
                x,
                y,
                group,
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
                .w(px(760.))
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
                                .label("Save and run")
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
                group: {
                    let name = self.group.read(cx).value().trim().to_string();
                    (self.kind == ChartKind::StackedBar && !name.is_empty()).then_some(name)
                },
            }
        } else {
            WidgetDisplay::Grid
        };
        let name = self.name.read(cx).value().trim().to_string();
        Widget {
            name: (!name.is_empty()).then_some(name),
            width: Some(self.width),
            sql,
            display,
        }
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

/// A labelled form field: the label above its control, and help beneath
/// when the control alone does not say what to type.
fn field(
    label: &'static str,
    help: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
        .child(control)
        .when_some(help, |this, help| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(help),
            )
        })
}

/// A small visible set of choices on one line.
fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    selected: T,
    on_change: impl Fn(T, &mut Window, &mut App) + 'static,
) -> RadioGroup {
    let values: Vec<T> = options.iter().map(|(value, _)| *value).collect();
    RadioGroup::horizontal(id)
        .selected_index(values.iter().position(|value| *value == selected))
        .on_change(move |index, window, cx| {
            if let Some(value) = values.get(*index) {
                on_change(*value, window, cx);
            }
        })
        .children(options.iter().enumerate().map(|(index, (_, label))| {
            Radio::new(SharedString::from(format!("{id}-{index}")))
                .label(SharedString::from(*label))
        }))
}

impl Render for WidgetEditDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chart = self.chart;
        let view = cx.entity().downgrade();
        let show = {
            let view = view.clone();
            move |chart: bool, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.chart = chart;
                    cx.notify();
                })
                .ok();
            }
        };
        let size = {
            let view = view.clone();
            move |width: WidgetWidth, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.width = width;
                    cx.notify();
                })
                .ok();
            }
        };
        let kind = move |kind: ChartKind, _: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| {
                this.kind = kind;
                cx.notify();
            })
            .ok();
        };

        v_flex()
            .id("widget-dialog")
            .size_full()
            .gap_4()
            .track_focus(&self.focus)
            .key_context("WidgetDialog")
            .on_action(cx.listener(Self::on_close))
            .child(field(
                "Name",
                None,
                Input::new(&self.name)
                    .id("widget-name")
                    .aria_label("Widget name")
                    .small(),
                cx,
            ))
            .child(field(
                "Query",
                Some("Runs when the dashboard refreshes. Use :name for dashboard variables."),
                div()
                    .h(px(240.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .overflow_hidden()
                    .child(self.editor.clone()),
                cx,
            ))
            .child(
                h_flex()
                    .gap_8()
                    .child(field(
                        "Show as",
                        None,
                        choice(
                            "widget-display",
                            &[(true, "Chart"), (false, "Data grid")],
                            chart,
                            show,
                        ),
                        cx,
                    ))
                    .child(field(
                        "Width",
                        None,
                        choice(
                            "widget-width",
                            &[(WidgetWidth::Half, "Half"), (WidgetWidth::Full, "Full")],
                            self.width,
                            size,
                        ),
                        cx,
                    )),
            )
            .when(chart, |this| {
                this.child(
                    v_flex()
                        .gap_4()
                        .p_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted.opacity(0.4))
                        .child(field(
                            "Chart type",
                            None,
                            choice(
                                "widget-chart-kind",
                                &[
                                    (ChartKind::Line, "Line"),
                                    (ChartKind::Bar, "Bar"),
                                    (ChartKind::Area, "Area"),
                                    (ChartKind::StackedBar, "Stacked bar"),
                                ],
                                self.kind,
                                kind,
                            ),
                            cx,
                        ))
                        .child(
                            h_flex()
                                .gap_4()
                                .items_start()
                                .child(
                                    div().w(px(220.)).child(field(
                                        "X column",
                                        Some("Optional; the first column by default."),
                                        Input::new(&self.x)
                                            .id("widget-x")
                                            .aria_label("X column")
                                            .small(),
                                        cx,
                                    )),
                                )
                                .child(
                                    div().flex_1().child(field(
                                        "Y columns",
                                        Some("Comma-separated; the numeric columns by default."),
                                        Input::new(&self.y)
                                            .id("widget-y")
                                            .aria_label("Y columns")
                                            .small(),
                                        cx,
                                    )),
                                ),
                        )
                        .when(self.kind == ChartKind::StackedBar, |this| {
                            this.child(field(
                                "Group column",
                                Some("Splits the first Y column into one segment per value."),
                                Input::new(&self.group)
                                    .id("widget-group")
                                    .aria_label("Group column")
                                    .small(),
                                cx,
                            ))
                        }),
                )
            })
    }
}

#[cfg(test)]
pub(crate) struct WidgetSummary {
    pub(crate) title: String,
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
                    WidgetStatus::Queued => "queued".to_string(),
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
                                        ChartKind::StackedBar => "StackedBar",
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
                    title: widget_title(&self.dashboard.widgets[index], index),
                    status,
                    chart,
                    chart_missing,
                    rows: state.result.as_ref().map(|result| result.rows.len()),
                    applied_limit: state.applied_limit,
                }
            })
            .collect()
    }

    pub(crate) fn move_widget_for_test(
        &mut self,
        index: usize,
        later: bool,
        cx: &mut Context<Self>,
    ) {
        self.move_widget(index, later, cx);
    }

    /// Start a full refresh and, before it has run anything, replace widget
    /// `index`: the state an edit meets when other widgets are still loading.
    pub(crate) fn edit_during_refresh_for_test(
        &mut self,
        index: usize,
        widget: Widget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh(window, cx);
        self.replace_widget(index, widget, window, cx);
    }

    pub(crate) fn variables_dialog_for_test(&self) -> Option<Entity<VariablesView>> {
        self.variables_dialog.clone()
    }

    /// Pretend a refresh is in flight, for tests: the state a save
    /// from the variables dialog meets when a refresh is still running.
    pub(crate) fn mark_refreshing_for_test(&mut self) {
        self.refreshing = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(name: Option<&str>, display: WidgetDisplay) -> Widget {
        Widget {
            name: name.map(str::to_string),
            width: None,
            sql: "\n  select 1\nfrom t".to_string(),
            display,
        }
    }

    fn chart() -> WidgetDisplay {
        WidgetDisplay::Chart {
            kind: ChartKind::Line,
            x: None,
            y: Vec::new(),
            group: None,
        }
    }

    #[test]
    fn a_name_is_the_title_and_a_blank_one_falls_back() {
        assert_eq!(
            widget_title(&widget(Some(" Revenue "), chart()), 0),
            "Revenue"
        );
        assert_eq!(
            widget_title(&widget(Some("  "), chart()), 1),
            "Widget 2 · select 1"
        );
        assert_eq!(
            widget_title(&widget(None, WidgetDisplay::Grid), 0),
            "Widget 1 · select 1"
        );
    }

    #[test]
    fn a_chosen_width_overrides_the_display_default() {
        let mut wide_chart = widget(None, chart());
        wide_chart.width = Some(WidgetWidth::Full);
        let mut narrow_grid = widget(None, WidgetDisplay::Grid);
        narrow_grid.width = Some(WidgetWidth::Half);
        let widgets = [
            narrow_grid.clone(),
            narrow_grid,
            wide_chart,
            widget(None, chart()),
        ];
        assert_eq!(layout_rows(&widgets), vec![vec![0, 1], vec![2], vec![3]]);
    }

    #[test]
    fn grids_take_a_row_and_charts_pair_up() {
        let widgets = [
            widget(None, chart()),
            widget(None, WidgetDisplay::Grid),
            widget(None, chart()),
            widget(None, chart()),
            widget(None, chart()),
            widget(None, WidgetDisplay::Grid),
        ];
        assert_eq!(
            layout_rows(&widgets),
            vec![vec![0], vec![1], vec![2, 3], vec![4], vec![5]]
        );
        assert!(layout_rows(&[]).is_empty());
    }
}
