# Data export: CSV / JSON / SQL INSERT

## Context

TODO.md backlog item: "Export selected rows or entire tables to CSV, JSON, and SQL
`INSERT` statements." Confirmed with user:

- "Entire table" re-queries the whole table, ignoring the current page/limit.
- Entry points: a footer button (TableView) plus a row-menu item, both dropdown
  menus of format choices.
- Covers both `TableView` (sidebar-opened tables) and plain query-result grids
  (`QueryEditor` + `DataGrid`).

One deviation from a literal reading, surfaced here rather than silently: SQL
`INSERT` needs a table name. A plain query result is an arbitrary `SELECT` with
no target table, so **SQL Insert is offered only from `TableView`** (which has
`target()`); the plain query-result grid offers CSV/JSON only. Selected/loaded
row export reads through `ResultDelegate::cell()`, which already folds in
unwritten edits and skips deleted rows — so it exports what's on screen, not a
stale server value.

## 1. `src/db/export.rs` (new)

```rust
pub enum ExportFormat { Csv, Json, SqlInsert }
impl ExportFormat { pub fn extension(&self) -> &'static str { "csv"/"json"/"sql" } }
pub(crate) fn render(
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Cell>],
    table: Option<(&str, Engine)>, // None unless SqlInsert
) -> String
```

- **Csv**: header + rows, hand-rolled RFC4180 quoting (quote a field containing
  `,`/`"`/`\n`/`\r`, double embedded `"`); `None` → empty field. No new
  dependency.
- **Json**: `serde_json` array of objects, `Value::String`/`Value::Null` per
  cell (values stay text — "Values are text, both ways" — no numeric/bool
  inference), `serde_json::to_string_pretty`.
- **SqlInsert**: one `INSERT INTO {table} ({cols}) VALUES (...);` per row.
  Reuse `db::quote_identifier` (columns) and `db::sql::quote_literal` (values,
  already `pub(crate)` at the crate root per `db/mod.rs`); `NULL` for `None`
  and for `query::is_placeholder` cells (unsupported/binary values can't be
  reconstructed — call this out in a doc comment). Widen `quote_literal`'s doc
  comment (`db/sql.rs:31-35`), which currently says it's "used only for
  metadata queries" — export is a second, legitimate caller.
- Re-export `ExportFormat` from `db/mod.rs`.
- Inline `#[cfg(test)] mod tests`: CSV escaping, JSON null handling, SQL
  literal escaping + NULL + placeholder handling.

## 2. `src/ui/data_grid/mod.rs` — accessors + event

- `pub fn has_row_selection(&self, cx: &App) -> bool`
- `pub fn export_columns(&self, cx: &App) -> Vec<String>`
- `pub fn export_rows(&self, only_selected: bool, cx: &App) -> Vec<Vec<Cell>>` —
  iterate `0..order.len() + drafts.len()`, skip `delegate.is_deleted(row_ix)`,
  skip unselected rows when `only_selected`, read every column through
  `delegate.cell(row_ix, col_ix)` (already merges staged edits/drafts).
- New event `pub struct ExportRequested { pub format: ExportFormat, pub only_selected: bool }`
  + `impl EventEmitter<ExportRequested> for DataGrid {}`, emitted by a new
  `pub fn request_export(&mut self, format, only_selected, cx)`. The grid only
  reports the request; the owner (TableView or the query tab) does the file
  dialog and write, since only it knows the suggested name / table / engine —
  same split as `GridEdit`/`ViewReporter` today.
- `ResultDelegate::row_menu_items` (`delegate.rs:392-454`) gains an "Export
  selected rows" / "Export row" submenu (CSV/JSON only — the grid itself has
  no table name) next to the existing "Delete row(s)" item, built the same way
  and calling back through a new `ExportReporter` closure (mirrors
  `ViewReporter`) into `DataGrid::request_export`.

## 3. `TableView` (`src/ui/table_view/mod.rs`, `sql.rs`)

- Footer button beside "New row"/"Apply" (`render_footer`, `mod.rs:878-965`):
  `Button::new("export").ghost().xsmall().label("Export")` with
  `.dropdown_menu(...)` (`gpui_kit` `Button`/`PopupMenu` support this
  directly — see `dropdown_button.rs`'s inner `Button::new("popup")...
  .dropdown_menu_with_anchor(...)`). Menu: `.submenu("Selected rows", ...)`
  (only when `grid.has_row_selection(cx)`), `.submenu("All rows", ...)`,
  `.submenu("Entire table", ...)` — each a nested submenu of CSV/JSON/SQL
  Insert (`PopupMenu::submenu`, `gpui-kit/crates/component/src/menu/popup_menu.rs:669`).
- Subscribe to the grid's `ExportRequested` alongside the existing `GridEdit`
  subscription → `TableView::export_loaded(format, only_selected, cx)`: reads
  `grid.export_columns`/`export_rows`, calls
  `db::export::render(format, &cols, &rows, Some((&self.target(), engine)))`,
  then the save flow below.
- New `TableView::export_entire_table(format, cx)` in `sql.rs`, beside
  `target()`/`query_with_params`: builds `SELECT * FROM {target}`, runs it via
  `self.connection.run_query(...)` (same runtime path as paging), feeds the
  resulting `QueryResult` into `db::export::render`.
- Save flow: generalize `src/ui/sql_file.rs`'s `prompt_for_save`/
  `suggested_name`/`with_extension` to take an `extension: &str` parameter
  instead of the hardcoded `EXTENSION` const; existing `.sql` save/open
  callers pass `"sql"` explicitly. Export passes `format.extension()`. Write
  via the existing `write(path, contents)` on the background executor, same
  await-then-spawn shape the SQL save action already uses.

## 4. Plain query-result grid (`src/ui/session/mod.rs`)

- `render_panes` (`mod.rs:1178-1218`): wrap `grid.clone()` in a small
  always-shown bar (currently only `render_result_bar` shows conditionally,
  for `results.len() > 1`) holding one `Button::new("export").ghost().xsmall().label("Export")`
  with `.dropdown_menu(...)` offering "Selected rows"/"All rows" ×
  CSV/JSON only.
- Subscribe to the grid's `ExportRequested` the same way the tab already
  subscribes to `GridEdit`/`SortRequested`; call
  `db::export::render(format, &cols, &rows, None)`, then the same save flow
  (suggested name from the tab's own title, mirroring `sql_file::suggested_name`).

## 5. Tests

- `src/db/export.rs`: unit tests per format (see above).
- New `src/ui/tests/export.rs`, registered in `tests/mod.rs`'s module list,
  using the existing `table_view(cx)` fixture (`tests/rows.rs`) and the
  `ScratchDir`/`simulate_new_path_selection`/`did_prompt_for_new_path` harness
  from `tests/files.rs`:
  - Export "all rows" and "selected rows" from a `TableView`, each format;
    assert written file contents.
  - "Entire table" re-queries beyond the current page (seed more rows than
    page size).
  - Menu items appear/disappear correctly with/without a row selection, and
    the plain query-result grid never offers SQL Insert.

## Verification

- `cargo test export` (new tests), then `cargo test` (full suite).
- `cargo run`: run a `SELECT`, export selected rows to CSV and JSON; open a
  table from the sidebar, export "entire table" to SQL Insert, and replay the
  generated file against a scratch SQLite DB to confirm it round-trips.
