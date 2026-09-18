# Welcome screen redesign and connection tags/colours

## Context
`src/ui/welcome.rs` (553 lines) is a two-column form: a 260px "SAVED CONNECTIONS" list on the left (ghost button per connection, engine dot, delete `x`), and an always-visible edit form on the right (engine buttons, name, host/port, user/password, database, safety mode buttons, Save/Connect footer). Problems:
- The screen opens on an empty "new connection" form even when connections are saved, so the common action (connect to something I already have) takes a selection plus a click, or a double click nobody is told about.
- One entity is both launcher and editor; the form is shown whether or not the user wants to edit anything.
- Nothing separates a production database from a local one. The only per-connection signal is the engine dot, and safety mode is invisible once connected. A wrong-database write is the failure this app most needs to prevent.
- No search, grouping, or ordering; the list is in file order.
- Delete is one click with no confirmation, next to the connect target.

Goal: launcher-first Welcome screen, editor in a separate dialog, and a per-connection tag with a colour that follows the connection into the workspace tab and session so it is always visible.

## Decisions
- Launcher first: saved connections shown as a searchable card grid/list; the form moves into a dialog ("New connection" / "Edit connection"). Empty state (no connections) shows a single call to action that opens the dialog.
- Tag model: one **tag per connection** with a **colour**, not many tags. Rationale: the goal is an unmistakable environment signal (Production/Staging/Local), and one colour per connection keeps the header unambiguous. A free-form multi-tag list is a follow-up if wanted.
  - `ConnectionConfig::tag: Option<String>` (free text, e.g. "Production") and `ConnectionConfig::color: Option<TagColor>`. Both `#[serde(default)]`, so old `connections.json` files read back as untagged.
  - `TagColor` is a fixed palette enum (Red, Orange, Yellow, Green, Teal, Blue, Purple, Pink, Gray), serialised as a kebab-case string. A palette instead of arbitrary hex: colours can be resolved against the theme for contrast in light and dark, and stay valid if a theme changes. Custom hex is out of scope.
  - Quick-pick presets in the editor: Production (red), Staging (orange), Development (blue), Local (green). Selecting one fills tag and colour; both can then be edited independently.
- Colour is never the only cue (project rule): the tag's text is always shown next to its colour, and the connection tab/header shows the tag label, not a bare colour.
- Should ReadOnly/safety mode auto-tie to colour? No. Independent settings; but the editor shows a non-blocking hint when tag is "Production"-like and safety is `AutoApply` ("Writes apply immediately on a production connection"). Decide wording with the user.
- Sort: pinned by nothing in v1; default order = most recently connected first, then name. Add `last_connected: Option<DateTime<Utc>>` to the config, set on successful connect (`#[serde(default)]`). Group-by-tag view toggle is a stretch goal (see below).

## Design

### 1. Data — `src/db/config.rs`, `src/db/store.rs`
- Add `tag`, `color`, `last_connected` to `ConnectionConfig` with `#[serde(default)]`; `TagColor` enum with `ALL`, `label()`, `key()`.
- `TagColor::hsla(cx)` (in `src/ui/mod.rs` next to `engine_color`): map each variant onto the theme's semantic colours where they exist (`danger`, `success`, `warning`, `info`/`primary`) and to fixed hues elsewhere, adjusted for light/dark. Provide `on_color(cx)` for text drawn on top; verify contrast of at least 4.5:1 against every bundled theme in a test that iterates `settings::themes_for`.
- `store::save` unchanged in shape; add `record_connected(id)` helper that updates `last_connected` and saves. Passwords are still never in the JSON.
- Round-trip tests: a file with no new fields loads with `None`s; a tagged config serialises and reads back.

### 2. Screen structure — split `welcome.rs` into a directory (`welcome/`), matching how `session/` and `data_grid/` are laid out
- `welcome/mod.rs`: `Welcome` view (launcher) and `WelcomeEvent::Connected` (unchanged contract with `Workspace`).
- `welcome/card.rs`: renders one connection.
- `welcome/editor.rs`: `ConnectionEditor` entity (the form, moved out of `Welcome`), emits `EditorEvent::{Saved(ConnectionConfig), Connect(ConnectionConfig), Dismissed}`; `Welcome` subscribes (child emits, parent reacts).
- Keep the existing tests' entry points (`set_connections_for_test`, `show_error_for_test`) working, or update them in the same commit.

### 3. Launcher layout
- Header: app name and one line of help; right side: `New connection` primary button (`Cmd+N`? check `NewConnection` in `app.rs`/`keymap.rs` — a `NewConnection` action already exists, wire it to open the editor).
- Search `Input` (autofocus when there are connections), filtering by name, host, database, tag, engine text; empty result says so. `Enter` connects to the first match, arrow keys move through results (use gpui-kit `List`/`Tree` for rows so ARIA role and arrow-key handling come free, as the sidebar does; fall back to a hand-rolled focus list only if `List` cannot render the card).
- Each card/row: left edge a 4px bar in the tag colour; title (display name); subtitle `host:port / database` (or the file path for SQLite, middle-truncated); engine label with its dot; a tag chip (`Production` in the colour, text always present); "Connected 2 h ago" when known. Untagged connections show no chip and a neutral bar.
- Primary action: clicking the card (or `Enter`) connects. Secondary actions in a trailing `…` menu (`Button` with `.accessibility_label("Actions for <name>")`): Edit, Duplicate, Delete. Delete asks for confirmation in a dialog naming the connection; no one-click delete.
- Connecting state: card shows "Connecting…" and is disabled; errors appear in an inline banner at the top of the screen as `Error: …` (retain the scrollable max-height behaviour of the current footer), not in a footer far from the card that failed.
- Sections: "Recent" (last 3 connected, only when more than ~6 saved) and "All connections". Skip if it adds clutter at small counts.
- Layout must hold at the minimum window width: single column below ~640px.
- Empty state: centred illustration-free text ("No connections yet"), `New connection` and, for SQLite, an `Open SQLite file…` shortcut that pre-fills the editor via `sql_file::prompt_for_open` (check it fits; otherwise cut).

### 4. Editor dialog (`welcome/editor.rs`)
- Opened with `window.open_dialog` like `value_dialog`/`shortcuts_dialog`; focus first field; `Escape` closes; `Cmd+Enter` = Connect; Save button; validation errors say `Error: …`.
- Sections top to bottom: Engine (segmented buttons, as now), Name, connection fields (unchanged fields and file-based variant), **Label** (tag `Input` with preset chips Production / Staging / Development / Local, then a colour swatch row), Safety mode (as now, plus the hint above).
- Colour swatches: a row of small `Button`s, each with `.accessibility_label("Red")` and a selected state shown by a check icon inside the swatch (not only a ring), plus "None". Selected colour previews live in a small header strip at the top of the dialog so the user sees how the connection will look.
- Reuse `Welcome::config`/`set_engine`/`load`/`save` logic by moving it, not rewriting; port the existing port-default and safety behaviour verbatim.

### 5. Carry the colour into the app — `src/app.rs`, `src/ui/session/`
- Workspace tab (`render_tab_bar`): tab already supports `tab.icon_color`; add a small filled tag chip or a coloured bottom border on the tab plus the tag text in the label (e.g. `Prod DB · Production`). The selected-tab case must remain readable in every theme.
- Session header: a slim full-width strip under the toolbar in the tag colour with the tag name (and a lock icon plus "Read only" when the safety mode is `ReadOnly`). This is the "Red header for Production" requirement. Height ~24px, text on top via `on_color`, always visible while the session is active.
- Sidebar/quick switcher: show the tag chip on the database list row when several connections are shown; optional.
- Data flows through `Connection::config`, so no new plumbing across the DB layer.
- Untagged connections render as today (no strip).

### 6. Accessibility
- Every icon-only button (`…`, swatches, close) has `.accessibility_label`; buttons with a bound action use `.tooltip_with_action`.
- Colour is always paired with the tag text; swatch selection is shown by an icon.
- Cards are reachable by keyboard and expose name + tag + engine in their label ("Prod DB, Postgres, Production").
- Verify tab order: search, New connection, list, then each card's menu.

### 7. Sequencing (each step compiles and tests green)
1. Data model (`tag`, `color`, `last_connected`) + serde tests + `TagColor` palette and contrast test.
2. Extract `ConnectionEditor` into `welcome/editor.rs` as a dialog with current fields only; launcher opens it. No visual redesign yet; existing tests pass.
3. Launcher cards, search, ordering, Edit/Duplicate/Delete-with-confirm.
4. Tag + colour controls in editor, chips on cards.
5. Workspace tab chip and session header strip.
6. Empty state, responsive single column, polish.
7. Docs.

## Critical files
- new: `src/ui/welcome/{mod,card,editor}.rs` (replaces `welcome.rs`), `src/ui/tests/welcome.rs`
- edit: `src/db/config.rs`, `src/db/store.rs`, `src/ui/mod.rs` (colour helper), `src/app.rs`, `src/ui/session/mod.rs`, `src/keymap.rs`, `src/ui/shortcuts_dialog.rs`, `src/ui/tests/{mod,workspace}.rs`, `README.md`, `TODO.md`, `CLAUDE.md` (architecture note: Welcome is a directory; tags/colours are metadata in `connections.json`)
- reuse: `engine_color`, `SafetyMode::{ALL,label,description}`, `Engine::default_port`, `store::{load,save,set_password}`, `sql_file::prompt_for_open`, dialog pattern from `value_dialog.rs`.

## Testing
- Unit: config serde compatibility, `TagColor` keys, `last_connected` ordering, search matching function (pure, in `welcome/mod.rs`).
- UI (`src/ui/tests/welcome.rs`; fixtures in `mod.rs`): empty state shows New connection; saved connections listed most-recent first; search narrows by tag and host; card click connects (existing connect test path); Edit opens editor pre-filled; saving with tag/colour persists and re-renders chip; Duplicate creates a new id without copying the password; Delete requires confirmation; error banner reads `Error: …`.
- Workspace: tagged connection's tab title includes the tag; session header strip present for a tagged connection and absent for untagged; ReadOnly shows the lock label.
- Contrast test over all bundled themes for tag chip/strip text.
- Tests must not touch the real config dir or keychain (reuse the config-dir override planned in query-history.md).
- Manual: `cargo run` in light and dark themes with one connection per palette colour; check narrow window; keyboard-only run through search, connect, edit; VoiceOver pass on the cards.

## Risks / open questions
- Ask the user: one tag per connection (proposed) vs multiple tags, and whether colour presets should be auto-suggested from the tag name ("prod" → red) or only via the preset chips.
- `NewConnection` action currently exists at workspace level (opens a Welcome tab); its meaning may need to change to "open editor dialog" versus "open new Welcome tab". Decide before step 3.
- gpui-kit `List` may not support rich card rows or a coloured leading bar; step 3 may need a custom focusable row. Check gpui-kit docs (llms.txt) first.
- Tab bar styling in `TabVariant::Outline` may not allow per-tab colour; if not, fall back to a coloured prefix chip rather than restyling the bar.
- A strip in every session costs vertical space; make it a setting only if users complain.
- Production-like colour with `AutoApply` hint wording is a judgment call; confirm.

## Follow-ups (not in this plan)
Group-by-tag view, pinning/favourites, multiple tags, custom hex colours, import/export of connections, connection URL paste ("postgres://…" fills the form).
