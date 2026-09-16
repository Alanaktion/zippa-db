//! A table's own definition: columns, indexes, and foreign keys.
//!
//! TODO.md's "Table Schema Inspector & DDL Generator". Every section is now a
//! form that diffs itself against what was loaded and turns the difference
//! into statements (`sql.rs`), previewed and confirmed before they run —
//! unconditionally, on every `SafetyMode`, since a dropped column or foreign
//! key can lose data or break other tables in a way a single row's edit
//! cannot. A table's own structure is a handful of rows with heterogeneous
//! per-row detail, which is a form rather than a data grid, so this does not
//! reach for `gpui_kit::component::table` the way `DataGrid` does.

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
    ColumnDef, Connection, DatabaseObject, Engine, ForeignKeyDef, IndexDef, ObjectKind,
    ReferentialAction, TableSchema, runtime,
};

mod sql;
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
        cx.notify();

        let connection = self.connection.clone();
        let object = self.object.clone();
        let task = runtime::spawn(async move {
            (
                connection.table_schema(&object).await,
                connection.objects().await,
            )
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((Ok(schema), objects)) => {
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
                    }
                    Ok((Err(error), _)) => this.error = Some(format!("{error:#}")),
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

    /// Build a row's name box, seeded from `original` when there is one.
    fn new_index_row(
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
    fn add_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn toggle_index_drop(&mut self, id: usize, cx: &mut Context<Self>) {
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
    fn toggle_index_column(
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

    fn set_index_unique(&mut self, id: usize, unique: bool, cx: &mut Context<Self>) {
        if let Some(index) = self.indexes.iter_mut().find(|index| index.id == id) {
            index.unique = unique;
        }
        self.pending = None;
        cx.notify();
    }

    fn set_index_primary_key(&mut self, id: usize, primary_key: bool, cx: &mut Context<Self>) {
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

    fn index_edits(&self, cx: &App) -> Vec<IndexEdit> {
        self.indexes
            .iter()
            .map(|index| index.snapshot(cx))
            .collect()
    }

    fn has_index_changes(&self, cx: &App) -> bool {
        self.index_edits(cx).iter().any(|edit| {
            edit.dropped
                || (edit.is_new() && !edit.columns.is_empty() && !edit.name.trim().is_empty())
        })
    }

    /// Build a row's name box, seeded from `original` when there is one.
    fn new_foreign_key_row(
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

    /// Whether foreign keys can be changed at all: SQLite cannot add or drop
    /// one on an existing table without the phase 3 rebuild, so it is
    /// refused here rather than in the generator alone.
    fn foreign_keys_editable(&self) -> bool {
        self.is_editable() && self.connection.config.engine != Engine::Sqlite
    }

    /// Append a blank row for the user to name and connect by hand.
    fn add_foreign_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn toggle_foreign_key_drop(&mut self, id: usize, cx: &mut Context<Self>) {
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
    fn toggle_foreign_key_column(
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
    fn pick_referenced_table(&mut self, id: usize, table: DatabaseObject, cx: &mut Context<Self>) {
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
    fn toggle_referenced_column(
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

    fn set_foreign_key_on_delete(
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

    fn set_foreign_key_on_update(
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

    fn foreign_key_edits(&self, cx: &App) -> Vec<ForeignKeyEdit> {
        self.foreign_keys
            .iter()
            .map(|key| key.snapshot(cx))
            .collect()
    }

    fn has_foreign_key_changes(&self, cx: &App) -> bool {
        self.foreign_key_edits(cx).iter().any(|edit| {
            edit.dropped
                || (edit.is_new()
                    && !edit.columns.is_empty()
                    && !edit.referenced_table.trim().is_empty()
                    && !edit.referenced_columns.is_empty())
        })
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
        let column_statements = sql::generate_alter_statements(engine, &target, &self.edits(cx));
        let index_statements =
            sql::generate_index_statements(engine, &self.object, &self.index_edits(cx));
        let foreign_key_statements =
            sql::generate_foreign_key_statements(engine, &self.object, &self.foreign_key_edits(cx));

        let combined = column_statements.and_then(|mut statements| {
            statements.extend(index_statements?);
            statements.extend(foreign_key_statements?);
            Ok(statements)
        });

        match combined {
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

    fn render_index_row(&self, index: &EditableIndex, cx: &mut Context<Self>) -> impl IntoElement {
        let id = index.id;
        let editable = self.is_editable();
        let engine = self.connection.config.engine;
        let edit = index.snapshot(cx);
        let is_new = edit.is_new();
        let dropped = index.dropped;
        // SQLite cannot add or drop a primary key on an existing table at
        // all without the rebuild phase 3 adds; a plain index is unaffected.
        let is_primary_key = index
            .original
            .as_ref()
            .is_some_and(|original| original.is_primary_key)
            || index.primary_key;
        let pk_blocked_on_sqlite = engine == Engine::Sqlite && is_primary_key;

        let row_tint = if dropped {
            Some(cx.theme().danger.opacity(0.15))
        } else if is_new {
            Some(cx.theme().info.opacity(0.15))
        } else {
            None
        };

        let row = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_1()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(row_tint, |this, color| this.bg(color));

        let row = if is_new {
            row.child(
                div().flex_1().min_w_0().child(
                    Input::new(&index.name)
                        .id(SharedString::from(format!("index-name-{id}")))
                        .xsmall()
                        .readonly(!editable),
                ),
            )
            .child(self.render_index_column_picker(index, cx))
            .child(
                h_flex()
                    .flex_none()
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
                                    .disabled(
                                        !editable
                                            || engine == Engine::Sqlite
                                            || self.has_existing_primary_key(),
                                    )
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
            )
        } else {
            let kind = match (index.primary_key, index.unique) {
                (true, _) => "PRIMARY KEY",
                (false, true) => "UNIQUE",
                (false, false) => "INDEX",
            };

            row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .when(dropped, |this| this.line_through())
                    .child(edit.name.clone()),
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
                    .child(edit.columns.join(", ")),
            )
        };

        row.when(editable, |this| {
            if pk_blocked_on_sqlite {
                return this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Needs a rebuild on SQLite"),
                );
            }

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
        })
    }

    fn render_indexes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.is_editable();

        let mut rows = Vec::with_capacity(self.indexes.len());
        for index in &self.indexes {
            rows.push(self.render_index_row(index, cx).into_any_element());
        }

        v_flex()
            .w_full()
            .gap_1()
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
            .when(self.indexes.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No indexes"),
                )
            })
            .children(rows)
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
                    .items_center()
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
                    .items_center()
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
            .w(px(180.))
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
            .w(px(110.))
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

    fn render_foreign_key_row(
        &self,
        key: &EditableForeignKey,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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

        let container = v_flex()
            .w_full()
            .gap_1()
            .px_1()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(row_tint, |this, color| this.bg(color));

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

            container
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&key.name)
                                    .id(SharedString::from(format!("fk-name-{id}")))
                                    .xsmall()
                                    .readonly(!editable),
                            ),
                        )
                        .child(table_picker)
                        .child(delete_picker)
                        .child(update_picker)
                        .child(
                            Button::new(("fk-drop", id))
                                .ghost()
                                .xsmall()
                                .label("Remove")
                                .disabled(!editable)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.toggle_foreign_key_drop(id, cx);
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
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
                )
        } else {
            let reference = match &edit.referenced_schema {
                Some(schema) => format!("{schema}.{}", edit.referenced_table),
                None => edit.referenced_table.clone(),
            };

            container.child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .when(dropped, |this| this.line_through())
                            .child(edit.name.clone()),
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
                                edit.columns.join(", "),
                                edit.referenced_columns.join(", ")
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(220.))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "ON DELETE {} · ON UPDATE {}",
                                edit.on_delete.label(),
                                edit.on_update.label()
                            )),
                    )
                    .when(editable, |this| {
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
            )
        }
    }

    fn render_foreign_keys(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.foreign_keys_editable();
        // The table can otherwise be edited, but not its foreign keys —
        // SQLite cannot change one on an existing table at all.
        let sqlite_blocked = self.is_editable() && !editable;

        let mut rows = Vec::with_capacity(self.foreign_keys.len());
        for key in &self.foreign_keys {
            rows.push(self.render_foreign_key_row(key, cx).into_any_element());
        }

        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
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
                    })
                    .when(sqlite_blocked, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("SQLite: rebuild required to change"),
                        )
                    }),
            )
            .when(self.foreign_keys.is_empty(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No foreign keys"),
                )
            })
            .children(rows)
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
                            .child(self.render_indexes(cx))
                            .child(self.render_foreign_keys(cx)),
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

    pub(crate) fn index_names_for_test(&self, cx: &App) -> Vec<String> {
        self.indexes
            .iter()
            .map(|index| index.name.read(cx).value().to_string())
            .collect()
    }

    pub(crate) fn add_index_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_index(window, cx);
    }

    pub(crate) fn set_index_name_for_test(
        &mut self,
        index: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.indexes[index].name.clone();
        input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn toggle_index_column_for_test(
        &mut self,
        index: usize,
        column: &str,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.indexes[index].id;
        self.toggle_index_column(id, column.to_string(), included, cx);
    }

    pub(crate) fn set_index_unique_for_test(
        &mut self,
        index: usize,
        unique: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.indexes[index].id;
        self.set_index_unique(id, unique, cx);
    }

    pub(crate) fn set_index_primary_key_for_test(
        &mut self,
        index: usize,
        primary_key: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.indexes[index].id;
        self.set_index_primary_key(id, primary_key, cx);
    }

    pub(crate) fn toggle_index_drop_for_test(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.indexes[index].id;
        self.toggle_index_drop(id, cx);
    }

    pub(crate) fn foreign_key_names_for_test(&self, cx: &App) -> Vec<String> {
        self.foreign_keys
            .iter()
            .map(|key| key.name.read(cx).value().to_string())
            .collect()
    }

    pub(crate) fn tables_for_test(&self) -> Vec<DatabaseObject> {
        self.tables.clone()
    }

    pub(crate) fn add_foreign_key_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_foreign_key(window, cx);
    }

    /// Push a blank row the way `add_foreign_key` does, but without its
    /// "not on SQLite" gate — for exercising the picker mechanics on the
    /// only engine the test harness has, independent of that gate, which
    /// `foreign_keys_cannot_be_added_at_all_on_sqlite` covers on its own.
    pub(crate) fn push_new_foreign_key_row_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.new_foreign_key_row(None, window, cx);
        self.foreign_keys.push(row);
    }

    pub(crate) fn set_foreign_key_name_for_test(
        &mut self,
        index: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.foreign_keys[index].name.clone();
        input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn toggle_foreign_key_column_for_test(
        &mut self,
        index: usize,
        column: &str,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.toggle_foreign_key_column(id, column.to_string(), included, cx);
    }

    pub(crate) fn pick_referenced_table_for_test(
        &mut self,
        index: usize,
        table: DatabaseObject,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.pick_referenced_table(id, table, cx);
    }

    pub(crate) fn referenced_table_columns_for_test(&self, index: usize) -> Vec<String> {
        self.foreign_keys[index].referenced_table_columns.clone()
    }

    pub(crate) fn foreign_key_actions_for_test(
        &self,
        index: usize,
    ) -> (ReferentialAction, ReferentialAction) {
        let key = &self.foreign_keys[index];
        (key.on_delete, key.on_update)
    }

    pub(crate) fn toggle_referenced_column_for_test(
        &mut self,
        index: usize,
        column: &str,
        included: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.toggle_referenced_column(id, column.to_string(), included, cx);
    }

    pub(crate) fn set_foreign_key_on_delete_for_test(
        &mut self,
        index: usize,
        action: ReferentialAction,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.set_foreign_key_on_delete(id, action, cx);
    }

    pub(crate) fn set_foreign_key_on_update_for_test(
        &mut self,
        index: usize,
        action: ReferentialAction,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.set_foreign_key_on_update(id, action, cx);
    }

    pub(crate) fn toggle_foreign_key_drop_for_test(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let id = self.foreign_keys[index].id;
        self.toggle_foreign_key_drop(id, cx);
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

    /// Read the structure again, the way applying a change does — used to
    /// pick up a change made through a second connection.
    pub(crate) fn reload_for_test(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }
}
