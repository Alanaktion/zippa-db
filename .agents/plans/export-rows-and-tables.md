# Export rows/tables to CSV, JSON, SQL INSERT

## Context
TODO.md "Import & Export Tools": export selected rows or entire tables to CSV, JSON, SQL `INSERT`. Decisions: entire-table export honours current filters + sort (no paging); entry points are a footer Export menu (table) and a row-menu item (picked rows).

## Design

### 1. Pure formatters — new `src/db/export.rs` (re-export in `db/mod.rs`)
`pub enum Format { Csv, Json, Sql }` with `extension()`, `label()`.
`pub fn render(format, engine, table: &str, columns: &[String], types: &[String], rows: &[Vec<Cell>]) -> String`
- CSV: header row; quote fields containing `,` `"` `\n` `\r`; NULL is empty field; CRLF-free `\n` line ends.
- JSON: array of objects via `serde_json`; NULL -> null; numeric types (from `column_types`) -> numbers, bool types -> true/false, json/jsonb -> embedded value if it parses, else string. Keep order of columns (enable serde_json `preserve_order` if absent, or build the string by hand).
- SQL: `INSERT INTO {quote_identifier(table)} (cols) VALUES (...);` one per row; NULL -> `NULL`; numbers bare, otherwise `quote_literal`. MySQL also escapes `\` (`quote_literal` does not) — add an engine-aware variant in `db/sql.rs`.
- Placeholders (`query::is_placeholder`, e.g. `<3 bytes>`, `<GEOMETRY>`) are never real data: emit NULL in all formats and have `render` return a count of skipped cells so the UI can say so in its notice.
- Unit tests in the module (quoting, NULL, MySQL backslash, placeholder, type-aware JSON).

### 2. Save dialog — generalise `src/ui/sql_file.rs`
`prompt_for_save` / `with_extension` are hard-wired to `sql`; add an extension parameter (existing caller passes `"sql"`). Reuse `write` (background executor).

### 3. Entire table — `src/ui/table_view/sql.rs`
Split `query_with_params` (sql.rs:52) into `select_sql(&self, cx, paged: bool)`; `paged=false` omits `limit/offset`. Reuse `where_clause`, `sort`, `target`, `take_key_column` (drop rowid column). Add `TableView::export(format, window, cx)`:
1. prompt for path (`sql_file::prompt_for_save`),
2. `runtime::spawn(connection.run_query_with(sql, params))` — same pattern as `reload` (mod.rs:388),
3. `render`, write file, set `self.notice` "Exported N rows to <file>"; errors via `notify_error`.
Note: `run_query_with` loads all rows into memory; acceptable for v1, mention in TODO as follow-up for streaming.

### 4. Picked rows — data grid
- Add `DataGrid::picked(cx) -> Option<(Vec<String> columns, Vec<String> types, Vec<Vec<Cell>>)>` using sorted `rows_selected` and `row_cells` (includes staged edits, consistent with Copy).
- Row menu (`delegate.rs:546-559`, beside "Copy N rows"): "Export N rows…" item. It needs to reach `TableView`; emit a new `GridEvent::Export` (or similar) that `TableView` already subscribes to (mod.rs:189-192). Format chosen via submenu Export as CSV/JSON/SQL. Query-result grids (non-table tabs) have no table name: SQL format uses a placeholder name, so hide the SQL item there or only offer export for `TableView`.

### 5. Footer — `render_footer` (table_view/mod.rs:1015)
Add icon `Button` with `.accessibility_label("Export")`, `.tooltip(...)`, opening a `PopupMenu` (CSV / JSON / SQL INSERT). Follow the pattern at mod.rs:1185-1198. Keyboard access via the button being tab-reachable; no new keybinding required (avoids conflicts in keymap.rs).

### 6. Housekeeping
Update TODO.md checkbox and README feature list if it lists features.

## Verification
- `cargo test db::export` — formatter unit tests.
- New `src/ui/tests/export.rs` (fixtures in `tests/mod.rs`, per repo convention): use `simulate_new_path_selection` with `ScratchDir`, export table and picked rows, assert file contents for each format, honouring filter + sort.
- `cargo clippy`, `cargo test`.
- Manual: `cargo run`, open a SQLite table, filter, export each format, re-import the SQL file via the editor to confirm round trip.
