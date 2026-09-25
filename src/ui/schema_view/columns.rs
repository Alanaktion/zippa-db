//! The structure tab's column section: adding, dropping and retyping columns.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, SharedString, Window, div};

use super::layout::*;
use super::sql::{self};
use super::{EditableColumn, SchemaView};

impl SchemaView {
    /// Append a blank row for the user to fill in by hand.
    pub(super) fn add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let row = self.new_row(None, window, cx);
        self.columns.push(row);
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    /// Mark an existing row to be dropped, or take the mark back off; a row
    /// added by hand is removed outright instead, since the server has
    /// nothing of it to drop.
    pub(super) fn toggle_drop(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(index) = self.columns.iter().position(|column| column.id == id) else {
            return;
        };
        if self.columns[index].original.is_some() {
            self.columns[index].dropped = !self.columns[index].dropped;
        } else {
            self.columns.remove(index);
        }
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_type(&mut self, id: usize, type_name: String, cx: &mut Context<Self>) {
        if let Some(column) = self.columns.iter_mut().find(|column| column.id == id) {
            column.type_name = type_name;
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_nullable(&mut self, id: usize, nullable: bool, cx: &mut Context<Self>) {
        if let Some(column) = self.columns.iter_mut().find(|column| column.id == id) {
            column.nullable = nullable;
        }
        self.pending = None;
        cx.notify();
    }

    fn render_type_picker(
        &self,
        column: &EditableColumn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = column.id;
        let current = column.type_name.clone();
        let engine = self.connection.config.engine;
        let editable = self.is_editable();
        let weak = cx.entity().downgrade();

        Button::new(("column-type", id))
            .outline()
            .xsmall()
            .w_full()
            .label(current.clone())
            .dropdown_caret(true)
            .disabled(!editable)
            .dropdown_menu(move |mut menu, _window, _cx| {
                for candidate in sql::common_types(engine) {
                    let candidate = candidate.to_string();
                    let weak = weak.clone();
                    let checked = candidate == current;
                    menu = menu.item(
                        PopupMenuItem::new(candidate.clone())
                            .checked(checked)
                            .on_click(move |_, _window, cx| {
                                if let Some(view) = weak.upgrade() {
                                    let candidate = candidate.clone();
                                    view.update(cx, |view, cx| view.set_type(id, candidate, cx));
                                }
                            }),
                    );
                }
                menu
            })
    }

    /// The heading row the columns table's header and every body row share.
    fn render_columns_header(&self) -> TableRow {
        TableRow::new()
            .child(sized(TableHead::new().text_center().child("Key"), COL_KEY))
            .child(sized(TableHead::new().child("Name"), COL_NAME))
            .child(sized(TableHead::new().child("Type"), COL_TYPE))
            .child(sized(
                TableHead::new().text_center().child("Nullable"),
                COL_NULLABLE,
            ))
            .child(sized(TableHead::new().child("Default"), COL_DEFAULT))
            .child(sized(TableHead::new(), COL_ACTIONS))
    }

    fn render_column_row(&self, column: &EditableColumn, cx: &mut Context<Self>) -> TableRow {
        let id = column.id;
        let editable = self.is_editable();
        let edit = column.snapshot(cx);
        let is_new = edit.is_new();
        let changed = edit.changed();
        let dropped = column.dropped;
        let is_primary_key = column
            .original
            .as_ref()
            .is_some_and(|column| column.is_primary_key);

        // Strongest state first: red for a row about to be dropped, blue for
        // one added by hand, then green for a row with an edit staged on it.
        let row_tint = if dropped {
            Some(cx.theme().danger.opacity(0.15))
        } else if is_new {
            Some(cx.theme().info.opacity(0.15))
        } else if changed {
            Some(cx.theme().success.opacity(0.15))
        } else {
            None
        };

        TableRow::new()
            .when_some(row_tint, |this, color| this.bg(color))
            .child(sized(
                TableCell::new().text_center().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(if is_primary_key { "PK" } else { "" }),
                ),
                COL_KEY,
            ))
            .child(sized(
                TableCell::new().child(
                    Input::new(&column.name)
                        .id(SharedString::from(format!("column-name-{id}")))
                        .xsmall()
                        .min_w_0()
                        .readonly(!editable || dropped)
                        .when(dropped, |this| this.line_through()),
                ),
                COL_NAME,
            ))
            .child(sized(
                TableCell::new().child(self.render_type_picker(column, cx)),
                COL_TYPE,
            ))
            .child(sized(
                TableCell::new().text_center().child(
                    Checkbox::new(("column-nullable", id))
                        .accessibility_label("Nullable")
                        .checked(column.nullable)
                        .disabled(!editable || dropped)
                        .on_click(cx.listener(move |this, nullable: &bool, _window, cx| {
                            this.set_nullable(id, *nullable, cx);
                        })),
                ),
                COL_NULLABLE,
            ))
            .child(sized(
                TableCell::new().child(
                    Input::new(&column.default)
                        .id(SharedString::from(format!("column-default-{id}")))
                        .xsmall()
                        .min_w_0()
                        .readonly(!editable || dropped),
                ),
                COL_DEFAULT,
            ))
            .child(sized(
                TableCell::new().justify_end().when(editable, |this| {
                    let label = if column.original.is_some() {
                        if dropped { "Undrop" } else { "Drop" }
                    } else {
                        "Remove"
                    };
                    this.child(
                        Button::new(("column-drop", id))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .on_click(
                                cx.listener(move |this, _, _window, cx| this.toggle_drop(id, cx)),
                            ),
                    )
                }),
                COL_ACTIONS,
            ))
    }

    pub(super) fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.is_editable();
        let rows: Vec<TableRow> = self
            .columns
            .iter()
            .map(|column| self.render_column_row(column, cx))
            .collect();

        v_flex()
            .id("columns-section")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(self.render_section_heading("COLUMNS", cx))
                    .when(editable, |this| {
                        this.child(
                            Button::new("add-column")
                                .ghost()
                                .xsmall()
                                .label("Add column")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_column(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("columns-table-scroll")
                    .overflow_x_scrollbar()
                    .child(
                        Table::new()
                            .xsmall()
                            .accessibility_label("Columns")
                            .min_w(table_min_width(&[
                                COL_KEY,
                                COL_NAME,
                                COL_TYPE,
                                COL_NULLABLE,
                                COL_DEFAULT,
                                COL_ACTIONS,
                            ]))
                            .child(TableHeader::new().child(self.render_columns_header()))
                            .child(TableBody::new().children(rows)),
                    ),
            )
    }
}
