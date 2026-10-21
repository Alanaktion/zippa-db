//! A query tab's implicit row limit: the `LIMIT` a lone `SELECT` without one
//! is sent with, and the status bar's control for it — a "Limit" checkbox
//! and a row count, the way a table view's footer has a limit box.
//!
//! The settings decide both halves until the tab says otherwise: ticking
//! the box or typing a count makes it the tab's own, kept with the tab in
//! `workspace.json`, while a tab that never touched it follows a later
//! change to the settings.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{
    InputEvent, InputState, NumberInput, NumberInputEvent, StepAction,
};
use gpui_kit::component::{Disableable, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window, div, px};

use crate::settings::{self, Settings};

use super::SessionPanel;
use super::tab::TabContent;

pub(crate) struct QueryLimit {
    input: Entity<InputState>,
    /// The tab's own answer to whether to limit; `None` follows the settings.
    enabled: Option<bool>,
    /// The tab's own row count; `None` follows the settings.
    rows: Option<usize>,
    /// The limit the last run was sent with, so its summary can say the
    /// rows stopped there rather than at the end of the table.
    pub(super) applied: Option<usize>,
}

impl QueryLimit {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SessionPanel>) -> Self {
        let rows = Settings::global(cx)
            .implicit_limit_rows
            .clamp(1, settings::MAX_PAGE_SIZE);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(rows.to_string())
                .min(1.)
        });
        cx.subscribe_in(&input, window, SessionPanel::on_limit_input)
            .detach();
        // Stepped by hand rather than by the state's own `step`, the way a
        // table view's limit box is, so a click moves from the count in
        // force rather than from whatever half-typed text is in the box.
        cx.subscribe_in(&input, window, SessionPanel::on_limit_step)
            .detach();
        // A tab still following the settings shows a change to them.
        cx.observe_global_in::<Settings>(window, |panel, window, cx| {
            panel.follow_limit_setting(window, cx)
        })
        .detach();

        Self {
            input,
            enabled: None,
            rows: None,
            applied: None,
        }
    }

    fn enabled(&self, cx: &App) -> bool {
        self.enabled.unwrap_or(Settings::global(cx).implicit_limit)
    }

    fn rows(&self, cx: &App) -> usize {
        self.rows
            .unwrap_or(Settings::global(cx).implicit_limit_rows)
            .clamp(1, settings::MAX_PAGE_SIZE)
    }

    /// The tab's own choices, for `workspace.json`.
    pub(super) fn overrides(&self) -> (Option<bool>, Option<usize>) {
        (self.enabled, self.rows)
    }
}

impl SessionPanel {
    fn limit(&self) -> Option<&QueryLimit> {
        match &self.content {
            TabContent::Query { limit, .. } => Some(limit),
            _ => None,
        }
    }

    fn limit_mut(&mut self) -> Option<&mut QueryLimit> {
        match &mut self.content {
            TabContent::Query { limit, .. } => Some(limit),
            _ => None,
        }
    }

    /// The rows a lone `SELECT` run from this tab is limited to, or `None`
    /// when the tab — or the settings it follows — says not to limit.
    pub(crate) fn implicit_limit(&self, cx: &App) -> Option<usize> {
        let limit = self.limit()?;
        limit.enabled(cx).then(|| limit.rows(cx))
    }

    /// Record the limit the run about to start goes out with.
    pub(crate) fn set_applied_limit(&mut self, rows: Option<usize>) {
        if let Some(limit) = self.limit_mut() {
            limit.applied = rows;
        }
    }

    /// Put back the choices a restored tab had made for itself.
    pub(crate) fn restore_limit(
        &mut self,
        enabled: Option<bool>,
        rows: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(limit) = self.limit_mut() else {
            return;
        };
        limit.enabled = enabled;
        limit.rows = rows.map(|rows| rows.clamp(1, settings::MAX_PAGE_SIZE));
        let shown = limit.rows(cx);
        let input = limit.input.clone();
        input.update(cx, |input, cx| {
            input.set_value(shown.to_string(), window, cx)
        });
        cx.notify();
    }

    fn toggle_limit(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(limit) = self.limit_mut() {
            limit.enabled = Some(enabled);
            cx.notify();
        }
    }

    /// Typing a count makes it the tab's own as it is typed — nothing runs on
    /// a keystroke here, so there is no reason to wait for `Enter` the way a
    /// table view does. Text that is not a count is put back on blur.
    fn on_limit_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let setting = Settings::global(cx).implicit_limit_rows;
        let typed = input.read(cx).value().trim().parse::<usize>().ok();
        let Some(limit) = self.limit_mut() else {
            return;
        };
        match event {
            InputEvent::Change => {
                let Some(rows) = typed.filter(|rows| *rows > 0) else {
                    return;
                };
                let rows = rows.min(settings::MAX_PAGE_SIZE);
                // The settings' own count, shown in a tab that follows them,
                // is not a choice the tab made.
                if limit.rows.is_some() || rows != setting {
                    limit.rows = Some(rows);
                }
            }
            InputEvent::Blur => {
                let shown = limit.rows(cx);
                if typed != Some(shown) {
                    input.update(cx, |input, cx| {
                        input.set_value(shown.to_string(), window, cx)
                    });
                }
            }
            _ => {}
        }
    }

    fn on_limit_step(
        &mut self,
        input: &Entity<InputState>,
        event: &NumberInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(limit) = self.limit_mut() else {
            return;
        };
        let NumberInputEvent::Step(action) = event;
        let current = limit.rows(cx);
        let step = if current < 50 {
            1
        } else if current < 1000 {
            10
        } else {
            100
        };
        let rows = match action {
            StepAction::Increment => current.saturating_add(step),
            StepAction::Decrement => current.saturating_sub(step),
        }
        .clamp(1, settings::MAX_PAGE_SIZE);
        limit.rows = Some(rows);
        input.update(cx, |input, cx| {
            input.set_value(rows.to_string(), window, cx)
        });
        cx.notify();
    }

    /// The settings changed: a tab that has no count of its own shows theirs.
    fn follow_limit_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(limit) = self.limit() else {
            return;
        };
        if limit.rows.is_some() {
            return;
        }
        let rows = limit.rows(cx).to_string();
        let input = limit.input.clone();
        if input.read(cx).value() != rows {
            input.update(cx, |input, cx| input.set_value(rows, window, cx));
        }
        cx.notify();
    }

    /// The checkbox and the count, at the head of the status bar.
    pub(super) fn render_limit(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let limit = self.limit()?;
        let enabled = limit.enabled(cx);
        Some(
            h_flex()
                .id("implicit-limit")
                .flex_none()
                .gap_2()
                .child(
                    Checkbox::new("implicit-limit-enabled")
                        .label("Limit")
                        .xsmall()
                        .checked(enabled)
                        .on_click(cx.listener(|panel, checked: &bool, _window, cx| {
                            panel.toggle_limit(*checked, cx)
                        })),
                )
                .child(
                    div()
                        .w(px(88.))
                        .child(NumberInput::new(&limit.input).xsmall().disabled(!enabled)),
                )
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(
                        "Adds a LIMIT to a single SELECT that has none of its own",
                    )
                    .build(window, cx)
                }),
        )
    }

    /// What a run's summary adds when its rows stopped at the limit.
    pub(super) fn limit_note(&self, rows: usize) -> Option<String> {
        let applied = self.limit()?.applied?;
        (rows >= applied).then(|| format!("limited to {applied}"))
    }
}
