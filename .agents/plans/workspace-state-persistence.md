# Workspace state persistence: reopen previous tabs on startup

## Context

TODO.md backlog item: "Workspace state persistence (reopen previous tabs on startup)". Decisions confirmed with the user:

- **Auto-reconnect.** On launch, every connection that was open at quit is reopened and connected right away (password from the keychain), then its tabs are restored.
- **Flat tab list.** Persist which tabs each session had, their order, and which was active. Always restore into a single tab strip; no split or pane-size geometry.
- **Full SQL text.** A query tab's buffer is restored as typed, including unsaved text, plus its bound file path if any.

Findings that shape the design:

- `gpui-kit` has `DockArea::dump`/`load` and a `PanelRegistry`, but using them means `SessionPanel` implementing panel-level serialization and a registry builder that can build a panel before its connection exists. That is the "full geometry" option, which was declined; the flat list needs none of it.
- `Welcome::open(id, connect = true, ..)` (`src/ui/welcome.rs:110`) already loads a saved connection (including its keychain password) and connects, then emits `WelcomeEvent::Connected`. Auto-reconnect can drive that same path, so `Workspace::on_welcome_event` turns the tab into a session exactly as for a manual connect. A failed reconnect leaves an ordinary Welcome tab showing `Error: ...`, which is the manual fallback for free.
- The app is single-window (`src/main.rs`), so there is one `Workspace` and one state file.
- Nothing in the editor emits per-keystroke events, and `SessionPanel::is_dirty` already reads the buffer on demand. The plan follows that idiom: no change events on typing, and state is written at checkpoints instead (see section 4).

## 1. `src/workspace_state.rs` (new, crate root beside `settings.rs`)

Persistence for `workspace.json` in `db::store::config_dir()` (already `pub(crate)`; `settings.rs` reuses it the same way). No passwords, ever; only connection ids, which are already in `connections.json`.

```rust
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceState {
    pub version: u32,
    /// Index into `sessions` of the workspace tab that was in front.
    pub active: usize,
    /// One per connection tab, in tab order.
    pub sessions: Vec<SessionState>,
}

pub struct SessionState {
    pub connection: Uuid,
    /// Database the session was on, since the user may have switched from the config's.
    pub database: Option<String>,
    pub active: usize,
    pub panels: Vec<PanelState>,
}

pub enum PanelState {
    Query { title: String, sql: String, file: Option<PathBuf> },
    Table { object: DatabaseObject },
    Schema { object: DatabaseObject },
}
```

- `pub fn load() -> Result<WorkspaceState>` (missing file is `Default`) and `pub fn save(&WorkspaceState) -> Result<()>`, same shape as `store::load/save` and `settings::save`. Under `#[cfg(test)]` `save` is a no-op, for the reason `settings::save` gives (a test run must not rewrite the developer's file).
- Write to a temp file in the same directory, then rename, so a kill mid-write cannot leave a truncated file. A file that fails to parse is reported with `eprintln!` (as `settings::init` does) and treated as empty; it must never stop the app starting.
- `#[serde(default)]` on every struct and a `version` field so an older or newer file still reads.
- Derive `Serialize`/`Deserialize` on `DatabaseObject` and `ObjectKind` (`src/db/connection.rs:35-47`); they derive neither today.
- Register `mod workspace_state;` in `main.rs`.
- Inline `#[cfg(test)] mod tests`: round trip; a file with only `{}` loads; an unknown field is ignored; a garbage file yields an error the caller can turn into `Default`.

## 2. Reading a snapshot from live state

- `SessionPanel` (`src/ui/session/panel.rs`): `pub(crate) fn snapshot(&self, cx: &App) -> PanelState`. A query tab reads `editor.read(cx).sql(cx)`, `title`, and `file_path()`; a table or schema tab reads `object(cx)` and `mode()`.
- `Session` (`src/ui/session/mod.rs`): `pub(crate) fn snapshot(&self, cx: &App) -> SessionState`, from `connection.config.id`, `connection.database()`, `active_tab_index()`, and `panels()` in creation order.
- `Workspace` (`src/app.rs`): `fn snapshot(&self, cx: &App) -> WorkspaceState`, walking `tabs`. Only `TabContent::Session` tabs are recorded. A blank Connect tab is scratch form state and is not saved, so it is not restored, and `active` is remapped to the session index. A Connect tab that still holds a **pending restore** (section 3) is recorded from that pending state, so a failed reconnect does not erase the user's tabs from the file.

## 3. Restoring at startup (`src/app.rs`)

- `Workspace` gains `pending: HashMap<EntityId, SessionState>`, keyed by the Welcome entity that is connecting, and drops the `open_connect_tab()` call in `new` when there is something to restore.
- `Workspace::new` calls `workspace_state::load()`. For each `SessionState` whose `connection` is still in `store::load()`, push a `TabContent::Connect` built by the existing `Self::welcome(window, cx)`, record `pending[welcome.entity_id()] = state`, and call `welcome.update(cx, |w, cx| w.open(id, true, window, cx))`. Make `Welcome::open` `pub(crate)`. Set `self.active` from the saved index (clamped). A connection deleted since is skipped. If nothing at all was restorable, fall back to the single blank Connect tab as today.
- `on_welcome_event`: after `Session::new`, remove the `pending` entry for `welcome` and, if present, call a new `session.update(cx, |s, cx| s.restore(state, window, cx))` before the tab is swapped in.
- `Session::restore`: replay panels in order with the existing constructors (`open_tab` for a query with `title` / `sql`, and `set_file` when it had a path; `open_object` for Table and Schema), so the panel-key and `opened` numbering stay consistent. Then close the default empty "Query 1" tab that `Session::new` opened, but only if at least one panel was restored (`close_tab_now` already handles the "always keep one" rule, so opening first and closing after keeps that invariant). Then `activate_tab(state.active)`. If `state.database` differs from `connection.database()` and the engine is not file-based, call `switch_database` last.
- Restored query tabs are **not** run, and restored table tabs load their first page as they do when opened from the sidebar. A restored query tab's `baseline` stays the empty string, so a restored unsaved buffer shows as dirty and asks before closing, which is correct. For a tab bound to a file, do not re-read the file: the saved buffer text wins, because it may hold unsaved edits. Set `baseline` from the file only if that cheap read succeeds and matches; otherwise leave it dirty.
- Table and schema tabs whose object no longer exists show the view's own load error; no special handling.

## 4. Writing state: checkpoints, not keystrokes

`Workspace::save_state(&self, cx)` builds a snapshot and writes it with `cx.background_spawn(...)` (small file, but never block the UI thread). Skip the write when the snapshot equals the last one written (keep it in a field).

Call it from:

- `Workspace`: `open_connect_tab`, `close_tab`, `activate_tab`, `on_welcome_event`, and `on_session_event`.
- `Session` reports its own changes with a new `SessionEvent::Changed`, emitted from `install`, `forget`, `close_tab_now`, `touch`, `send` (a run is a natural checkpoint for the buffer text), `write` (file path and baseline change), and `switch_database`. `Workspace::on_session_event` currently does `let SessionEvent::Disconnected = event;` (irrefutable); turn it into a `match` and call `save_state` on `Changed`.
- App quit: in `main.rs`, clone the `Entity<Workspace>` before handing it to `Root::new` and register `cx.on_app_quit(move |cx| { workspace.update(cx, |w, cx| w.flush_state(cx)).ok(); std::future::ready(()) })`. `flush_state` writes **synchronously** (the process is about to exit), and this is what captures the final, freshest buffer text.

Must not clobber during startup: a save fired while some connections are still connecting is safe only because section 2 folds pending restores into the snapshot.

## 5. Deviations and limits (stated, not silent)

- **No split/pane geometry.** Flat list only, as chosen.
- **Buffer text is checkpointed.** Text typed since the last checkpoint is lost only if the process is killed or crashes; a normal quit flushes it. Autosaving on a timer was rejected: it adds a polling loop and steady disk writes for a crash-only benefit.
- **Not restored:** blank Connect tabs and their half-typed forms, result grids, filters, sort, page, scroll position, staged grid edits, and unapplied schema edits. Quitting already discards the last three today.
- **SQL text is stored in plaintext** in the config dir, like `connections.json` metadata. It can contain literals the user pasted. Call this out in the README so it is not a surprise; no opt-out setting in this change.
- **Auto-connect on launch.** The connection's `SafetyMode` is unchanged and still governs writes, but a production server is now contacted without a click. A keychain prompt may appear at launch, as it does for a manual connect.

## 6. Tests

- `src/workspace_state.rs`: unit tests above.
- New tests in `src/ui/tests/workspace.rs` (the file matching the area), with any new fixture in `tests/mod.rs`: build a `Workspace` from an injected `WorkspaceState` (add a `Workspace::with_state_for_test`, since `load()` must not read the developer's real file), using a SQLite connection fixture. `runtime::spawn` runs inline under test, so the connect completes deterministically. Assert:
  - tabs come back in order with the right titles, SQL text, and active tab;
  - the default empty "Query 1" tab is gone when panels were restored, and kept when the state had none;
  - a state naming a deleted connection is skipped;
  - a failed connect leaves a Connect tab, and its pending tabs still appear in `snapshot()`;
  - `snapshot()` after opening a table, editing a buffer, and switching tabs round-trips through `restore`.
- `cargo test workspace_state` then `cargo test`.

## 7. Docs

- Check off the item in `TODO.md`.
- `CLAUDE.md` "Persistence split": add `workspace.json` (tabs and buffers, no passwords) and the checkpoint rule (`SessionEvent::Changed` plus quit flush; nothing on keystrokes).
- README: one line on restore behaviour and the plaintext-SQL note.

## Verification

- `cargo run`: connect to a SQLite file and a Postgres server, open a table tab, a structure tab, and two query tabs (one saved to a `.sql` file, one unsaved with text), switch to the second connection, quit with `Cmd+Q`, relaunch. Expect both connections, all tabs in order, the unsaved text, and the same active tabs.
- Relaunch with the Postgres server stopped: that tab shows the connection error; start the server, click Connect, and its tabs appear.
- Delete `workspace.json` and a saved connection by hand: app starts on a blank Connect tab without errors.
