# SQL dump import tool

## Context
TODO.md line 53 lists "SQL file restoration". Today the only way to run a dump is paste into the editor: `statement::split` loads whole input as `Vec<char>` (not feasible for big dumps), `run_script` has no transaction and hops pooled connections (so `SET`, `USE`, `FOREIGN_KEY_CHECKS`, temp tables do not persist), and no progress or compression support exists. Goal: File > Import SQL Dump… with safety checks, error handling, progress bar, and .gz/.bz2/.zst input.

Decisions (from user): modal dialog UI; error policy chosen in dialog (stop / roll back / continue-and-log); compression = gzip + bzip2 + zstd.

## Design

### 1. Streaming reader — `src/db/import/reader.rs` (new)
- `open(path) -> (impl Read, total_bytes, Compression)`; detect by magic bytes (`1f 8b`, `BZh`, `28 b5 2f fd`), fall back to extension. Deps: `flate2`, `bzip2`, `zstd` (Cargo.toml).
- Wrap the raw `File` in a counting reader (`Arc<AtomicU64>` of compressed bytes consumed) so progress = consumed / file size works for compressed input too (uncompressed size unknown).
- Sync `BufRead`, run inside `spawn_blocking`/dedicated thread; no tokio `fs`/`io-util` needed.

### 2. Streaming splitter — `src/db/import/splitter.rs` (new; keep `statement::split` for editor)
Iterator over `BufRead` yielding `Chunk { sql, line, bytes_end }` or `Chunk::CopyData`. Per-dialect state machine, one statement in memory at a time:
- Common: `'`, `"`, backtick, `--`, `/* */` (nested for Postgres), keeps `/*!40101 … */` verbatim.
- Backslash escapes only for MySQL strings (and PG `E'..'`); fixes gap in existing `skip_quoted`.
- Postgres: `$tag$` bodies; `COPY … FROM stdin;` switches to data mode until `\.` line, fed via `copy_in_raw`; skip psql `\` meta-lines (`\connect`, `\restrict`).
- MySQL: `DELIMITER x` client command handled and dropped.
- SQLite: `BEGIN…END` / `CASE…END` depth for triggers.
- Max statement size cap (e.g. 64 MiB) → error naming line, guards runaway unterminated quote.
- Unit tests per dialect, incl. sample pg_dump / mysqldump / sqlite `.dump` fixtures.

### 3. Import runner — `src/db/import/mod.rs`, `Connection::import_dump`
- Refuse when `config.safety.is_read_only()` (own check; do not rely on server-side setting).
- Acquire ONE dedicated `PoolConnection` per engine arm (generic over `DB`, reusing `execute_with` bounds).
- Error policy enum `OnError { Stop, Rollback, Continue }`:
  - Rollback: wrap in `begin()` for Postgres/SQLite (DDL transactional). MySQL: option disabled/warned ("DDL commits implicitly"), falls back to Stop.
  - Dumps with own `BEGIN/COMMIT` (pg `--single-transaction`, sqlite `.dump`): detect while scanning; skip our wrapper and note it in the dialog.
  - Continue: collect `ImportError { line, statement_excerpt, message }` list (capped).
- MySQL: on held connection set `foreign_key_checks`/`unique_checks` only if the dump does not set them itself; restore at end.
- Progress: `tokio::sync::mpsc::unbounded_channel` created in `runtime::spawn`'d future; sends `Progress { bytes, statements, errors, current_line }` throttled (~10/s). Inline test runtime is fine with unbounded.
- Cancel: abort handle from `runtime::spawn`; dropped `Transaction` rolls back (PG/SQLite). MySQL reports what was applied.
- Result: `ImportSummary { statements, errors, elapsed, rolled_back }`.

### 4. Safety checks (pre-flight, shown in dialog before Start)
- File: exists/readable, detected compression, size.
- Connection: engine, database name, safety mode. ReadOnly → dialog blocked with reason. ConfirmWrites/Staged → require explicit "Import into <db>" button (typed name not needed; button names target db). AutoApply → same button, still shown.
- Quick scan of first ~64 KiB: dialect hints (pg_dump header, mysqldump header, sqlite) vs connection engine → warning on mismatch (non-blocking).
- Scan flags destructive statements (`DROP`, `TRUNCATE`, `DELETE` without where via first-word check) → count shown as warning ("contains N DROP/TRUNCATE statements").

### 5. UI — `src/ui/import_dialog.rs` (new), follows `value_dialog` / `shortcuts_dialog`
- `ImportView` entity, emits `ImportEvent::{Dismissed, Finished}`. States: `Ready` (pre-flight + on-error radio + Start) → `Running` (progress bar, statements/errors counters, current line, Cancel) → `Done` (summary, scrollable error list with line numbers, copy log).
- Progress bar: check gpui-kit for `Progress`; else thin `div` bar. Text label always present ("42% · 1,204 statements · 2 errors") so it is not colour-only; errors say `Error: …` in words.
- Entry points: `ImportSqlDump` action in `session/mod.rs` `actions!`, handler in `Session` opens file picker (`sql_file::prompt_for_open`, single file, add `.sql .gz .bz2 .zst` filter) then `window.open_dialog`; `File > Import SQL Dump…` in `src/menu.rs`; entry in `shortcuts_dialog.rs` if key bound (suggest `secondary-shift-i`, context `Session`, in `keymap.rs`). Sidebar header button with `.accessibility_label` + `tooltip_with_action`.
- `Session::cancel_query` (`secondary-.`) also aborts an active import; Session holds `importing: Option<AbortHandle>`.
- On finish: refresh sidebar objects (`Refresh` path) and notify.

### 6. Docs
Tick/split TODO.md line 53, add README shortcut, add note to CLAUDE.md architecture ("Importing").

## Critical files
- new: `src/db/import/{mod,reader,splitter}.rs`, `src/ui/import_dialog.rs`, `src/ui/tests/import.rs`
- edit: `Cargo.toml`, `src/db/{mod,connection}.rs`, `src/ui/session/mod.rs`, `src/ui/session/sidebar.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `TODO.md`
- reuse: `runtime::spawn` + abort handle, `Connection::config.safety`, `sql_file::prompt_for_open`, `Session::report` / `notify_error`, `TempDatabase` and `ScratchDir` fixtures.
- Note: working tree already has uncommitted edits in several of these files; edit on top, do not revert.

## Verification
- `cargo test db::import` — splitter cases (DELIMITER, COPY, triggers, `$$`, backslash), reader magic-byte detection with gz/bz2/zst fixtures written in tests.
- DB tests on SQLite `TempDatabase`: happy path, Stop/Rollback/Continue on bad statement (assert table state), ReadOnly refusal, dump with own BEGIN/COMMIT, cancel mid-run.
- UI tests in `src/ui/tests/import.rs` (`simulate_path_prompt_response`, progress states, blocked on ReadOnly, error list).
- Postgres/MySQL: no live fixtures exist; manual check with real pg_dump / mysqldump output (plain and `.gz`) via `cargo run --release`.
- `cargo clippy`, `cargo fmt --check`.

## Out of scope (v1)
xz/lzma, pg_dump custom/tar formats, resumable imports, psql `\i` includes.
