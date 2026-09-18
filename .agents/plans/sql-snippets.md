# SQL snippets library

## Context
TODO.md "Query History & Snippets" lists a reusable SQL code snippets library. Users currently keep frequent queries in `.sql` files (`sql_file.rs` open/save) or retype them. Goal: named, tagged snippets stored locally, inserted into the editor from a searchable dialog, with optional `${placeholder}` fields. Shares the dialog pattern with [query-history.md](query-history.md); build history first or extract the shared list-dialog pieces when the second one lands.

## Decisions
- Scope: snippets are global (available on every connection) with an optional engine tag (`Any`, `Postgres`, `MySql`, `Sqlite`) so the picker can prefer matching ones. No per-connection scoping in v1.
- Storage: `snippets.json` in `store::config_dir()`, pretty-printed array so it is diff-able and hand-editable. Written atomically (write temp file, rename), since users may sync the config dir.
- Placeholders: `${name}` and `${name:default}` only. No shell/expression evaluation. Cursor marker `$0` places the caret after insertion. Escape a literal with `$$`.
- Insert, never auto-run. Running goes through the normal editor path, so safety modes are respected without any new code.

## Design

### 1. Data — `src/db/snippets.rs` (new; or `src/snippets.rs` if it should not live under `db`. Prefer `src/snippets.rs`: it is not database access)
```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct Snippet {
    pub id: Uuid,
    pub name: String,
    pub sql: String,
    #[serde(default)] pub description: String,
    #[serde(default)] pub engine: Option<Engine>,
    #[serde(default)] pub tags: Vec<String>,
}
```
- `load() / save(&[Snippet])` in `db/store.rs` style; missing file means none. On parse failure keep the broken file as `snippets.json.bak` and start empty rather than overwrite silently.
- `expand(sql: &str, values: &HashMap<String, String>) -> Expanded { text, caret: Option<usize> }` and `placeholders(sql) -> Vec<Placeholder { name, default }>` (ordered, de-duplicated). Pure, unit-tested.
- Ship a few starters on first run only if the file does not exist? Decision: no; empty state explains how to add. Avoid surprising content.
- Holder: `Global` `Snippets(Vec<Snippet>)`, loaded at startup like settings; mutations go through `snippets::update(cx, |list| …)` which saves (mirrors `settings::update`).

### 2. UI
- `src/ui/snippet_picker.rs` (new): dialog like `quick_switcher.rs`. `CommandState` search over name, description, tags, and SQL. Matching-engine snippets first. Enter inserts; a preview of the SQL sits beside or under the highlighted item. Footer buttons: `New`, `Edit`, `Delete`. Emits `SnippetEvent::Insert(String)`.
- `src/ui/snippet_editor.rs` (new): dialog to create/edit: name `Input`, description `Input`, engine `Select`, tags `Input` (comma separated), SQL multi-line editor (reuse the query editor's input styling/highlighting if `QueryEditor` exposes a reusable piece; else a plain multi-line `Input` with the editor font from `settings::editor_font`). Validation: name required and unique (case-insensitive); inline error text says `Error: …`. Save `Cmd+Enter`, cancel `Escape`.
- "Save selection as snippet…": `QueryEditor` action (`SaveSnippet`) takes the selection, or the statement at the caret via `statement::at_cursor`, and opens the editor dialog pre-filled. This is the main way users will create them.
- Placeholder form: when an inserted snippet has placeholders, show a small dialog with one `Input` per placeholder (defaults pre-filled, first focused), Enter to insert. No placeholders inserts immediately.
- Insertion: `QueryEditor::insert_text(text, caret, window, cx)` replaces the selection or inserts at the caret; if the active tab is a table tab, the `Insert` opens a new query tab first (`Session` handles it; the picker does not know about tabs).
- Accessibility: every icon-only button has `.accessibility_label`, action buttons use `.tooltip_with_action`. The list is keyboard-only reachable (`Command` component already is).

### 3. Wiring
- Actions in `query_editor.rs`/`session/mod.rs`: `InsertSnippet`, `SaveSnippet`. Keys in `keymap.rs`: `secondary-shift-s`? That is likely Save As on some platforms — check existing `SaveFileAs` binding and pick a free one; suggested `secondary-alt-s` for `InsertSnippet` and none for `SaveSnippet` (menu/context menu only, since it is rarer). Contexts: `QueryEditor > Input`, `QueryEditor`, `Session`.
- Menu (`menu.rs`): `Edit > Insert Snippet…`, `Edit > Save as Snippet…`. Quick switcher: `Insert Snippet…` action. Editor right-click menu: same two entries. Toolbar button with `.accessibility_label("Insert snippet")`.
- Shortcuts dialog, README, and `TODO.md` updated. CLAUDE.md gets a "Snippets" note under Persistence split.
- Settings: none in v1. Optional "Open snippets file" button to reveal `snippets.json` (also gives an export/backup path). Import/export dedicated UI is a follow-up.

### 4. Sequencing (each step compiles and tests green)
1. `snippets.rs` data + expand/placeholders + store round-trip + unit tests.
2. Global holder + `snippets::update`.
3. `QueryEditor::insert_text` + test.
4. Picker dialog + `InsertSnippet` action, keybinding, menu.
5. Editor dialog + `SaveSnippet` from selection.
6. Placeholder form.
7. Docs.

## Critical files
- new: `src/snippets.rs`, `src/ui/snippet_picker.rs`, `src/ui/snippet_editor.rs`, `src/ui/tests/snippets.rs`
- edit: `src/db/store.rs` (or a shared atomic-write helper it exposes), `src/ui/query_editor.rs`, `src/ui/session/mod.rs`, `src/ui/quick_switcher.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `src/main.rs`, `README.md`, `TODO.md`
- reuse: `statement::at_cursor`, `settings::update` pattern, `QuickSwitcherView` structure, `sql_file` for any file dialogs, `ScratchDir` fixture.

## Testing
- Unit: `placeholders` order/dedup/defaults, `expand` with `$0`, `$$`, missing values fall back to default then empty, nested-looking `${a${b}}` treated literally, unterminated `${` left as is; JSON round-trip; old file missing optional fields loads; corrupt file backed up.
- UI (`src/ui/tests/snippets.rs`, fixtures in `mod.rs`): saving selection creates a snippet and persists; duplicate name rejected with `Error:` text; picker search narrows by tag and SQL; insert replaces selection; caret lands at `$0`; placeholder form fills values; table tab insert opens a new query tab; delete removes it.
- As with history, the store path must be overridable so tests never touch the real config dir (share the override added there).
- Manual: `cargo run`, create from selection, insert via shortcut, restart and confirm persistence, hand-edit the JSON and confirm reload.

## Risks / open questions
- Two places to keep in sync if `snippets.json` is edited while the app runs; v1 loads once at startup. Add file watching only if asked.
- Shortcut choice needs a check against `keymap.rs` for conflicts.
- Placeholder syntax is one-way: once released, changing it breaks saved snippets. Confirm `${name:default}` with the user before building.
