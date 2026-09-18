# Query execution history

## Context
TODO.md "Query History & Snippets" lists a local execution history log, searchable by date, database, or query string. Today nothing records what ran: `Session::send` (`src/ui/session/mod.rs`) runs a buffer and shows results, then forgets it. Goal: every run from the query editor is logged locally, and a searchable History dialog lets the user reopen or re-run an entry.

## Decisions
- Log only what the user typed in the query editor (`send`, both single-statement and script). Do not log statements Zippa generates (table paging, staged edits, filters); they are noise and can be regenerated.
- Storage: append-only JSON Lines file `history.jsonl` in `store::config_dir()`. Chosen over a SQLite file because the app already persists JSON, appends are cheap and crash-safe, and a few tens of thousands of entries load and filter in memory instantly. Revisit if the cap needs to grow past ~50k.
- Cap: newest 5,000 entries (setting `history_limit`, `0` disables logging). Compact (rewrite file) at load time when the file exceeds the cap by 20%.
- Never store passwords: entries carry the connection id and name only. SQL text can contain secrets, so add a settings toggle "Record query history" (default on) and a "Clear history" button.

## Design

### 1. Data — `src/db/history.rs` (new, re-exported from `db/mod.rs`)
```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct HistoryEntry {
    pub id: Uuid,
    pub at: DateTime<Utc>,       // chrono is already a dependency for timestamptz; confirm in Cargo.toml
    pub connection_id: Uuid,
    pub connection_name: String, // snapshot so a deleted connection still reads sensibly
    pub engine: Engine,
    pub database: String,
    pub sql: String,
    pub script: bool,
    pub outcome: Outcome,        // Ok { rows: Option<u64>, affected: Option<u64> } | Error(String) | Cancelled
    pub elapsed_ms: u64,
}
```
- `store::load_history() -> Result<Vec<HistoryEntry>>`, `store::append_history(&HistoryEntry)`, `store::clear_history()`, `store::compact_history(limit)`. Tolerate a corrupt line (skip it, keep the rest); a torn last line from a crash must not lose the file.
- Filtering is a pure function `history::filter(&[HistoryEntry], &HistoryQuery) -> Vec<&HistoryEntry>` so it is unit-testable with no UI. `HistoryQuery { text: String, database: Option<String>, connection: Option<Uuid>, since: Option<DateTime>, until: Option<DateTime>, only_errors: bool }`. Text match is case-insensitive substring on `sql`; if the text is valid regex-looking input, reuse whatever `text_filter.rs` already does for the sidebar so behaviour is consistent (check it first).

### 2. Recording — `src/ui/session/mod.rs`
- In `send`, capture `Instant` and the database name (`connection.database()`) before spawning; on the completion branch build a `HistoryEntry` from `Ok(Ok(results))` (sum `affected`, first result row count), `Ok(Err(e))` (error text), `Err(_)` (cancelled). Write it via `runtime::spawn`-free `std::fs` append on the GPUI thread inside a small helper (append is one short write) — or `cx.background_spawn` if profiling says otherwise. Failure to write logs a `tracing`/`eprintln` line and never surfaces an error to the user.
- A `Global` `HistoryStore(Vec<HistoryEntry>)` (same pattern as `settings::Settings`) holds the loaded entries so the dialog opens instantly and recording updates it in place. Load in `main.rs` next to `settings::init`.
- Respect `Settings::record_history`. Skip recording blank/whitespace-only buffers.

### 3. UI — `src/ui/history_dialog.rs` (new; follow `quick_switcher.rs` + `value_dialog.rs`)
- `HistoryView` entity, emits `HistoryEvent::{Dismissed, Open(String), Run(String)}`; the session subscribes and acts (rule: child emits, parent reacts).
- Layout: search `Input` (focused on open) + filter row (database `Select` populated from distinct values in the log, date range `Select`: Any time / Today / Last 7 days / Last 30 days, "Errors only" `Checkbox` with accessibility label) above a virtualized list, newest first. Each row: first line of SQL truncated, and a subtitle `connection · database · 2 min ago · 3 ms · 12 rows`. Errors show the word `Error:` and the message (not colour-only).
- Right pane or expanded row: full SQL, read-only, with buttons `Open in new tab` (default, Enter), `Run again` (`Cmd+Enter`, honours the connection's safety mode by going through `Session::run`, not `run_now`), `Copy`, `Delete entry`.
- Default scope: current connection; a "All connections" toggle widens it. Entries from another engine open but show a warning that syntax may differ.
- Empty state text when nothing recorded / nothing matches.
- Every icon-only button gets `.accessibility_label`; buttons with actions use `.tooltip_with_action`.

### 4. Wiring
- Actions: `ShowHistory` in `session/mod.rs` `actions!`; handler opens the dialog (guard with `window.has_active_dialog`, as quick switcher does).
- Keybinding in `src/keymap.rs`: `secondary-shift-h`, contexts `Session`, `QueryEditor > Input`, `QueryEditor`, `TableView`, `DataGrid > DataTable` (mirror the quick switcher's list, since the editor's `Input` claims keys first). Add to `shortcuts_dialog.rs`, README shortcuts, and `menu.rs` (`View > Query History…`).
- Quick switcher: add `SwitcherTarget::History` quick action.
- Toolbar/sidebar button with a clock icon, labelled "Query history".
- Settings window: `record_history` switch, `history_limit` number, "Clear history…" button (confirm dialog, since irreversible). Add fields to `Settings` with `#[serde(default)]`.
- Query editor: optional `Alt+Up/Down` to step through this connection's previous statements is out of scope; note in TODO as follow-up.

### 5. Docs
Tick TODO.md history item; README shortcut; CLAUDE.md "Persistence split" gets a sentence: history is `history.jsonl`, contains SQL text, never passwords.

## Critical files
- new: `src/db/history.rs`, `src/ui/history_dialog.rs`, `src/ui/tests/history.rs`
- edit: `src/db/{mod,store}.rs`, `src/settings.rs`, `src/ui/settings_window.rs`, `src/ui/session/mod.rs`, `src/ui/quick_switcher.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `src/main.rs`, `README.md`, `TODO.md`, `Cargo.toml` (only if `chrono` is missing)
- reuse: `runtime::spawn` abort/cancel path, `Session::run`, `text_filter`, `ScratchDir` fixture for a temp config dir.

## Testing
- `db::history` unit tests: round-trip a line, skip a corrupt line, torn last line, compaction keeps newest N, `filter` by text/date/database/errors.
- `src/ui/tests/history.rs` (add fixtures to `mod.rs`): running a query records one entry with the right outcome; an error run records `Error`; cancelled run records `Cancelled`; `record_history = false` records nothing; blank buffer records nothing; table paging records nothing; dialog filter narrows the list; `Open` creates a query tab with the SQL; `Run again` on a `ConfirmWrites` connection parks in `Status::Confirm`.
- Tests must point the store at a temp dir: make `config_dir()` honour an override (env var or `#[cfg(test)]` thread-local) so tests never touch the real config.
- Manual: `cargo run`, run a few queries, reopen dialog with `Cmd+Shift+H`, filter, restart app and confirm persistence.

## Risks / open questions
- Large pasted scripts inflate the log: truncate stored `sql` at e.g. 100 KiB with a marker.
- Sensitive SQL (`CREATE USER … PASSWORD`): document, provide the off switch; optionally skip logging statements whose first keyword is `CREATE/ALTER USER|ROLE`. Decide with the user.
- Shortcut `Cmd+Shift+H` may clash with a macOS system binding when the app is not the target of the menu; verify.
