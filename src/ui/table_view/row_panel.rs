//! The side panel beside a table's grid: the row the keyboard is on, laid out
//! as one field per column.
//!
//! A wide row is awkward to edit a cell at a time, so the panel follows the
//! grid's focused row and stages whatever is typed into the same overlay a grid
//! edit uses — the grid tints it, and the ordinary apply flow writes it. The
//! panel never writes anything itself.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, Focusable, Window, div};
use regex::Regex;

use crate::db::query::{self, Cell};
use crate::settings::Settings;
use crate::ui::data_grid::{DataGrid, GridEdit, RowStatus, needs_a_window};
use crate::ui::text_filter;

pub(crate) enum RowPanelEvent {
    CloseRequested,
}

impl EventEmitter<RowPanelEvent> for RowPanel {}

pub(crate) struct RowPanel {
    grid: Entity<DataGrid>,
    columns: Vec<String>,
    column_types: Vec<String>,
    focused_row: Option<usize>,
    /// One text box per column, index-aligned with `columns`.
    fields: Vec<Entity<InputState>>,
    filter_input: Entity<InputState>,
    filter: Option<Regex>,
}

impl RowPanel {
    pub(crate) fn new(grid: Entity<DataGrid>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter columns"));
        cx.subscribe_in(&filter_input, window, Self::on_filter_event)
            .detach();
        cx.subscribe_in(&grid, window, Self::on_grid_edit).detach();

        let focused_row = grid.read(cx).focused_row(cx);
        Self {
            grid,
            columns: Vec::new(),
            column_types: Vec::new(),
            focused_row,
            fields: Vec::new(),
            filter_input,
            filter: None,
        }
    }

    /// Rebuild one field per column, as when a page brings a different set.
    pub(crate) fn sync_columns(
        &mut self,
        columns: &[String],
        column_types: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.columns = columns.to_vec();
        self.column_types = column_types.to_vec();
        self.fields = (0..columns.len())
            .map(|col_ix| {
                let field = cx.new(|cx| InputState::new(window, cx));
                cx.subscribe_in(
                    &field,
                    window,
                    move |this, _, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                            this.commit_field(col_ix, window, cx);
                        }
                    },
                )
                .detach();
                field
            })
            .collect();
        self.focused_row = self.grid.read(cx).focused_row(cx);
        self.reseed(true, window, cx);
    }

    fn on_filter_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let pattern = input.read(cx).value().trim().to_string();
        self.filter = text_filter::compile(&pattern);
        cx.notify();
    }

    fn on_grid_edit(
        &mut self,
        _: &Entity<DataGrid>,
        event: &GridEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GridEdit::RowFocused(row) => {
                let moved = self.focused_row != *row;
                self.focused_row = *row;
                self.reseed(moved, window, cx);
            }
            GridEdit::Staged => self.reseed(false, window, cx),
            GridEdit::RowLeft { .. } => {}
        }
    }

    /// Put the focused row's values in the fields.
    ///
    /// A field being typed into keeps its text unless the row itself changed:
    /// another field's edit event would otherwise stomp it mid-keystroke.
    fn reseed(&mut self, row_changed: bool, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let Some(row_ix) = self.focused_row else {
            return;
        };
        let Some(cells) = self.grid.read(cx).row_cells(row_ix, cx) else {
            return;
        };

        for (field, cell) in self.fields.iter().zip(cells) {
            if !row_changed && field.read(cx).focus_handle(cx).is_focused(window) {
                continue;
            }
            // Raw text, the way the grid's own editor opens: a NULL is empty,
            // not the word.
            field.update(cx, |input, cx| {
                input.set_value(cell.unwrap_or_default(), window, cx)
            });
        }
    }

    /// Stage what a field holds, the way the grid does when its editor closes.
    fn commit_field(&mut self, col_ix: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(row_ix) = self.focused_row else {
            return;
        };
        let Some(field) = self.fields.get(col_ix) else {
            return;
        };
        let Some(cells) = self.grid.read(cx).row_cells(row_ix, cx) else {
            return;
        };

        let text = field.read(cx).value().to_string();
        // A box that shows what the cell already holds has not been edited;
        // staging it would turn a NULL, shown empty, into an empty string.
        let current = cells.get(col_ix).cloned().flatten().unwrap_or_default();
        if text == current {
            return;
        }

        let coerce = Settings::global(cx).coerce_null_literal;
        let value: Cell = if coerce && text.eq_ignore_ascii_case("null") {
            None
        } else {
            Some(text)
        };
        self.grid
            .update(cx, |grid, cx| grid.stage_cell(row_ix, col_ix, value, cx));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .p_2()
            .items_center()
            .child(
                div().flex_1().child(
                    Input::new(&self.filter_input)
                        .id("row-panel-filter")
                        .small()
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("row-panel-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .accessibility_label("Hide row panel")
                    .tooltip("Hide row panel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(RowPanelEvent::CloseRequested))),
            )
    }

    fn render_field(
        &self,
        row_ix: usize,
        col_ix: usize,
        status: RowStatus,
        cells: &[Cell],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let column = self.columns[col_ix].clone();
        let type_name = self.column_types.get(col_ix).cloned().unwrap_or_default();
        let cell = cells.get(col_ix).cloned().flatten();
        let deleted = status == RowStatus::Deleted;
        let editable = !deleted && self.grid.read(cx).is_field_editable(row_ix, col_ix, cx);

        let note = if deleted {
            None
        } else if query::is_placeholder(&cell) {
            Some("This value was not read back from the server.")
        } else if !editable {
            Some("This value cannot be edited here.")
        } else {
            None
        };
        let big = needs_a_window(&cell, &type_name);
        let grid = self.grid.clone();
        let null_grid = self.grid.clone();

        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .when(deleted, |this| this.line_through())
                            .child(format!("{column} · {type_name}")),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .when(big, |this| {
                                this.child(
                                    Button::new(("row-panel-expand", col_ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Eye)
                                        .accessibility_label(format!("View {column}"))
                                        .tooltip("View the whole value")
                                        .on_click(move |_, _, cx| {
                                            grid.update(cx, |grid, cx| {
                                                grid.view_cell(row_ix, col_ix, cx)
                                            })
                                        }),
                                )
                            })
                            .when(editable, |this| {
                                this.child(
                                    Checkbox::new(("row-panel-null", col_ix))
                                        .accessibility_label(format!("Set {column} to NULL"))
                                        .label("NULL")
                                        .checked(cell.is_none())
                                        .on_click(move |checked: &bool, _, cx| {
                                            let value: Cell =
                                                if *checked { None } else { Some(String::new()) };
                                            null_grid.update(cx, |grid, cx| {
                                                grid.stage_cell(row_ix, col_ix, value, cx)
                                            })
                                        }),
                                )
                            }),
                    ),
            )
            .child(Input::new(&self.fields[col_ix]).small().readonly(!editable))
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(note),
                )
            })
    }
}

impl Render for RowPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.focused_row.and_then(|row_ix| {
            self.grid
                .read(cx)
                .row_status(row_ix, cx)
                .map(|s| (row_ix, s))
        });

        let body = match status {
            None => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select a row to see its fields"),
                )
                .into_any_element(),
            Some((row_ix, status)) => {
                let cells = self.grid.read(cx).row_cells(row_ix, cx).unwrap_or_default();
                let shown: Vec<usize> = (0..self.columns.len().min(self.fields.len()))
                    .filter(|col_ix| match &self.filter {
                        Some(filter) => filter.is_match(&self.columns[*col_ix]),
                        None => true,
                    })
                    .collect();

                let none_shown = shown.is_empty();
                let fields: Vec<gpui_kit::AnyElement> = shown
                    .into_iter()
                    .map(|col_ix| {
                        self.render_field(row_ix, col_ix, status, &cells, cx)
                            .into_any_element()
                    })
                    .collect();

                v_flex()
                    .id("row-panel-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .p_2()
                    .when(status == RowStatus::Deleted, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child("Row marked for deletion"),
                        )
                    })
                    .when(status == RowStatus::Draft, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("New row"),
                        )
                    })
                    .when(none_shown, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No columns match"),
                        )
                    })
                    .children(fields)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(self.render_header(cx))
            .child(body)
    }
}

#[cfg(test)]
impl RowPanel {
    pub(crate) fn focused_row_for_test(&self) -> Option<usize> {
        self.focused_row
    }

    pub(crate) fn field_value_for_test(&self, col_ix: usize, cx: &gpui_kit::App) -> String {
        self.fields[col_ix].read(cx).value().to_string()
    }

    pub(crate) fn set_field_for_test(
        &mut self,
        col_ix: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields[col_ix].update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn commit_field_for_test(
        &mut self,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_field(col_ix, window, cx);
    }

    pub(crate) fn set_filter_for_test(
        &mut self,
        pattern: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // `set_value` is silent, unlike typing, so the matcher is set directly.
        self.filter_input.update(cx, |input, cx| {
            input.set_value(pattern.to_string(), window, cx)
        });
        self.filter = text_filter::compile(pattern.trim());
        cx.notify();
    }

    /// The columns the filter lets through.
    pub(crate) fn shown_columns_for_test(&self) -> Vec<String> {
        self.columns
            .iter()
            .filter(|name| self.filter.as_ref().is_none_or(|f| f.is_match(name)))
            .cloned()
            .collect()
    }
}
