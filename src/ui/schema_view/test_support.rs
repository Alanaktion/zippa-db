//! Test-only reach-ins, for `src/ui/tests/schema.rs`.

use gpui_kit::{App, Context, Window};

use crate::db::{DatabaseObject, ReferentialAction, TableExtras, TableSchema};

use super::rebuild::Change;
use super::{Panel, SchemaView};

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

    pub(crate) fn set_table_name_for_test(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.table_name.clone();
        input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    pub(crate) fn is_new_table_for_test(&self) -> bool {
        self.new_table
    }

    pub(crate) fn preview_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview(window, cx);
    }

    pub(crate) fn pending_statements_for_test(&self) -> Option<Vec<String>> {
        self.pending
            .as_ref()
            .map(|change| change.statements().to_vec())
    }

    pub(crate) fn pending_is_rebuild_for_test(&self) -> bool {
        self.pending.as_ref().is_some_and(Change::is_rebuild)
    }

    pub(crate) fn apply_for_test(&mut self, cx: &mut Context<Self>) {
        self.apply(cx);
    }

    pub(crate) fn notice_for_test(&self) -> Option<String> {
        self.notice.clone()
    }

    /// The `CREATE VIEW` text the structure tab shows for a view.
    pub(crate) fn view_source_for_test(&self, cx: &App) -> String {
        self.definition.read(cx).value().to_string()
    }

    /// Which half of the tab is showing.
    pub(crate) fn panel_for_test(&self) -> Panel {
        self.panel
    }

    /// Whether the form half is available: views have none.
    pub(crate) fn form_disabled_for_test(&self) -> bool {
        self.object.kind == crate::db::ObjectKind::View
    }

    /// Switch halves the way the toggle buttons do.
    pub(crate) fn set_panel_for_test(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.object.kind == crate::db::ObjectKind::View {
            return;
        }
        self.panel = panel;
        cx.notify();
    }

    /// The SQL panel's definition text.
    pub(crate) fn definition_for_test(&self, cx: &App) -> String {
        self.definition.read(cx).value().to_string()
    }

    /// The read-only extras: checks, constraints, and triggers.
    pub(crate) fn extras_for_test(&self) -> TableExtras {
        self.extras.clone()
    }

    /// The note the SQL panel carries when the form has unsaved changes,
    /// or `None` when it carries none.
    pub(crate) fn sql_panel_note_for_test(&self, cx: &App) -> Option<String> {
        self.sql_panel_note(cx).map(str::to_string)
    }

    /// Run the SQL panel's Copy the way its button does.
    pub(crate) fn copy_definition_for_test(&mut self, cx: &mut Context<Self>) {
        self.copy_definition(cx);
    }

    /// Read the structure again, the way applying a change does — used to
    /// pick up a change made through a second connection.
    pub(crate) fn reload_for_test(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }
}
