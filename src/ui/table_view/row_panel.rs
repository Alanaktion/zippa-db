//! The side panel beside a table's grid: the row the keyboard is on, laid out
//! as one field per column.
//!
//! A wide row is awkward to edit a cell at a time, so the panel follows the
//! grid's focused row and stages whatever is typed into the same overlay a grid
//! edit uses — the grid tints it, and the ordinary apply flow writes it. The
//! panel never writes anything itself.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Focusable, Window, div, px};
use regex::Regex;

use crate::db::query::{self, Cell};
use crate::settings::Settings;
use crate::ui::data_grid::{DataGrid, GridEdit, RowStatus, needs_a_window};
use crate::ui::text_filter;

pub(crate) enum RowPanelEvent {
    CloseRequested,
}

impl EventEmitter<RowPanelEvent> for RowPanel {}

/// The most lines a text box grows to before it scrolls.
const TEXT_LINES: usize = 3;

/// Whether a column holds prose, which wants room to wrap rather than one line.
fn is_text_type(type_name: &str) -> bool {
    let name = type_name.trim_matches('"').to_ascii_uppercase();
    ["TEXT", "CHAR", "STRING", "CLOB", "JSON", "XML", "NAME"]
        .iter()
        .any(|kind| name.contains(kind))
}

/// One column's text box: a line for most types, a few lines for text.
enum Field {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Field {
    fn value(&self, cx: &App) -> String {
        match self {
            Field::Line(input) => input.read(cx).value().to_string(),
            Field::Area(input) => input.read(cx).value().to_string(),
        }
    }

    fn set_value(&self, text: String, window: &mut Window, cx: &mut App) {
        match self {
            Field::Line(input) => input.update(cx, |input, cx| input.set_value(text, window, cx)),
            Field::Area(input) => input.update(cx, |input, cx| input.set_value(text, window, cx)),
        }
    }

    fn is_focused(&self, window: &Window, cx: &App) -> bool {
        match self {
            Field::Line(input) => input.read(cx).focus_handle(cx).is_focused(window),
            Field::Area(input) => input.read(cx).focus_handle(cx).is_focused(window),
        }
    }
}

pub(crate) struct RowPanel {
    grid: Entity<DataGrid>,
    columns: Vec<String>,
    column_types: Vec<String>,
    focused_row: Option<usize>,
    /// One text box per column, index-aligned with `columns`.
    fields: Vec<Field>,
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
                let text = column_types.get(col_ix).is_some_and(|t| is_text_type(t));
                let on_event = move |this: &mut Self,
                                     event: &InputEvent,
                                     window: &mut Window,
                                     cx: &mut Context<Self>| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.commit_field(col_ix, window, cx);
                    }
                };
                if text {
                    let field =
                        cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, TEXT_LINES));
                    cx.subscribe_in(
                        &field,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            on_event(this, event, window, cx)
                        },
                    )
                    .detach();
                    Field::Area(field)
                } else {
                    let field = cx.new(|cx| InputState::new(window, cx));
                    cx.subscribe_in(
                        &field,
                        window,
                        move |this, _, event: &InputEvent, window, cx| {
                            on_event(this, event, window, cx)
                        },
                    )
                    .detach();
                    Field::Line(field)
                }
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
            if !row_changed && field.is_focused(window, cx) {
                continue;
            }
            // Raw text, the way the grid's own editor opens: a NULL is empty,
            // not the word.
            field.set_value(cell.unwrap_or_default(), window, cx);
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

        let text = field.value(cx);
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
        let staged = self.grid.read(cx).is_field_staged(row_ix, col_ix, cx);
        let draft = status == RowStatus::Draft;
        let grid = self.grid.clone();
        let menu_column = column.clone();

        let menu = Button::new(("row-panel-menu", col_ix))
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .accessibility_label(format!("Options for {menu_column}"))
            .tooltip("Field options")
            .dropdown_menu(move |menu, _window, _cx| {
                let stage = |label: &'static str, value: Cell| {
                    let grid = grid.clone();
                    PopupMenuItem::new(label)
                        .disabled(!editable)
                        .on_click(move |_, _, cx| {
                            let value = value.clone();
                            grid.update(cx, |grid, cx| grid.stage_cell(row_ix, col_ix, value, cx))
                        })
                };
                let reset = |label: &'static str, enabled: bool| {
                    let grid = grid.clone();
                    PopupMenuItem::new(label)
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| {
                            grid.update(cx, |grid, cx| grid.reset_cell(row_ix, col_ix, cx))
                        })
                };
                let view = grid.clone();
                menu.item(stage("Set NULL", None))
                    .item(stage("Set empty string", Some(String::new())))
                    // Only a new row has a default to fall back on: a loaded
                    // row's cell has no way to say "whatever the server picks".
                    .item(reset("Set Default", editable && draft && staged))
                    .item(reset("Revert change", editable && !draft && staged))
                    .item(PopupMenuItem::separator())
                    .item(
                        PopupMenuItem::new("View whole value")
                            .disabled(!big)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |grid, cx| grid.view_cell(row_ix, col_ix, cx))
                            }),
                    )
            });

        let input = match &self.fields[col_ix] {
            Field::Line(input) => Input::new(input)
                .xsmall()
                .readonly(!editable)
                .into_any_element(),
            Field::Area(input) => Textarea::new(input)
                .text_xs()
                .p_0() // TODO: actually apply xsmall()-like style, this padding override doesn't work.
                .readonly(!editable)
                .into_any_element(),
        };

        v_flex()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_1()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .when(deleted, |this| this.line_through())
                            .child(format!("{column} · {type_name}")),
                    )
                    .child(menu),
            )
            .child(input)
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .text_size(px(10.))
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
                    .gap_2()
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

    pub(crate) fn field_value_for_test(&self, col_ix: usize, cx: &App) -> String {
        self.fields[col_ix].value(cx)
    }

    pub(crate) fn set_field_for_test(
        &mut self,
        col_ix: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fields[col_ix].set_value(value.to_string(), window, cx);
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
