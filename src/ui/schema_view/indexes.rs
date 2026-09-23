//! The structure tab's index section, primary key included.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::table::{
    Table, TableBody, TableCaption, TableCell, TableHead, TableHeader, TableRow,
};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, SharedString, Window, div};

use crate::db::IndexDef;

use super::layout::*;
use super::sql::IndexEdit;
use super::{EditableIndex, SchemaView};

impl SchemaView {
    /// Build a row's name box, seeded from `original` when there is one.
    pub(super) fn new_index_row(
        &mut self,
        original: Option<IndexDef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EditableIndex {
        let id = self.next_id;
        self.next_id += 1;

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("index name")
                .default_value(
                    original
                        .as_ref()
                        .map(|i| i.name.as_str())
                        .unwrap_or_default(),
                )
        });
        cx.subscribe_in(&name, window, Self::on_field_event)
            .detach();

        let columns = original
            .as_ref()
            .map(|i| i.columns.clone())
            .unwrap_or_default();
        let unique = original.as_ref().map(|i| i.unique).unwrap_or(false);
        let primary_key = original.as_ref().map(|i| i.is_primary_key).unwrap_or(false);

        EditableIndex {
            id,
            original,
            name,
            columns,
            unique,
            primary_key,
            dropped: false,
        }
    }

    /// Append a blank row for the user to name and pick columns for.
    pub(super) fn add_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        let row = self.new_index_row(None, window, cx);
        self.indexes.push(row);
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    /// Mark an existing index to be dropped, or take the mark back off; an
    /// index added by hand is removed outright instead, since the server has
    /// nothing of it to drop.
    pub(super) fn toggle_index_drop(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(position) = self.indexes.iter().position(|index| index.id == id) else {
            return;
        };
        if self.indexes[position].original.is_some() {
            self.indexes[position].dropped = !self.indexes[position].dropped;
        } else {
            self.indexes.remove(position);
        }
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    /// Add or remove `column` from a new index's column set, keeping the
    /// order columns were picked in — that order is what a composite index
    /// searches by.
    pub(super) fn toggle_index_column(
        &mut self,
        id: usize,
        column: String,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.indexes.iter_mut().find(|index| index.id == id) {
            if included {
                if !index.columns.contains(&column) {
                    index.columns.push(column);
                }
            } else {
                index.columns.retain(|existing| existing != &column);
            }
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_index_unique(&mut self, id: usize, unique: bool, cx: &mut Context<Self>) {
        if let Some(index) = self.indexes.iter_mut().find(|index| index.id == id) {
            index.unique = unique;
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_index_primary_key(
        &mut self,
        id: usize,
        primary_key: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.indexes.iter_mut().find(|index| index.id == id) {
            index.primary_key = primary_key;
        }
        self.pending = None;
        cx.notify();
    }

    /// Whether the table already has a primary key from the server that has
    /// not been marked to drop — a table can only have one, so this disables
    /// adding a second.
    fn has_existing_primary_key(&self) -> bool {
        self.indexes.iter().any(|index| {
            index
                .original
                .as_ref()
                .is_some_and(|original| original.is_primary_key)
                && !index.dropped
        })
    }

    pub(super) fn index_edits(&self, cx: &App) -> Vec<IndexEdit> {
        self.indexes
            .iter()
            .map(|index| index.snapshot(cx))
            .collect()
    }

    pub(super) fn has_index_changes(&self, cx: &App) -> bool {
        self.index_edits(cx).iter().any(|edit| {
            edit.dropped
                || (edit.is_new() && !edit.columns.is_empty() && !edit.name.trim().is_empty())
        })
    }

    /// A checkbox per column the table has, letting a new index pick which
    /// ones it covers and in what order — an existing index's composition is
    /// fixed, so this is only shown for a row added by hand.
    fn render_index_column_picker(
        &self,
        index: &EditableIndex,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = index.id;
        let editable = self.is_editable();
        let available: Vec<String> = self
            .schema
            .as_ref()
            .map(|schema| {
                schema
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        let mut boxes = Vec::with_capacity(available.len());
        for name in available {
            let checked = index.columns.contains(&name);
            let for_click = name.clone();
            boxes.push(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(
                        Checkbox::new(SharedString::from(format!("index-column-{id}-{name}")))
                            .accessibility_label(format!("Include {name}"))
                            .checked(checked)
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, included: &bool, _window, cx| {
                                this.toggle_index_column(id, for_click.clone(), *included, cx);
                            })),
                    )
                    .child(div().text_xs().child(name))
                    .into_any_element(),
            );
        }

        h_flex()
            .flex_1()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .children(boxes)
    }

    /// The heading row the indexes table's header and every body row share.
    fn render_indexes_header(&self) -> TableRow {
        TableRow::new()
            .child(sized(TableHead::new().child("Name"), COL_INDEX_NAME))
            .child(sized(TableHead::new().child("Kind"), COL_INDEX_KIND))
            .child(sized(TableHead::new().child("Columns"), COL_INDEX_COLUMNS))
            .child(sized(TableHead::new(), COL_ACTIONS))
    }

    fn render_index_row(&self, index: &EditableIndex, cx: &mut Context<Self>) -> TableRow {
        let id = index.id;
        let editable = self.is_editable();
        let edit = index.snapshot(cx);
        let is_new = edit.is_new();
        let dropped = index.dropped;

        let row_tint = if dropped {
            Some(cx.theme().danger.opacity(0.15))
        } else if is_new {
            Some(cx.theme().info.opacity(0.15))
        } else {
            None
        };

        let row = TableRow::new().when_some(row_tint, |this, color| this.bg(color));

        let row = if is_new {
            row.child(sized(
                TableCell::new().child(
                    div().w_full().min_w_0().child(
                        Input::new(&index.name)
                            .id(SharedString::from(format!("index-name-{id}")))
                            .xsmall()
                            .readonly(!editable),
                    ),
                ),
                COL_INDEX_NAME,
            ))
            .child(sized(
                TableCell::new().child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Checkbox::new(("index-unique", id))
                                        .accessibility_label("Unique")
                                        .checked(index.unique)
                                        .disabled(!editable || index.primary_key)
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _window, cx| {
                                                this.set_index_unique(id, *checked, cx);
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("UNIQUE"),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Checkbox::new(("index-primary-key", id))
                                        .accessibility_label("Primary key")
                                        .checked(index.primary_key)
                                        .disabled(!editable || self.has_existing_primary_key())
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _window, cx| {
                                                this.set_index_primary_key(id, *checked, cx);
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("PK"),
                                ),
                        ),
                ),
                COL_INDEX_KIND,
            ))
            .child(sized(
                TableCell::new().child(self.render_index_column_picker(index, cx)),
                COL_INDEX_COLUMNS,
            ))
        } else {
            let kind = match (index.primary_key, index.unique) {
                (true, _) => "PRIMARY KEY",
                (false, true) => "UNIQUE",
                (false, false) => "INDEX",
            };

            row.child(sized(
                TableCell::new().child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .when(dropped, |this| this.line_through())
                        .child(edit.name.clone()),
                ),
                COL_INDEX_NAME,
            ))
            .child(sized(
                TableCell::new().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(kind),
                ),
                COL_INDEX_KIND,
            ))
            .child(sized(
                TableCell::new().child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(edit.columns.join(", ")),
                ),
                COL_INDEX_COLUMNS,
            ))
        };

        row.child(sized(
            TableCell::new().justify_end().when(editable, |this| {
                let label = if index.original.is_some() {
                    if dropped { "Undrop" } else { "Drop" }
                } else {
                    "Remove"
                };
                this.child(
                    Button::new(("index-drop", id))
                        .ghost()
                        .xsmall()
                        .label(label)
                        .on_click(
                            cx.listener(move |this, _, _window, cx| this.toggle_index_drop(id, cx)),
                        ),
                )
            }),
            COL_ACTIONS,
        ))
    }

    pub(super) fn render_indexes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.is_editable();
        let rows: Vec<TableRow> = self
            .indexes
            .iter()
            .map(|index| self.render_index_row(index, cx))
            .collect();
        let empty = if editable {
            "No indexes. Use Add index to create one."
        } else {
            "No indexes."
        };

        v_flex()
            .id("indexes-section")
            .test_support()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(self.render_section_heading("INDEXES", cx))
                    .when(editable, |this| {
                        this.child(
                            Button::new("add-index")
                                .ghost()
                                .xsmall()
                                .label("Add index")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_index(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("indexes-table-scroll")
                    .w_full()
                    .min_w_0()
                    .h_auto()
                    .overflow_x_scrollbar()
                    .child(
                        Table::new()
                            .xsmall()
                            .accessibility_label("Indexes")
                            .min_w(table_min_width(&[
                                COL_INDEX_NAME,
                                COL_INDEX_KIND,
                                COL_INDEX_COLUMNS,
                                COL_ACTIONS,
                            ]))
                            .child(TableHeader::new().child(self.render_indexes_header()))
                            .child(TableBody::new().children(rows))
                            .when(self.indexes.is_empty(), |this| {
                                this.child(TableCaption::new().child(empty))
                            }),
                    ),
            )
    }
}
