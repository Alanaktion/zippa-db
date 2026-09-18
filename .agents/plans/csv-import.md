# CSV import into a table

## Context
TODO.md's "Import & Export Tools" section has export but no way to load tabular data. Users with a CSV/TSV file and an existing table have to write `INSERT`s by hand or convert to a dump. Goal: import a delimited file into one specific table, with a UI to map file columns to table columns, a preview, safety checks, error handling, and progress. Complements [sql-import.md](sql-import.md), which covers whole-dump restore; the two share the runner shape (dedicated connection, progress channel, error policy) and should share code where noted.

## Design

### 1. Parsing: `src/db/import/csv.rs` (new)
- Add the `csv` crate (streaming `Reader`, handles quoting, embedded newlines, custom delimiter and quote). Reuse the reader from sql-import (`src/db/import/reader.rs`: magic-byte detection and counting reader) so `.csv.gz` / `.bz2` / `.zst` work here too. If sql-import is not built first, land `reader.rs` as part of this work and let sql-import reuse it.
- `CsvOptions { delimiter, quote, has_header, encoding, null_text, trim }`.
  - Delimiter is auto-detected from the first ~64 KiB (candidates `,` `;` `\t` `|`; pick the one with the most consistent field count across sampled lines), then editable in the dialog.
  - Encoding: UTF-8 (strip BOM) by default; UTF-16 via BOM; offer Latin-1 / Windows-1252 through `encoding_rs`. Invalid UTF-8 is an error naming the line and offset, never lossy replacement.
  - `null_text`: default empty string means NULL is not assumed. The setting `NULL text handling` (see commit 88649cb, `settings::Settings`) already decides how the grid reads NULL; reuse its meaning as the default, with an override in the dialog.
- `Preview { headers, rows (first 50), row_count_estimate, ragged_rows }` from a bounded sample; never reads the whole file for the preview. Estimate = sampled bytes per row against file size (for compressed input, the count is approximate and labelled "about").
- Ragged rows (field count differs from the header): reported in preview, handled per error policy at import time.

### 2. Target and mapping model: `src/db/import/mapping.rs` (new)
- `TargetColumn { name, type_name, nullable, has_default, generated, is_key }`. Source: extend the existing column metadata used by `TableView` (`column_types` in `src/ui/table_view`) or query the catalog per engine (`postgres.rs` / `mysql.rs` / `sqlite.rs`, same shape as `OBJECTS_SQL`): add `COLUMNS_SQL` per engine returning name, type, nullability, default, generated flag. Keep the per-engine split the architecture already uses.
- `Mapping { source: Option<usize>, target: usize, transform: Transform }` per target column. `Transform` v1: `AsText` (default), `Trim`, `EmptyToNull`, `Constant(String)` (column not in file, fixed value), `Default` (omit the column so the server fills its default, matching how `insert_statement` omits untouched columns).
- Auto-map: match header to column name case-insensitively after normalising (`_`, spaces, `-`); exact match first, then normalised, then leave unmapped. Headerless files map by position.
- Validation, all computed before Start and shown inline:
  - a non-nullable column with no default, not generated, and unmapped, is an error;
  - a generated / identity-always column mapped to file data is an error (Postgres `GENERATED ALWAYS`, MySQL generated columns, SQLite generated) unless left unmapped;
  - the same source column mapped twice is allowed (warning: "used twice");
  - a mapping to a binary or unsupported column type is an error (values are text both ways; binary would need hex/base64 handling, out of scope, see below);
  - a `Constant` that fails the type cast is caught by the dry run (section 4).

### 3. Insert generation and runner: `src/db/import/table.rs`, `Connection::import_rows`
- Statement built once per import from the mapping: `insert into <target> (<cols>) values (<typed placeholders>)`. Reuse `quote_identifier`, `typed_placeholder`, `keyword_literal` from `src/db/sql.rs`; the same logic as `TableView::insert_statement` (`src/ui/table_view/sql.rs:275`). Factor a shared `insert_for(engine, target, columns, types)` into `db/sql.rs` and have `insert_statement` call it, rather than duplicating. Keyword literals (`now()`, `default`) apply only to `Constant` transforms, never to file data: a CSV cell reading `NULL` or `now()` is data, so file values are always bound as parameters. (Differs from the grid, where typed keywords are intentional.)
- Batching: multi-row `values (...), (...)` up to the engine's bind limit (Postgres 65535, MySQL 65535, SQLite 32766 on current builds; cap by `rows_per_batch = limit / columns`, and at most 1000 rows). Prepared statement text is cached per batch size (full size and the remainder).
- One dedicated connection (`pool.acquire()`), same pattern as sql-import's `import_dump`. Refuse when `config.safety.is_read_only()` with its own check.
- Error policy, chosen in the dialog (same enum as sql-import, `OnError { Stop, Rollback, Continue }`):
  - Rollback: whole import in one transaction on all three engines (plain `INSERT`s are transactional everywhere, unlike MySQL DDL, so this works on MySQL too).
  - Stop: commit per batch, halt at the first failed batch.
  - Continue: a failed batch is retried row by row inside a savepoint (`SAVEPOINT` on Postgres, since a failed statement aborts the transaction; per-statement on MySQL and SQLite) so only the bad rows are skipped and logged. Errors recorded as `RowError { line, values excerpt, message }`, capped (e.g. 1000; the count keeps going).
- Optional conflict handling (dialog select, default "fail"): `skip` (`ON CONFLICT DO NOTHING` / `INSERT IGNORE` / `INSERT OR IGNORE`) and `update` (upsert on the key; only offered if the table has a primary key and the key columns are all mapped). Generated per engine in `db/sql.rs`.
- Optional "truncate table first" checkbox: off by default; when on, requires the destructive confirmation below, runs inside the transaction on Postgres/SQLite (`delete from` on SQLite, `truncate` elsewhere; MySQL `TRUNCATE` commits implicitly, so use `delete from` there when Rollback is selected).
- Progress: same channel design as sql-import (`Progress { bytes, rows_done, rows_failed, current_line }`, unbounded, throttled to ~10/s, created inside the `runtime::spawn`'d future). Cancel is the abort handle; a dropped transaction rolls back under Rollback, otherwise batches already committed stay and the summary says how many.
- Result: `ImportSummary { rows_inserted, rows_skipped, errors, elapsed, rolled_back }` (reuse the sql-import type, with a rows variant).
- CPU-bound CSV parsing runs on a blocking thread feeding a bounded `std::sync::mpsc` / `tokio::sync::mpsc::channel(4)` of batches, so parsing overlaps the network round trip and memory stays flat for large files.

### 4. Safety checks (pre-flight and dry run)
- ReadOnly connection: dialog opens but Start is blocked, reason stated in words.
- Confirm step names the target: button reads "Import N rows into <table>". `ConfirmWrites`, `Staged`, and `AutoApply` all show it (an import is bulk by nature, so `AutoApply` is not exempt); "truncate first" adds a warning line and a second checkbox "I understand existing rows are deleted".
- Dry run ("Validate first", on by default): parse the first N rows (default 1000) and run them in a transaction that is always rolled back, so type-cast failures, length overflows, and constraint violations surface with line numbers before any real write. Not a guarantee for the rest of the file, and the dialog says so. On MySQL, MyISAM tables ignore rollback, so detect the storage engine via `information_schema.tables.engine` and disable the dry run (and the Rollback policy) for them, with the reason stated.
- File checks: readable, non-empty, encoding decodes, header row present when expected, duplicate header names flagged.
- Size guard: warn above e.g. 500 MB uncompressed-estimate or 5 million rows that it will take a while and can be cancelled with `Cmd+.`.

### 5. UI: `src/ui/csv_import/` (directory: `mod.rs` view, `mapping.rs` grid, `preview.rs`)
Modal dialog (`window.open_dialog`), same hosting as `value_dialog` / sql-import's `ImportView`. `CsvImportView` emits `CsvImportEvent::{Dismissed, Finished}`. Steps in one dialog, not a wizard with hidden state, so a change on any part updates the rest live:
1. **Source**: file name, detected compression, delimiter / quote / header / encoding / NULL-text controls. Changing any re-parses the preview.
2. **Mapping**: one row per *target* column: name, type, nullability badge ("required", "default", "generated"), a source-column `Select` (file headers plus "(skip)", "(constant…)", "(server default)"), a transform `Select`, and a live sample value (first preview row after transform). Rows with a validation problem show `Error: <reason>` in words, plus an icon; nothing is told by colour alone. "Auto-map again" and "Clear" buttons. Keyboard: normal tab order through selects (gpui-kit `Select`); no icon-only buttons without `.accessibility_label(...)`.
3. **Preview**: the first ~20 rows exactly as they will be written (after mapping and transforms), in a read-only `DataGrid`-style table, cells failing the cast highlighted *and* underlined, with hover/dialog text of the reason once the dry run has run.
4. **Options**: on error (stop / roll back / continue), conflict handling, truncate first, validate first.
5. **Run**: replaces the body with progress: bar plus text label ("38% · 41,200 rows · 3 errors"), Cancel button (also `Cmd+.` via `Session::cancel_query`), then the summary and a scrollable error list with line numbers and "Copy errors" / "Save errors as CSV" (writes the rejected lines, so they can be fixed and re-imported).
- Progress bar: use gpui-kit `Progress` if it exists (sql-import's plan flagged this as unverified); otherwise a thin `div`.
- Entry points:
  - Sidebar table context menu (`src/ui/session/sidebar.rs`, next to "Open"): "Import CSV…" for a table (not for views; views offered only if the engine reports them updatable, else omitted).
  - Table tab toolbar: an "Import" button in `TableView` with `.accessibility_label("Import CSV")` and `tooltip_with_action` (action `ImportCsv`, context `Session`).
  - Menu: `File > Import CSV…` in `src/menu.rs`; action `ImportCsv` declared in `session/mod.rs` `actions!`; when no table is selected the file picker still opens and the dialog asks for a target from a `Select` of the connection's tables (from `Connection::objects`). Optional key `secondary-shift-i` is already proposed by sql-import; use `secondary-alt-i` here (context `Session`) and add both to `shortcuts_dialog.rs` and `README.md`.
  - Dropping a file onto a table tab is a nice follow-up, not v1.
- File picker: `sql_file::prompt_for_open`-style `prompt_for_data_file` with `.csv .tsv .txt .gz .bz2 .zst`, single file.
- On finish: if the table is open in a tab, offer "Reload" (re-reads from the first page through the existing staged-edit guard); refresh the sidebar object list. Staged edits in that tab are never discarded silently.

### 6. Docs and backlog
TODO.md: add "Import CSV into a table" under "Import & Export Tools" and tick it when done; README shortcut; CLAUDE.md gets a short "Importing rows" note next to "Running SQL".

## Critical files
- new: `src/db/import/{csv,mapping,table}.rs` (plus `reader.rs` from sql-import if not yet landed), `src/ui/csv_import/{mod,mapping,preview}.rs`, `src/ui/tests/csv_import.rs`
- edit: `Cargo.toml` (`csv`, `encoding_rs`, and compression crates if sql-import has not added them), `src/db/{mod,connection,sql}.rs`, `src/db/{postgres,mysql,sqlite}.rs` (`COLUMNS_SQL`), `src/ui/table_view/sql.rs` (call the shared insert builder), `src/ui/table_view/mod.rs` (Import button), `src/ui/session/{mod,sidebar}.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `TODO.md`, `README.md`
- reuse: `runtime::spawn` + abort handle, `quote_identifier` / `typed_placeholder` / `keyword_literal` / `placeholder` (`src/db/sql.rs`), `Connection::objects` and `row_key` (primary key lookup for upsert), `config.safety`, `sql_file::prompt_for_open`, `Session::report` / `notify_error`, `TempDatabase` / `ScratchDir` fixtures, `settings::Settings` NULL-text setting.
- Note: the working tree has uncommitted edits in several of these files (`src/db/connection.rs`, `src/ui/session/mod.rs`, `src/ui/session/sidebar.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`); edit on top, never revert.

## Verification
- Unit: delimiter detection (comma, semicolon, tab, pipe, quoted delimiters in fields), BOM and UTF-16 handling, embedded newlines and doubled quotes, ragged rows, auto-map (case, underscores, headerless), mapping validation (required unmapped, generated mapped, duplicate source), batch sizing against bind limits, insert SQL per engine (placeholders and casts, conflict clauses).
- SQLite integration on `TempDatabase`-style file DBs: happy path with `ScratchDir` CSV fixtures (plain and gz); Stop / Rollback / Continue with a bad row in the middle (assert table contents and error line numbers); truncate-first inside a rolled-back run leaves the table intact; upsert and skip; ReadOnly refusal; cancel mid-run; a cell reading `NULL` / `now()` is stored as that text, not evaluated.
- UI tests in `src/ui/tests/csv_import.rs`: `simulate_path_prompt_response` to pick a file, auto-mapping shown, changing a mapping updates preview and validation message, Start blocked on ReadOnly and on required-unmapped, progress and summary states, error list.
- Postgres / MySQL have no live fixtures: manually import a real CSV (identity column, defaults, a bad row) on each through `cargo run --release`; check Continue on Postgres (savepoint path) and MyISAM dry-run detection on MySQL.
- `cargo clippy`, `cargo fmt --check`.

## Out of scope (v1)
Excel (.xlsx), JSON / NDJSON import, importing into a new table (create-from-file with type inference: good follow-up, would share the preview and mapping UI), binary / blob columns, per-column type coercion beyond server-side casts, scheduled or repeated imports, drag-and-drop.
