//! A table's own definition: columns, indexes, and foreign keys.
//!
//! TODO.md's "Table Schema Inspector & DDL Generator". Every section is now a
//! form that diffs itself against what was loaded and turns the difference
//! into statements (`sql.rs`), previewed and confirmed before they run —
//! unconditionally, on every `SafetyMode`, since a dropped column or foreign
//! key can lose data or break other tables in a way a single row's edit
//! cannot. Every section lays its rows out in a `gpui_kit::component::table`
//! `Table`, so the fields line up under column headings the way they do
//! elsewhere in the app. The rows stay form controls — text boxes, dropdowns,
//! and checkboxes — rather than a virtualized data grid, because a row here is
//! a handful of heterogeneous fields being edited, not one record among many.

use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window, div, px};

use crate::db::{
    ColumnDef, Connection, DatabaseObject, Engine, ForeignKeyDef, IndexDef, ObjectKind,
    RebuildSource, ReferentialAction, TableSchema, runtime,
};

mod columns;
mod foreign_keys;
mod indexes;
mod layout;
mod rebuild;
mod sql;
#[cfg(test)]
mod test_support;

use rebuild::Change;
use sql::{ColumnEdit, ForeignKeyEdit, IndexEdit};

/// The fixed set of `ON DELETE`/`ON UPDATE` choices, for the action dropdowns.
fn referential_actions() -> [ReferentialAction; 5] {
    [
        ReferentialAction::NoAction,
        ReferentialAction::Restrict,
        ReferentialAction::Cascade,
        ReferentialAction::SetNull,
        ReferentialAction::SetDefault,
    ]
}

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

/// One index row: a whole index added or a whole one dropped, never
/// modified in place — see `sql::IndexEdit`, which this becomes for the
/// generator. Only a new row's column set is editable; an existing index's
/// composition is shown as loaded.
struct EditableIndex {
    id: usize,
    /// `None` for an index added by hand.
    original: Option<IndexDef>,
    name: Entity<InputState>,
    /// In the order picked — order matters for a composite index.
    columns: Vec<String>,
    unique: bool,
    primary_key: bool,
    /// Only ever set on an index that came from the server; a dropped new
    /// index is removed from the list outright instead.
    dropped: bool,
}

impl EditableIndex {
    fn snapshot(&self, cx: &App) -> IndexEdit {
        IndexEdit {
            original: self.original.clone(),
            name: self.name.read(cx).value().trim().to_string(),
            columns: self.columns.clone(),
            unique: self.unique,
            primary_key: self.primary_key,
            dropped: self.dropped,
        }
    }
}

/// One foreign key row: a whole relationship added or a whole one dropped,
/// never modified in place — see `sql::ForeignKeyEdit`. Only a new row's
/// columns and reference are editable; an existing one is shown as loaded.
struct EditableForeignKey {
    id: usize,
    /// `None` for a foreign key added by hand.
    original: Option<ForeignKeyDef>,
    name: Entity<InputState>,
    /// Local columns, in the order picked — order matches `referenced_columns`.
    columns: Vec<String>,
    referenced_table: Option<DatabaseObject>,
    /// Columns of `referenced_table`, read once it is picked; empty until
    /// then or while that read is in flight.
    referenced_table_columns: Vec<String>,
    /// In the order picked, matching `columns` one for one.
    referenced_columns: Vec<String>,
    on_delete: ReferentialAction,
    on_update: ReferentialAction,
    /// Only ever set on a foreign key that came from the server; a dropped
    /// new one is removed from the list outright instead.
    dropped: bool,
}

impl EditableForeignKey {
    fn snapshot(&self, cx: &App) -> ForeignKeyEdit {
        ForeignKeyEdit {
            original: self.original.clone(),
            name: self.name.read(cx).value().trim().to_string(),
            columns: self.columns.clone(),
            referenced_schema: self
                .referenced_table
                .as_ref()
                .and_then(|t| t.schema.clone()),
            referenced_table: self
                .referenced_table
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_default(),
            referenced_columns: self.referenced_columns.clone(),
            on_delete: self.on_delete,
            on_update: self.on_update,
            dropped: self.dropped,
        }
    }
}

pub struct SchemaView {
    connection: Arc<Connection>,
    object: DatabaseObject,
    /// As loaded, kept for reference: it is what a foreign key's own row
    /// picks its referenced columns from once the target is this table.
    schema: Option<TableSchema>,
    /// As SQLite stores it, read alongside `schema`: what a rebuild has to put
    /// back on top of the table (its indexes, triggers, and views). `None` on
    /// the other engines, and for a view.
    rebuild_source: Option<RebuildSource>,
    /// The Columns section's own editable state, seeded from `schema` each
    /// time it (re)loads.
    columns: Vec<EditableColumn>,
    /// The Indexes section's own editable state, seeded the same way.
    indexes: Vec<EditableIndex>,
    /// The Foreign Keys section's own editable state, seeded the same way.
    foreign_keys: Vec<EditableForeignKey>,
    /// Other tables a new foreign key can reference, read once alongside the
    /// structure rather than reusing the sidebar's list, so this view stays
    /// self-contained.
    tables: Vec<DatabaseObject>,
    next_id: usize,
    loading: bool,
    applying: bool,
    error: Option<String>,
    notice: Option<String>,
    /// The generated statements, shown in `preview` and waiting for "Apply?"
    /// — asked unconditionally, regardless of `SafetyMode`. `None` when
    /// nothing is waiting.
    pending: Option<Change>,
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
            rebuild_source: None,
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            tables: Vec::new(),
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
        self.rebuild_source = None;
        cx.notify();

        let connection = self.connection.clone();
        let object = self.object.clone();
        // Only SQLite needs the stored text a rebuild works from, and only a
        // table has one.
        let wants_source =
            connection.config.engine == Engine::Sqlite && object.kind == ObjectKind::Table;
        let task = runtime::spawn(async move {
            let schema = connection.table_schema(&object).await;
            let source = if wants_source {
                connection.rebuild_source(&object).await.ok()
            } else {
                None
            };
            (schema, connection.objects().await, source)
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((Ok(schema), objects, source)) => {
                        let mut columns = Vec::with_capacity(schema.columns.len());
                        for column in schema.columns.clone() {
                            columns.push(this.new_row(Some(column), window, cx));
                        }
                        this.columns = columns;

                        let mut indexes = Vec::with_capacity(schema.indexes.len());
                        for index in schema.indexes.clone() {
                            indexes.push(this.new_index_row(Some(index), window, cx));
                        }
                        this.indexes = indexes;

                        let mut foreign_keys = Vec::with_capacity(schema.foreign_keys.len());
                        for key in schema.foreign_keys.clone() {
                            foreign_keys.push(this.new_foreign_key_row(Some(key), window, cx));
                        }
                        this.foreign_keys = foreign_keys;

                        // Other tables a new foreign key could reference; a
                        // view cannot be one, and referencing this table
                        // itself is left out to keep the picker simple.
                        this.tables = objects
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|candidate| {
                                candidate.kind == ObjectKind::Table && candidate != &this.object
                            })
                            .collect();

                        this.schema = Some(schema);
                        this.rebuild_source = source;
                    }
                    Ok((Err(error), _, _)) => this.error = Some(format!("{error:#}")),
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
        let columns_changed = self.edits(cx).iter().any(|edit| {
            edit.changed() || edit.dropped || (edit.is_new() && !edit.name.trim().is_empty())
        });
        columns_changed || self.has_index_changes(cx) || self.has_foreign_key_changes(cx)
    }

    /// Generate the statements for what has changed and show them, or say
    /// why not.
    fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() {
            return;
        }
        self.notice = None;

        let engine = self.connection.config.engine;
        let change = rebuild::plan(
            engine,
            &self.object,
            &self.edits(cx),
            &self.index_edits(cx),
            &self.foreign_key_edits(cx),
            self.rebuild_source.as_ref(),
        );

        match change {
            Ok(change) if change.is_empty() => {
                self.error = Some("nothing has changed".into());
                self.pending = None;
            }
            Ok(change) => {
                self.error = None;
                let text = change.statements().join(";\n") + ";";
                self.preview
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                self.pending = Some(change);
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

    /// Run the previewed change, then reload from the server — a rename or a
    /// retype changes identity, so nothing here is patched in place.
    fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(change) = self.pending.take() else {
            return;
        };

        self.applying = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        let connection = self.connection.clone();
        let task = runtime::spawn(async move {
            match change {
                Change::Statements(statements) => {
                    connection.execute_script(&statements).await?;
                }
                // A rebuild is all or nothing: one connection, one
                // transaction, foreign keys checked before it commits.
                Change::Rebuild(statements) => connection
                    .rebuild_table(statements)
                    .await
                    .context("the table rebuild failed")?,
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

    fn render_confirm(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
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
                            .p_3()
                            .gap_4()
                            .child(self.render_header(cx))
                            .child(self.render_columns(cx))
                            .child(self.render_indexes(cx))
                            .child(self.render_foreign_keys(cx)),
                    ),
            )
            .when(self.pending.is_some(), |this| {
                this.when(
                    self.pending.as_ref().is_some_and(Change::is_rebuild),
                    |this| {
                        this.child(
                            div()
                                .flex_none()
                                .px_3()
                                .pt_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "This rebuilds the table: its rows are copied into a new one \
                                     and swapped in, in a single transaction, with the foreign keys \
                                     checked before it commits.",
                                ),
                        )
                    },
                )
                .child(
                    Textarea::new(&self.preview)
                        .flex_none()
                        .mx_3()
                        .mb_2()
                        .h(px(120.))
                        .readonly(true)
                        .font_family(crate::settings::grid_font(cx)),
                )
                .child(self.render_confirm(cx))
            })
            .child(self.render_footer(cx))
    }
}
