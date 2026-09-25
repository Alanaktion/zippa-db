//! The structure tab's foreign key section.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::table::{
    Table, TableBody, TableCaption, TableCell, TableHead, TableHeader, TableRow,
};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, SharedString, Window, div, px};

use crate::db::{DatabaseObject, ForeignKeyDef, ObjectKind, ReferentialAction, runtime};

use super::layout::*;
use super::sql::ForeignKeyEdit;
use super::{EditableForeignKey, SchemaView, referential_actions};

impl SchemaView {
    /// Build a row's name box, seeded from `original` when there is one.
    pub(super) fn new_foreign_key_row(
        &mut self,
        original: Option<ForeignKeyDef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EditableForeignKey {
        let id = self.next_id;
        self.next_id += 1;

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("foreign key name")
                .default_value(
                    original
                        .as_ref()
                        .map(|k| k.name.as_str())
                        .unwrap_or_default(),
                )
        });
        cx.subscribe_in(&name, window, Self::on_field_event)
            .detach();

        let columns = original
            .as_ref()
            .map(|k| k.columns.clone())
            .unwrap_or_default();
        let referenced_table = original.as_ref().map(|k| DatabaseObject {
            schema: k.referenced_schema.clone(),
            name: k.referenced_table.clone(),
            kind: ObjectKind::Table,
        });
        let referenced_columns = original
            .as_ref()
            .map(|k| k.referenced_columns.clone())
            .unwrap_or_default();
        let on_delete = original
            .as_ref()
            .map(|k| k.on_delete)
            .unwrap_or(ReferentialAction::NoAction);
        let on_update = original
            .as_ref()
            .map(|k| k.on_update)
            .unwrap_or(ReferentialAction::NoAction);

        EditableForeignKey {
            id,
            original,
            name,
            columns,
            referenced_table,
            referenced_table_columns: Vec::new(),
            referenced_columns,
            on_delete,
            on_update,
            dropped: false,
        }
    }

    /// Whether foreign keys can be changed at all. SQLite now has the table
    /// rebuild behind an add or drop, so this is the same gate as the rest.
    fn foreign_keys_editable(&self) -> bool {
        self.is_editable()
    }

    /// Append a blank row for the user to name and connect by hand.
    pub(super) fn add_foreign_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.foreign_keys_editable() {
            return;
        }
        let row = self.new_foreign_key_row(None, window, cx);
        self.foreign_keys.push(row);
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    /// Mark an existing foreign key to be dropped, or take the mark back
    /// off; one added by hand is removed outright instead, since the server
    /// has nothing of it to drop.
    pub(super) fn toggle_foreign_key_drop(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(position) = self.foreign_keys.iter().position(|key| key.id == id) else {
            return;
        };
        if self.foreign_keys[position].original.is_some() {
            self.foreign_keys[position].dropped = !self.foreign_keys[position].dropped;
        } else {
            self.foreign_keys.remove(position);
        }
        self.notice = None;
        self.pending = None;
        cx.notify();
    }

    /// Add or remove `column` from a new foreign key's local columns,
    /// keeping the order picked — it has to line up with the referenced
    /// columns one for one.
    pub(super) fn toggle_foreign_key_column(
        &mut self,
        id: usize,
        column: String,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.foreign_keys.iter_mut().find(|key| key.id == id) {
            if included {
                if !key.columns.contains(&column) {
                    key.columns.push(column);
                }
            } else {
                key.columns.retain(|existing| existing != &column);
            }
        }
        self.pending = None;
        cx.notify();
    }

    /// Point a new foreign key at `table`, clearing its referenced columns —
    /// they belonged to whatever was picked before — and read that table's
    /// own columns to offer as the picker.
    pub(super) fn pick_referenced_table(
        &mut self,
        id: usize,
        table: DatabaseObject,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.foreign_keys.iter_mut().find(|key| key.id == id) {
            key.referenced_table = Some(table.clone());
            key.referenced_columns.clear();
            key.referenced_table_columns.clear();
        }
        self.pending = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move { connection.table_schema(&table).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Ok(Ok(schema)) = result
                    && let Some(key) = this.foreign_keys.iter_mut().find(|key| key.id == id)
                {
                    key.referenced_table_columns = schema
                        .columns
                        .into_iter()
                        .map(|column| column.name)
                        .collect();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Add or remove `column` from a new foreign key's referenced columns,
    /// keeping the order picked.
    pub(super) fn toggle_referenced_column(
        &mut self,
        id: usize,
        column: String,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.foreign_keys.iter_mut().find(|key| key.id == id) {
            if included {
                if !key.referenced_columns.contains(&column) {
                    key.referenced_columns.push(column);
                }
            } else {
                key.referenced_columns
                    .retain(|existing| existing != &column);
            }
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_foreign_key_on_delete(
        &mut self,
        id: usize,
        action: ReferentialAction,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.foreign_keys.iter_mut().find(|key| key.id == id) {
            key.on_delete = action;
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn set_foreign_key_on_update(
        &mut self,
        id: usize,
        action: ReferentialAction,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = self.foreign_keys.iter_mut().find(|key| key.id == id) {
            key.on_update = action;
        }
        self.pending = None;
        cx.notify();
    }

    pub(super) fn foreign_key_edits(&self, cx: &App) -> Vec<ForeignKeyEdit> {
        self.foreign_keys
            .iter()
            .map(|key| key.snapshot(cx))
            .collect()
    }

    pub(super) fn has_foreign_key_changes(&self, cx: &App) -> bool {
        self.foreign_key_edits(cx).iter().any(|edit| {
            edit.dropped
                || (edit.is_new()
                    && !edit.columns.is_empty()
                    && !edit.referenced_table.trim().is_empty()
                    && !edit.referenced_columns.is_empty())
        })
    }

    /// A checkbox per column `self.schema` has, letting a new foreign key
    /// pick its local columns and the order they match the referenced ones.
    fn render_foreign_key_column_picker(
        &self,
        key: &EditableForeignKey,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = key.id;
        let editable = self.foreign_keys_editable();
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
            let checked = key.columns.contains(&name);
            let for_click = name.clone();
            boxes.push(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Checkbox::new(SharedString::from(format!("fk-local-col-{id}-{name}")))
                            .accessibility_label(format!("Include {name}"))
                            .checked(checked)
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, included: &bool, _window, cx| {
                                this.toggle_foreign_key_column(
                                    id,
                                    for_click.clone(),
                                    *included,
                                    cx,
                                );
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

    /// The same, over the picked referenced table's own columns — empty
    /// until one is picked, and shown as loading between the pick and the
    /// read that fills it in.
    fn render_referenced_column_picker(
        &self,
        key: &EditableForeignKey,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = key.id;
        let editable = self.foreign_keys_editable();

        if key.referenced_table.is_some() && key.referenced_table_columns.is_empty() {
            return div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Loading columns…")
                .into_any_element();
        }

        let mut boxes = Vec::with_capacity(key.referenced_table_columns.len());
        for name in key.referenced_table_columns.clone() {
            let checked = key.referenced_columns.contains(&name);
            let for_click = name.clone();
            boxes.push(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Checkbox::new(SharedString::from(format!("fk-ref-col-{id}-{name}")))
                            .accessibility_label(format!("Include {name}"))
                            .checked(checked)
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, included: &bool, _window, cx| {
                                this.toggle_referenced_column(id, for_click.clone(), *included, cx);
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
            .into_any_element()
    }

    /// The dropdown a new foreign key picks its referenced table from,
    /// sourced from `self.tables` — read once alongside the structure.
    fn render_referenced_table_picker(
        &self,
        key: &EditableForeignKey,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = key.id;
        let editable = self.foreign_keys_editable();
        let label = key
            .referenced_table
            .as_ref()
            .map(|table| table.label())
            .unwrap_or_else(|| "Pick a table".to_string());
        let tables = self.tables.clone();
        let weak = cx.entity().downgrade();

        Button::new(("fk-table", id))
            .outline()
            .xsmall()
            .w_full()
            .label(label)
            .dropdown_caret(true)
            .disabled(!editable)
            .dropdown_menu(move |mut menu, _window, _cx| {
                if tables.is_empty() {
                    return menu.label("No other tables");
                }
                for table in &tables {
                    let table = table.clone();
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(table.label()).on_click(
                        move |_, _window, cx| {
                            if let Some(view) = weak.upgrade() {
                                let table = table.clone();
                                view.update(cx, |view, cx| {
                                    view.pick_referenced_table(id, table, cx)
                                });
                            }
                        },
                    ));
                }
                menu.scrollable(true).max_h(px(240.))
            })
    }

    /// The `ON DELETE`/`ON UPDATE` dropdown for one foreign key; `on_delete`
    /// tells the two apart since they otherwise share every argument.
    fn render_referential_action_picker(
        &self,
        key_id: usize,
        on_delete: bool,
        current: ReferentialAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editable = self.foreign_keys_editable();
        let weak = cx.entity().downgrade();
        let field = if on_delete { "delete" } else { "update" };

        Button::new(SharedString::from(format!("fk-{field}-{key_id}")))
            .outline()
            .xsmall()
            .w_full()
            .label(current.label())
            .dropdown_caret(true)
            .disabled(!editable)
            .dropdown_menu(move |mut menu, _window, _cx| {
                for action in referential_actions() {
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(action.label())
                            .checked(action == current)
                            .on_click(move |_, _window, cx| {
                                if let Some(view) = weak.upgrade() {
                                    view.update(cx, |view, cx| {
                                        if on_delete {
                                            view.set_foreign_key_on_delete(key_id, action, cx);
                                        } else {
                                            view.set_foreign_key_on_update(key_id, action, cx);
                                        }
                                    });
                                }
                            }),
                    );
                }
                menu
            })
    }

    /// The heading row the foreign keys table's header and rows share.
    fn render_foreign_keys_header(&self) -> TableRow {
        TableRow::new()
            .child(sized(TableHead::new().child("Name"), COL_FK_NAME))
            .child(sized(TableHead::new().child("Reference"), COL_FK_REFERENCE))
            .child(sized(TableHead::new().child("On delete"), COL_FK_ACTION))
            .child(sized(TableHead::new().child("On update"), COL_FK_ACTION))
            .child(sized(TableHead::new(), COL_ACTIONS))
    }

    fn render_foreign_key_row(&self, key: &EditableForeignKey, cx: &mut Context<Self>) -> TableRow {
        let id = key.id;
        let editable = self.foreign_keys_editable();
        let edit = key.snapshot(cx);
        let is_new = edit.is_new();
        let dropped = key.dropped;

        let row_tint = if dropped {
            Some(cx.theme().danger.opacity(0.15))
        } else if is_new {
            Some(cx.theme().info.opacity(0.15))
        } else {
            None
        };

        let row = TableRow::new().when_some(row_tint, |this, color| this.bg(color));

        if is_new {
            // Each of these borrows `cx` mutably for its own `cx.listener`s;
            // under edition 2024's RPIT capture rules that borrow would
            // otherwise still be tied to the returned element, so it is
            // erased into an owned `AnyElement` before the next one runs.
            let local_picker = self
                .render_foreign_key_column_picker(key, cx)
                .into_any_element();
            let referenced_picker = self
                .render_referenced_column_picker(key, cx)
                .into_any_element();
            let table_picker = self
                .render_referenced_table_picker(key, cx)
                .into_any_element();
            let delete_picker = self
                .render_referential_action_picker(id, true, key.on_delete, cx)
                .into_any_element();
            let update_picker = self
                .render_referential_action_picker(id, false, key.on_update, cx)
                .into_any_element();

            row.child(sized(
                TableCell::new().child(
                    Input::new(&key.name)
                        .id(SharedString::from(format!("fk-name-{id}")))
                        .xsmall()
                        .min_w_0()
                        .readonly(!editable),
                ),
                COL_FK_NAME,
            ))
            .child(sized(
                TableCell::new().child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(table_picker)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(local_picker)
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("→"),
                                )
                                .child(referenced_picker),
                        ),
                ),
                COL_FK_REFERENCE,
            ))
            .child(sized(TableCell::new().child(delete_picker), COL_FK_ACTION))
            .child(sized(TableCell::new().child(update_picker), COL_FK_ACTION))
            .child(sized(
                TableCell::new().justify_end().child(
                    Button::new(("fk-drop", id))
                        .ghost()
                        .xsmall()
                        .label("Remove")
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.toggle_foreign_key_drop(id, cx);
                        })),
                ),
                COL_ACTIONS,
            ))
        } else {
            let reference = match &edit.referenced_schema {
                Some(schema) => format!("{schema}.{}", edit.referenced_table),
                None => edit.referenced_table.clone(),
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
                COL_FK_NAME,
            ))
            .child(sized(
                TableCell::new().child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(format!(
                            "{} → {reference}({})",
                            edit.columns.join(", "),
                            edit.referenced_columns.join(", ")
                        )),
                ),
                COL_FK_REFERENCE,
            ))
            .child(sized(
                TableCell::new().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(edit.on_delete.label()),
                ),
                COL_FK_ACTION,
            ))
            .child(sized(
                TableCell::new().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(edit.on_update.label()),
                ),
                COL_FK_ACTION,
            ))
            .child(sized(
                TableCell::new().justify_end().when(editable, |this| {
                    let label = if dropped { "Undrop" } else { "Drop" };
                    this.child(
                        Button::new(("fk-drop", id))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.toggle_foreign_key_drop(id, cx)
                            })),
                    )
                }),
                COL_ACTIONS,
            ))
        }
    }

    pub(super) fn render_foreign_keys(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.foreign_keys_editable();

        let rows: Vec<TableRow> = self
            .foreign_keys
            .iter()
            .map(|key| self.render_foreign_key_row(key, cx))
            .collect();
        let empty = if editable {
            "No foreign keys. Use Add foreign key to create one."
        } else {
            "No foreign keys."
        };

        v_flex()
            .id("foreign-keys-section")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(self.render_section_heading("FOREIGN KEYS", cx))
                    .when(editable, |this| {
                        this.child(
                            Button::new("add-foreign-key")
                                .ghost()
                                .xsmall()
                                .label("Add foreign key")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_foreign_key(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("foreign-keys-table-scroll")
                    .overflow_x_scrollbar()
                    .child(
                        Table::new()
                            .xsmall()
                            .accessibility_label("Foreign keys")
                            .min_w(table_min_width(&[
                                COL_FK_NAME,
                                COL_FK_REFERENCE,
                                COL_FK_ACTION,
                                COL_FK_ACTION,
                                COL_ACTIONS,
                            ]))
                            .child(TableHeader::new().child(self.render_foreign_keys_header()))
                            .child(TableBody::new().children(rows))
                            .when(self.foreign_keys.is_empty(), |this| {
                                this.child(TableCaption::new().child(empty))
                            }),
                    ),
            )
    }
}
