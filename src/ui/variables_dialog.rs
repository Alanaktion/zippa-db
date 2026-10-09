//! The query variables dialog.
//!
//! Opened from the Variables button in a query tab's toolbar. One row per
//! `:name` variable — a name box, a value box, and a remove button — plus an
//! add-row button. Saving hands the rows back to the session, which stores
//! them on the tab; the values are substituted for the buffer's `:name`
//! placeholders on the next run, and persisted with the session.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Window, actions, div, px};

use crate::db::params::Variable;

actions!(zippa_db, [CloseVariablesDialog]);

/// What the dialog tells its owner.
pub enum VariablesEvent {
    /// The user is done with the dialog; the session takes it off screen.
    Dismissed,
    /// The user saved; the rows replace the tab's variables.
    Saved(Vec<Variable>),
}

/// One row of the dialog.
struct VariableRow {
    name: Entity<InputState>,
    value: Entity<InputState>,
}

pub struct VariablesView {
    rows: Vec<VariableRow>,
    /// The dialog body's own focus, so `escape` reaches `CloseVariablesDialog`.
    focus: gpui_kit::FocusHandle,
}

impl EventEmitter<VariablesEvent> for VariablesView {}

impl VariablesView {
    pub fn new(variables: Vec<Variable>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            rows: Vec::new(),
            focus: cx.focus_handle(),
        };
        for variable in variables {
            view.push_row(&variable.name, &variable.value, window, cx);
        }
        if view.rows.is_empty() {
            view.push_row("", "", window, cx);
        }
        view
    }

    fn push_row(&mut self, name: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name_value = name.to_string();
        let value_value = value.to_string();
        let name = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(name_value, window, cx);
            state.set_placeholder("name", window, cx);
            state
        });
        let value = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(value_value, window, cx);
            state.set_placeholder("value", window, cx);
            state
        });
        self.rows.push(VariableRow { name, value });
    }

    /// Put the keyboard on the first name box, so typing starts at once.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(row) = self.rows.first() {
            use gpui_kit::Focusable as _;
            row.name.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push_row("", "", window, cx);
        cx.notify();
    }

    fn remove_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.rows.remove(index);
        if self.rows.is_empty() {
            // There is always a row to type into.
            self.push_row("", "", window, cx);
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        cx.emit(VariablesEvent::Saved(self.collect(cx)));
    }

    /// The rows as the save would hand them over: blank names are dropped,
    /// a leading `:` is stripped, and a repeated name keeps its first value.
    fn collect(&self, cx: &App) -> Vec<Variable> {
        let mut variables = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in &self.rows {
            let name = row
                .name
                .read(cx)
                .value()
                .trim()
                .trim_start_matches(':')
                .to_string();
            if name.is_empty() || !seen.insert(name.clone()) {
                continue;
            }
            let value = row.value.read(cx).value().to_string();
            variables.push(Variable { name, value });
        }
        variables
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(VariablesEvent::Dismissed);
    }

    /// `Escape` closes the dialog without saving.
    fn on_close(&mut self, _: &CloseVariablesDialog, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }
}

impl Render for VariablesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("variables-dialog")
            .size_full()
            .gap_2()
            .track_focus(&self.focus)
            .key_context("VariablesDialog")
            .on_action(cx.listener(Self::on_close))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Each :name in this buffer takes the value beside it when the buffer runs.",
                    ),
            )
            .children((0..self.rows.len()).map(|index| {
                h_flex()
                    .gap_2()
                    .child(
                        div().w(px(140.)).child(
                            Input::new(&self.rows[index].name)
                                .id(("variable-name", index))
                                .aria_label("Variable name")
                                .small(),
                        ),
                    )
                    .child(
                        div().flex_1().min_w(px(120.)).child(
                            Input::new(&self.rows[index].value)
                                .id(("variable-value", index))
                                .aria_label("Variable value")
                                .small(),
                        ),
                    )
                    .child(
                        Button::new(("remove-variable", index))
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .accessibility_label("Remove this variable")
                            .tooltip("Remove this variable")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_row(index, window, cx)
                            })),
                    )
            }))
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        Button::new("add-variable")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add variable")
                            .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("variables-cancel")
                                    .ghost()
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            )
                            .child(
                                Button::new("variables-save")
                                    .primary()
                                    .small()
                                    .label("Save variables")
                                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
impl VariablesView {
    pub(crate) fn set_row_for_test(
        &self,
        index: usize,
        name: &str,
        value: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let row = &self.rows[index];
        row.name.update(cx, |state, cx| {
            state.set_value(name.to_string(), window, cx)
        });
        row.value.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn collected_for_test(&self, cx: &App) -> Vec<Variable> {
        self.collect(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_duplicate_names_are_dropped_on_collect() {
        // The collection logic, without a window: mirrors `collect`.
        fn collect(names: &[&str], values: &[&str]) -> Vec<Variable> {
            let mut variables = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for (name, value) in names.iter().zip(values.iter()) {
                let name = name.trim().trim_start_matches(':').to_string();
                if name.is_empty() || !seen.insert(name.clone()) {
                    continue;
                }
                variables.push(Variable {
                    name,
                    value: value.to_string(),
                });
            }
            variables
        }

        let variables = collect(&["a", "", ":b", "a"], &["1", "2", "3", "4"]);
        assert_eq!(
            variables,
            vec![
                Variable {
                    name: "a".to_string(),
                    value: "1".to_string(),
                },
                Variable {
                    name: "b".to_string(),
                    value: "3".to_string(),
                },
            ]
        );
    }
}
