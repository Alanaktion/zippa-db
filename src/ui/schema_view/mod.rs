//! A table's own definition: columns, indexes, and foreign keys.
//!
//! TODO.md's "Table Schema Inspector & DDL Generator". Phase 1 was read-only;
//! phase 2 turns the Columns section into a form that diffs itself against
//! what was loaded and turns the difference into `ALTER TABLE` statements
//! (`sql.rs`), previewed and confirmed before they run — unconditionally, on
//! every `SafetyMode`, since a dropped column or a retyped `NOT NULL` can
//! lose data outright in a way a single row's edit cannot. Indexes and
//! foreign keys stay read-only until phases 4 and 5. A table's own structure
//! is a handful of rows with heterogeneous per-row detail, which is a form
//! rather than a data grid, so this does not reach for
//! `gpui_kit::component::table` the way `DataGrid` does.

use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, SharedString, Window, div, px};

use crate::db::{
    ColumnDef, Connection, DatabaseObject, ForeignKeyDef, IndexDef, ObjectKind, TableSchema,
    runtime,
};

mod sql;
use sql::ColumnEdit;

/// One column row as the form shows it: a stable identity (so a rename is
/// tracked by which row it is, not by matching names) plus the fields the
/// user can change. Name and default are kept as `InputState` directly —
/// there is nothing to mirror into a plain field, since the box already is
/// the value.
struct EditableColumn {
    id: usize,
    /// `None` for a column added by hand.
    original: Option<ColumnDef>,
    name: Entity<InputState>,
    type_name: String,
    nullable: bool,
    default: Entity<InputState>,
    /// Only ever set on a column that came from the server; a dropped new
    /// column is removed from the list outright instead.
    dropped: bool,
}

impl EditableColumn {
    fn snapshot(&self, cx: &App) -> ColumnEdit {
        let default = self.default.read(cx).value().trim().to_string();
        ColumnEdit {
            original: self.original.clone(),
            name: self.name.read(cx).value().trim().to_string(),
            type_name: self.type_name.clone(),
            nullable: self.nullable,
            default: (!default.is_empty()).then_some(default),
            dropped: self.dropped,
        }
    }
}

pub struct SchemaView {
    connection: Arc<Connection>,
    object: DatabaseObject,
    /// Indexes and foreign keys as loaded; read-only until phases 4 and 5.
    schema: Option<TableSchema>,
    /// The Columns section's own editable state, seeded from `schema` each
    /// time it (re)loads.
    columns: Vec<EditableColumn>,
    next_id: usize,
    loading: bool,
    applying: bool,
    error: Option<String>,
    notice: Option<String>,
    /// The generated statements, shown in `preview` and waiting for "Apply?"
    /// — asked unconditionally, regardless of `SafetyMode`. `None` when
    /// nothing is waiting.
    pending: Option<Vec<String>>,
    preview: Entity<TextareaState>,
}

impl SchemaView {
    pub fn new(
        connection: Arc<Connection>,
        object: DatabaseObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preview = cx.new(|cx| TextareaState::new(window, cx));

        let mut view = Self {
            connection,
            object,
            schema: None,
            columns: Vec::new(),
            next_id: 0,
            loading: false,
            applying: false,
            error: None,
            notice: None,
            pending: None,
            preview,
        };
        view.reload(cx);
        view
    }

    pub fn object(&self) -> &DatabaseObject {
        &self.object
    }

    /// Unapplied edits, or a preview waiting on "Apply?", would be lost by
    /// closing the tab.
    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        self.pending.is_some() || self.has_changes(cx)
    }

    /// Point the view at a new connection, as when the database is switched.
    pub(crate) fn set_connection(&mut self, connection: Arc<Connection>, cx: &mut Context<Self>) {
        self.connection = connection;
        self.schema = None;
        self.pending = None;
        self.notice = None;
        self.reload(cx);
    }

    /// Read the structure fresh from the server.
    ///
    /// Leaves `notice` alone: `apply`'s "Changes applied" is set right before
    /// this runs and is meant to survive it, since the reload it is
    /// reporting on is this same one.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self.pending = None;
        cx.notify();

        let connection = self.connection.clone();
        let object = self.object.clone();
        let task = runtime::spawn(async move { connection.table_schema(&object).await });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(schema)) => {
                        let mut columns = Vec::with_capacity(schema.columns.len());
                        for column in schema.columns.clone() {
                            columns.push(this.new_row(Some(column), window, cx));
                        }
                        this.columns = columns;
                        this.schema = Some(schema);
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => {
                        this.error = Some("reading the table's structure was cancelled".into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Build a row's inputs, seeded from `original` when there is one.
    fn new_row(
        &mut self,
        original: Option<ColumnDef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EditableColumn {
        let id = self.next_id;
        self.next_id += 1;

        let name = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                original
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or_default(),
            )
        });
        cx.subscribe_in(&name, window, Self::on_field_event)
            .detach();

        let default = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Default")
                .default_value(
                    original
                        .as_ref()
                        .and_then(|c| c.default.as_deref())
                        .unwrap_or_default(),
                )
        });
        cx.subscribe_in(&default, window, Self::on_field_event)
            .detach();

        let type_name = original
            .as_ref()
            .map(|c| c.type_name.clone())
            .unwrap_or_else(|| sql::common_types(self.connection.config.engine)[0].to_string());
        let nullable = original.as_ref().map(|c| c.nullable).unwrap_or(true);

        EditableColumn {
            id,
            original,
            name,
            type_name,
            nullable,
            default,
            dropped: false,
        }
    }

    /// A row's text box changed; the view has no mirrored copy of it to
    /// update, but the preview and the change summary need to redraw.
    fn on_field_event(
        &mut self,
        _input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.pending = None;
            cx.notify();
        }
    }

    /// Whether this connection and object allow changing columns at all.
    fn is_editable(&self) -> bool {
        !self.connection.config.safety.is_read_only() && self.object.kind == ObjectKind::Table
    }

    /// Why editing is off, when it is, for the footer.
    fn read_only_reason(&self) -> Option<&'static str> {
        if self.connection.config.safety.is_read_only() {
            return Some("this connection is read-only");
        }
        if self.object.kind == ObjectKind::View {
            return Some("a view has no columns of its own to change");
        }
        None
    }

    fn edits(&self, cx: &App) -> Vec<ColumnEdit> {
        self.columns
            .iter()
            .map(|column| column.snapshot(cx))
            .collect()
    }

    fn has_changes(&self, cx: &App) -> bool {
        self.edits(cx).iter().any(|edit| {
            edit.changed() || edit.dropped || (edit.is_new() && !edit.name.trim().is_empty())
        })
    }

    /// Append a blank row for the user to fill in by hand.
    fn add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn toggle_drop(&mut self, id: usize, cx: &mut Context<Self>) {
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

    fn set_type(&mut self, id: usize, type_name: String, cx: &mut Context<Self>) {
        if let Some(column) = self.columns.iter_mut().find(|column| column.id == id) {
            column.type_name = type_name;
        }
        self.pending = None;
        cx.notify();
    }

    fn set_nullable(&mut self, id: usize, nullable: bool, cx: &mut Context<Self>) {
        if let Some(column) = self.columns.iter_mut().find(|column| column.id == id) {
            column.nullable = nullable;
        }
        self.pending = None;
        cx.notify();
    }

    /// Generate the statements for what has changed and show them, or say
    /// why not.
    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        self.notice = None;

        let engine = self.connection.config.engine;
        let target = sql::target(&self.object, engine);
        let edits = self.edits(cx);

        match sql::generate_alter_statements(engine, &target, &edits) {
            Ok(statements) if statements.is_empty() => {
                self.error = Some("nothing has changed".into());
                self.pending = None;
            }
            Ok(statements) => {
                self.error = None;
                let text = statements.join(";\n") + ";";
                self.preview
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                self.pending = Some(statements);
            }
            Err(message) => {
                self.error = Some(message);
                self.pending = None;
            }
        }
        cx.notify();
    }

    /// Leave the preview without running it; the edits stay staged to try
    /// again.
    fn cancel_preview(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        cx.notify();
    }

    /// Run the previewed statements in order, then reload from the server —
    /// a rename or a retype changes identity, so nothing here is patched in
    /// place.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.pending.take() else {
            return;
        };

        self.applying = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let connection = self.connection.clone();
        let total = statements.len();
        let task = runtime::spawn(async move {
            // No transaction yet, same as `Connection::run_script`: a
            // statement that fails leaves the ones before it applied.
            for (position, statement) in statements.iter().enumerate() {
                connection
                    .execute(statement, Vec::new())
                    .await
                    .with_context(|| format!("statement {} of {total}", position + 1))?;
            }
            Ok::<(), anyhow::Error>(())
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.applying = false;
                match result {
                    Ok(Ok(())) => {
                        this.notice = Some("Changes applied".into());
                        this.reload(cx);
                    }
                    Ok(Err(error)) => this.error = Some(format!("{error:#}")),
                    Err(_) => this.error = Some("applying the changes was cancelled".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        let kind = match self.object.kind {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
        };

        h_flex()
            .w_full()
            .flex_none()
            .gap_2()
            .child(div().text_sm().child(self.object.label()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(kind),
            )
    }

    fn render_section_heading(&self, title: &'static str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(title)
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
            .w(px(160.))
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

    fn render_column_row(
        &self,
        column: &EditableColumn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = column.id;
        let editable = self.is_editable();
        let edit = column.snapshot(cx);
        let is_new = edit.is_new();
        let changed = edit.changed();
        let dropped = column.dropped;

        let row_tint = if dropped {
            Some(cx.theme().danger.opacity(0.15))
        } else if is_new {
            Some(cx.theme().info.opacity(0.15))
        } else if changed {
            Some(cx.theme().success.opacity(0.15))
        } else {
            None
        };

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_1()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(row_tint, |this, color| this.bg(color))
            .child(
                div()
                    .flex_none()
                    .w(px(20.))
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(
                        if column.original.as_ref().is_some_and(|c| c.is_primary_key) {
                            "PK"
                        } else {
                            ""
                        },
                    ),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&column.name)
                        .id(SharedString::from(format!("column-name-{id}")))
                        .xsmall()
                        .readonly(!editable || dropped)
                        .when(dropped, |this| this.line_through()),
                ),
            )
            .child(self.render_type_picker(column, cx))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(
                        Checkbox::new(("column-nullable", id))
                            .accessibility_label("Nullable")
                            .checked(column.nullable)
                            .disabled(!editable || dropped)
                            .on_click(cx.listener(move |this, nullable: &bool, _window, cx| {
                                this.set_nullable(id, *nullable, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("NULL"),
                    ),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&column.default)
                        .id(SharedString::from(format!("column-default-{id}")))
                        .xsmall()
                        .readonly(!editable || dropped),
                ),
            )
            .when(editable, |this| {
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
            })
    }

    fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.is_editable();

        // A `.map(...).collect()` here would tie each row's element to a
        // reborrow of `cx` that ends inside the closure — the row needs
        // `cx` mutably (for its own `cx.listener`s), so it is built in a
        // plain loop instead, reborrowing `cx` fresh each time, and turned
        // into an owned `AnyElement` before it joins the others.
        let mut rows = Vec::with_capacity(self.columns.len());
        for column in &self.columns {
            rows.push(self.render_column_row(column, cx).into_any_element());
        }

        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
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
            .children(rows)
    }

    fn render_indexes(&self, indexes: &[IndexDef], cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(self.render_section_heading("INDEXES", cx))
            .when(indexes.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No indexes"),
                )
            })
            .children(indexes.iter().map(|index| {
                let kind = match (index.is_primary_key, index.unique) {
                    (true, _) => "PRIMARY KEY",
                    (false, true) => "UNIQUE",
                    (false, false) => "INDEX",
                };

                h_flex()
                    .w_full()
                    .gap_3()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(index.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(100.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(kind),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(index.columns.join(", ")),
                    )
            }))
    }

    fn render_foreign_keys(&self, keys: &[ForeignKeyDef], cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .child(self.render_section_heading("FOREIGN KEYS", cx))
            .when(keys.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No foreign keys"),
                )
            })
            .children(keys.iter().map(|key| {
                let reference = match &key.referenced_schema {
                    Some(schema) => format!("{schema}.{}", key.referenced_table),
                    None => key.referenced_table.clone(),
                };

                h_flex()
                    .w_full()
                    .gap_3()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(key.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} → {reference}({})",
                                key.columns.join(", "),
                                key.referenced_columns.join(", ")
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(260.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "ON DELETE {} · ON UPDATE {}",
                                key.on_delete.label(),
                                key.on_update.label()
                            )),
                    )
            }))
    }

    fn render_confirm(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_2()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child("Apply these changes?"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cancel-schema-apply")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_preview(cx))),
                    )
                    .child(
                        Button::new("confirm-schema-apply")
                            .primary()
                            .xsmall()
                            .label("Apply")
                            .on_click(cx.listener(|this, _, _window, cx| this.apply(cx))),
                    ),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let message = match (&self.error, self.loading, self.applying, &self.notice) {
            (Some(error), _, _, _) => (format!("Error: {error}"), cx.theme().danger),
            (None, _, true, _) => ("Applying…".to_string(), cx.theme().muted_foreground),
            (None, true, _, _) => ("Loading…".to_string(), cx.theme().muted_foreground),
            (None, false, false, Some(notice)) => (notice.clone(), cx.theme().muted_foreground),
            (None, false, false, None) => (String::new(), cx.theme().muted_foreground),
        };

        h_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_1()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(div().text_xs().text_color(message.1).child(message.0))
            .child(
                h_flex()
                    .gap_2()
                    .when_some(self.read_only_reason(), |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Read-only: {reason}")),
                        )
                    })
                    .when(self.is_editable() && self.pending.is_none(), |this| {
                        this.child(
                            Button::new("preview-schema-changes")
                                .primary()
                                .xsmall()
                                .label("Preview changes")
                                .disabled(self.applying || !self.has_changes(cx))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.preview(window, cx);
                                })),
                        )
                    }),
            )
    }
}

impl Render for SchemaView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("schema-view")
            .test_support()
            .size_full()
            .child(
                div()
                    .id("schema-sections")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .p_3()
                            .gap_4()
                            .child(self.render_header(cx))
                            .child(self.render_columns(cx))
                            .when_some(self.schema.clone(), |this, schema| {
                                this.child(self.render_indexes(&schema.indexes, cx))
                                    .child(self.render_foreign_keys(&schema.foreign_keys, cx))
                            }),
                    ),
            )
            .when(self.pending.is_some(), |this| {
                this.child(
                    div().flex_none().px_3().pb_2().child(
                        Textarea::new(&self.preview)
                            .h(px(120.))
                            .readonly(true)
                            .font_family(crate::settings::grid_font(cx)),
                    ),
                )
                .child(self.render_confirm(cx))
            })
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
impl SchemaView {
    pub(crate) fn schema_for_test(&self) -> Option<TableSchema> {
        self.schema.clone()
    }

    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn column_names_for_test(&self, cx: &App) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.name.read(cx).value().to_string())
            .collect()
    }

    pub(crate) fn set_column_name_for_test(
        &mut self,
        index: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.columns[index].name.clone();
        input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn set_column_default_for_test(
        &mut self,
        index: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.columns[index].default.clone();
        input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn set_column_type_for_test(
        &mut self,
        index: usize,
        type_name: &str,
        cx: &mut Context<Self>,
    ) {
        let id = self.columns[index].id;
        self.set_type(id, type_name.to_string(), cx);
    }

    pub(crate) fn set_column_nullable_for_test(
        &mut self,
        index: usize,
        nullable: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.columns[index].id;
        self.set_nullable(id, nullable, cx);
    }

    pub(crate) fn toggle_drop_for_test(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.columns[index].id;
        self.toggle_drop(id, cx);
    }

    pub(crate) fn add_column_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_column(window, cx);
    }

    pub(crate) fn preview_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview(window, cx);
    }

    pub(crate) fn pending_statements_for_test(&self) -> Option<Vec<String>> {
        self.pending.clone()
    }

    pub(crate) fn apply_for_test(&mut self, cx: &mut Context<Self>) {
        self.apply(cx);
    }

    pub(crate) fn notice_for_test(&self) -> Option<String> {
        self.notice.clone()
    }
}
