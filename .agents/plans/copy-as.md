# Copy as… (CSV, TSV, JSON, Markdown, SQL)

## Context
`Cmd+C` in the grid copies picked rows as tab-separated lines, or the selected cell (`CopyValue`, `data_grid/mod.rs`; row menu items `Copy value` and `Copy N rows`). That is the only clipboard format. `export-rows-and-tables.md` plans pure formatters in `src/db/export.rs` (`render(format, engine, table, columns, types, rows)` for CSV/JSON/SQL, with placeholder handling) plus file export. This plan is the clipboard counterpart and must not fork the formatters: it depends on that plan's `export::render` and adds the formats only the clipboard wants.

## Decisions
- One formatter module. Build `db/export.rs` (from the export plan) first, or in the same change, and extend it with `Format::{Tsv, Markdown}` plus the two list forms below. File export can then offer the same formats later at no cost.
- Scope of a copy: picked rows, else every row currently in the grid (what is on screen, including staged edits, as `Copy` already does), else the selected cell for the single-value forms. A `Copy column` form takes the selected cell's column.
- Never silently truncate: the clipboard payload is capped (default 100,000 rows or 50 MB of text); over the cap the user is told `Copied first 100,000 of 250,000 rows` instead of hanging the UI. The cap is a constant, not a setting, in v1.

## Design

### 1. Formats
| Menu item | Output |
| --- | --- |
| Copy with headers (TSV) | header line, then tab-separated rows; `Cmd+Shift+C` |
| Copy as CSV | RFC 4180 quoting, header row, `\n` line ends |
| Copy as JSON | array of objects, type-aware (numbers, booleans, embedded JSON), column order kept |
| Copy as Markdown table | header, `---` separator row, rows; `|` escaped as `\|`, newlines in cells replaced by `<br>`, padded columns so it reads in plain text |
| Copy as SQL INSERT | one `INSERT` per row via `export::render`; only on a table tab (needs the table name) |
| Copy column values | selected column, one value per line |
| Copy as `IN (...)` list | selected column as `('a', 'b', 3)`: numbers bare, text via `quote_literal`, NULLs dropped, duplicates kept in order; the everyday "paste ids into a WHERE" case |
Placeholders (`<3 bytes>`, `<GEOMETRY>`; `query::is_placeholder`) become NULL in every format, and the notice says how many cells were skipped, matching the export plan.

### 2. Grid API — `src/ui/data_grid/mod.rs`
The export plan adds `DataGrid::picked(cx) -> Option<(columns, types, rows)>`. Generalise to `DataGrid::snapshot(scope, cx) -> Snapshot` where `Scope` is `Picked | All | Column(usize)`, returning columns, types, and rows with staged edits applied (reuse `row_cells`). One place decides what "the rows to copy" means; `CopyValue` keeps its current behaviour by calling it, so existing tests must pass untouched.

### 3. UI
- Row menu (`delegate.rs`, beside `Copy value` / `Copy N rows`): a `Copy as` submenu with the items above. The action shown beside `Copy with headers` is its shortcut, using `.action(Box::new(...))` the way `CopyValue` is shown. Items that need a table name are hidden on query-result grids; items that need a selected column are disabled without one, with a tooltip.
- Header cell menu (if the grid has one; else skip): `Copy column name`, `Copy column values`.
- New action `CopyWithHeaders` next to `CopyValue` in `data_grid`, bound in `keymap.rs` as `secondary-shift-c` in `DataGrid > DataTable` and `TableView > DataTable` (verify it does not collide with an existing binding or the input's own `secondary-shift-c`, and that the value dialog and query editor contexts are unaffected).
- After every copy, a transient status message `Copied 12 rows as CSV` in the table view's `notice` / the query tab's status bar; it is the only confirmation and must not be colour-only.
- Row-picking rules unchanged: `Copy as` operates on picked rows when any are picked and on all rows otherwise; the menu label states which (`Copy as CSV (12 rows)`).

### 4. Formatting details worth pinning in tests
- CSV: fields containing `,`, `"`, `\n`, `\r` quoted; embedded quotes doubled; NULL as an empty unquoted field (say so in the docs; an empty string is `""`, which keeps the two distinguishable).
- TSV: tabs and newlines inside cells replaced with spaces to keep one row per line; this is lossy and worth one sentence in the UI tooltip (`Tabs and newlines in values become spaces`). Existing `Copy N rows` behaviour stays exactly as it is today.
- JSON: numeric types from `column_types`; `json`/`jsonb` embedded as values when they parse; large integers beyond 2^53 emitted as strings to avoid precision loss in JavaScript consumers (document; choose consistently with the export plan).
- Markdown: escape `|` and backslashes; long values are not truncated.
- SQL: engine-aware string literal quoting (the export plan adds the MySQL backslash variant of `quote_literal`).
- `IN` list: type-aware, so a column typed as numeric or boolean is bare, uuid/date/text quoted; empty selection yields `()` with a notice `No values to copy`.

## Sequencing
1. Land or extend `db/export.rs` with `Tsv`, `Markdown`, `values`, and `in_list`; unit tests.
2. `DataGrid::snapshot` and refactor `CopyValue`/`Copy N rows` onto it (no behaviour change).
3. `Copy as` submenu and `CopyWithHeaders` shortcut; status message.
4. Payload cap, docs.

## Critical files
- edit: `src/db/export.rs` (from the export plan), `src/db/sql.rs` (`quote_literal` engine variant), `src/ui/data_grid/{mod,delegate}.rs`, `src/ui/table_view/mod.rs`, `src/ui/session/mod.rs` (query-result copy notices), `src/keymap.rs`, `src/ui/shortcuts_dialog.rs`, `README.md`, `TODO.md`
- new: `src/ui/tests/copy.rs` (or add to `rows.rs`, matching the file-per-area convention; `rows.rs` already owns row selection tests, so prefer it and only split if it grows unwieldy)
- reuse: `ClipboardItem`, `row_cells`, `query::is_placeholder`, `export::render`.

## Testing
- Formatter unit tests per format (quoting, NULL vs empty string, placeholder skip count, big integers, Markdown escaping, `IN` list typing and NULL dropping, empty input).
- Grid: with rows picked, `Copy as CSV` copies only those; with none picked, copies all shown rows including a staged edit; deleted-row overlay rows are excluded from the copy (decide and test: a row marked for deletion is still on screen but is going away; exclude them and say so in the tooltip, or include them. Check what `Copy N rows` does today and match it).
- Read the clipboard through the GPUI test context, as existing copy tests do.
- Cap: a synthetic 100,001-row result copies 100,000 and reports it.
- Manual: paste CSV into a spreadsheet, Markdown into a GitHub comment, `IN` list into the editor and run it.

## Risks / open questions
- The clipboard payload for very large results is built on the UI thread; do it in a background task beyond ~10,000 rows and show `Copying…`.
- `secondary-shift-c` may already be claimed by the text input for copy-as-something or by a platform menu; confirm before committing to it, otherwise leave the action menu-only.
- Query-result grids have no table name, so `Copy as SQL INSERT` is unavailable there; a `table_name` placeholder prompt could be added later.
