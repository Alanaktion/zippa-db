# Right-column single-row edit panel

## Context

TODO2.md item: "Add right-column single-row edit UI with column filter matching table filter behavior." Wide tables are awkward to edit cell-by-cell in the grid; a side panel showing one row as a vertical list of `column: value` fields (a master/detail view) is easier to scan and edit, and a live filter over the field list helps find a column in a wide table quickly.

Decisions confirmed with the user:
- **Scope**: a resizable split *inside* `TableView`, next to the grid — not a session-wide dock (the app has no left/right/bottom dock today, only a center tab dock; adding one would touch every tab type for a feature only tables need).
- **Row tracking**: the panel always mirrors whatever row currently has keyboard focus in the grid — no separate "open row" step.
- **Column filter**: matches the *sidebar's* live filter behavior (`Session::on_filter_event` / `compile_filter` in `src/ui/session/sidebar.rs`) — case-insensitive regex with literal-text fallback, live on every keystroke — applied to column names, not `FilterBar`'s structured column+operator+value predicate builder (that one builds SQL `WHERE` clauses and doesn't fit "narrow a field list").
- **Writes**: fields in the panel stage into the exact same overlay `DataGrid` cell edits use (`ResultDelegate::stage`), so they get the grid's own green-tint/underline treatment and are written by the existing Apply/Cmd+S/auto-apply flow. No new write path.

## Approach

### 1. `DataGrid` surface changes (`src/ui/data_grid/mod.rs`, `delegate.rs` untouched — all needed delegate methods already exist as `pub(super)`)

- New `GridEdit::RowFocused(Option<usize>)` variant, emitted unconditionally from `on_table_event` (mod.rs:350-358, alongside the existing `focused_row` update) and from `set_sorted_result` (mod.rs:983-995) / `clear` (mod.rs:1012-1019), which currently reset `focused_row` to `None` silently — the panel needs to hear about that reset (paging/sorting/filtering all go through `set_sorted_result`).
- Rename `stage_value(&mut self, row_ix, col_ix, value: String, cx)` (mod.rs:811-820) to `pub fn stage_cell(&mut self, row_ix, col_ix, value: Cell, cx)`, taking `Option<String>` so `None` (NULL) can be staged directly, not just typed text. Update the one call site, `view_cell`'s `Save` closure (mod.rs:757-790), to wrap in `Some(...)`.
- `pub fn focused_row(&self, cx: &App) -> Option<usize>` — promote the read already used by `#[cfg(test)] selection_for_test` (mod.rs:1176-1182).
- `pub fn row_cells(&self, row_ix: usize, cx: &App) -> Option<Vec<Cell>>` — `None` if out of range, else the merged baseline+edits+draft values via `delegate.cell(row_ix, col_ix)` for each column (this is the accessor `baseline_row`, mod.rs:904-906, deliberately does *not* provide, since it's used for write-keying and must stay unedited).
- `pub fn is_field_editable(&self, row_ix, col_ix, cx: &App) -> bool` — thin wrapper over `delegate.is_editable` (delegate.rs:829-855).
- `pub enum RowStatus { Loaded, Draft, Deleted }` + `pub fn row_status(&self, row_ix, cx: &App) -> Option<RowStatus>` — via `delegate.draft`/`delegate.is_deleted` (delegate.rs:455, 606).

### 2. Shared filter helper: `src/ui/text_filter.rs` (new)

Extract `compile_filter`'s body (sidebar.rs:246-259: case-insensitive `Regex`, literal-escape fallback on parse failure) into `pub(crate) fn compile(pattern: &str) -> Option<Regex>`. Register `mod text_filter;` in `src/ui/mod.rs`. Have `sidebar::compile_filter` forward to it (one line) so `test_support.rs`'s existing import keeps working. The new row panel calls `text_filter::compile` directly.

### 3. New entity: `src/ui/table_view/row_panel.rs`

```rust
pub(crate) enum RowPanelEvent { CloseRequested }

pub(crate) struct RowPanel {
    grid: Entity<DataGrid>,
    columns: Vec<String>,
    column_types: Vec<String>,
    focused_row: Option<usize>,
    fields: Vec<Entity<InputState>>,   // index-aligned with `columns`
    filter_input: Entity<InputState>,
    filter: Option<Regex>,
}
```

- `RowPanel::new(grid: Entity<DataGrid>, window, cx)`: builds `filter_input` (subscribed to `InputEvent::Change`, live filtering — no Enter-gating), subscribes to `grid`'s `GridEdit` via `cx.subscribe_in`, seeds `focused_row` from `grid.read(cx).focused_row(cx)`.
- `pub(crate) fn sync_columns(&mut self, columns: &[String], column_types: &[String], window, cx)`: called from `TableView::reload`'s success branch right beside the existing `this.filters.update(cx, |f, cx| f.set_columns(&columns, cx))` call (mod.rs:369-371). Rebuilds `fields`, one `InputState` per column, each subscribed with a closure capturing its `col_ix` (same idiom as sidebar's tree-row closures). Ends by calling `reseed`.
- `on_grid_edit`: `RowFocused(row) => { self.focused_row = row; reseed(); }`, `Staged => reseed()`, `RowLeft => {}`.
- `reseed(window, cx)`: if no focused row, nothing to show. Else `grid.row_cells(row_ix, cx)`; for each column's field, **skip any field that currently has keyboard focus** (so mid-keystroke text isn't clobbered by another field's edit event), otherwise `input.set_value(cell.clone().unwrap_or_default(), ...)` — raw text, matching the grid's own inline-editor convention (a NULL opens empty, not the string `"NULL"`).
- Render: header (filter `Input`, `.small().cleanable(true)`, matching the sidebar's exactly; a ghost icon-only close `Button` with `.accessibility_label("Hide row panel")` emitting `CloseRequested`), then by `grid.row_status`:
  - `None` (no row focused) → empty state text, matching the grid's own empty-state style.
  - `Deleted` → every field read-only + struck through + a text banner "Row marked for deletion" (never color alone).
  - `Draft` → fields editable, "New row" badge in words.
  - `Loaded` → normal editable fields, filtered live by `filter` against column name (name only, matching the sidebar precedent — not `column_types`).
  - Each field: label, `Input` (`.readonly(true)` when `!is_field_editable`, with the same reason text `view_cell` already uses — "This value cannot be edited here." etc.), a NULL `Checkbox` (`.accessibility_label("Set {column} to NULL")`) that calls `grid.stage_cell(row_ix, col_ix, None, cx)` directly, and — when `needs_a_window` (from `data_grid::format`) is true for that cell — an expand icon-button that calls the *existing* `grid.view_cell(row_ix, col_ix, cx)` rather than duplicating pretty-print/JSON layout.
  - Field commit (Enter/Blur): read text, apply the same null-literal coercion `commit_editor` already does (mod.rs ~502-507: `Settings::global(cx).coerce_null_literal` + case-insensitive `"null"` match), then `grid.stage_cell(row_ix, col_ix, value, cx)`.

### 4. `TableView` wiring (`src/ui/table_view/mod.rs`)

- New fields: `row_panel: Entity<RowPanel>`, `row_panel_visible: bool` (default `true`), `columns_pane: Entity<ResizableState>` (own state — `panel.rs`'s existing `panes: Entity<ResizableState>` field, used for the query tab's editor/grid split, is the direct precedent, mod.rs:69,104 in panel.rs).
- Build `row_panel` in `TableView::new`, subscribe to its events; `on_row_panel_event` handles `CloseRequested` by setting `row_panel_visible = false`.
- In `reload()`'s success branch, call `row_panel.update(cx, |p, cx| p.sync_columns(&this.columns, &this.column_types, window, cx))` beside the `filters.update` call (mod.rs:369-371).
- New action `ToggleRowPanel` (added to the existing `actions!` block), bound in `render()`.
- `render()` (mod.rs:1167-1186): replace `.child(div().flex_1().min_h_0().child(self.grid.clone()))` (line 1180) with, when `row_panel_visible`, an `h_resizable("table-view-columns")` of `resizable_panel()` (grid) + `resizable_panel().size(px(280.)).size_range(px(220.)..px(480.))` (row panel); otherwise the grid alone as today. `ResizableState`/`resizable_panel`/`h_resizable` are already used this way in `panel.rs` — same import needed here.
- `render_footer` (mod.rs:972+) gets one more persistent icon `Button` (e.g. `IconName::PanelRight` or nearest match) — `.accessibility_label("Toggle row detail panel")`, `.tooltip_with_action(...)` — unconditionally rendered (it's the only way back in once the panel's own close button hides it, satisfying "every setting needs a way in that is not a keystroke").
- `src/keymap.rs`: bind `ToggleRowPanel` to an unused chord (e.g. `secondary-\`) in the `"TableView"` context.

### 5. Tests: `src/ui/tests/row_panel.rs` (new, registered in `tests/mod.rs`)

Cover: focus-follow (selecting a different cell updates the panel; clearing selection empties it), write-through (typing + Enter stages exactly like a grid edit, verify via `grid.staged(cx)`), no-clobber (typing into an unfocused-elsewhere field doesn't stomp a field mid-edit), NULL (checkbox stages `None`, box reads empty after), filter (live substring/regex narrowing, literal fallback on bad pattern), draft row (all fields editable, writes land in `drafts` not `edits`), deleted row (read-only + struck-through + banner), read-only table/column (reason text shown), toggle (footer button + panel's own close button + `ResizableState`/full-width fallback). Add `#[cfg(test)]` reach-ins on `TableView` (`row_panel_for_test`) and `RowPanel` (`focused_row_for_test`, `field_value_for_test`, `filter_pattern_for_test`) following the existing `grid_for_test`/`filters_for_test` style (mod.rs:1188+).

## Known edge case (accepted, not solved)

Discarding/deleting a draft row via the row menu (delegate.rs ~567-603) calls `table.refresh(cx)` directly rather than emitting `GridEdit::Staged` (it only holds a weak handle to the table, not the grid) — pre-existing behavior. If the panel is showing that exact draft row, its field values can lag by one interaction until the next `GridEdit`. Narrow enough (requires the row-menu path specifically on the currently-shown row) to leave as a follow-up rather than widening scope here.

## Critical files

- `src/ui/data_grid/mod.rs` — new event variant, `stage_cell` rename, new `pub` accessors
- `src/ui/data_grid/delegate.rs` — read-only, confirms existing `pub(super)` methods are sufficient
- `src/ui/table_view/mod.rs` — panel field, layout split, footer toggle button, action/keymap wiring
- `src/ui/table_view/row_panel.rs` (new) — the panel entity
- `src/ui/session/sidebar.rs` — `compile_filter` becomes a forward to the new shared helper
- `src/ui/text_filter.rs` (new) — extracted filter-matching helper
- `src/keymap.rs` — `ToggleRowPanel` binding

## Verification

- `cargo test` — new `row_panel` test file plus existing `data_grid`/`table_view`/`sidebar` suites (confirm the `stage_value` → `stage_cell` rename and `compile_filter` extraction don't break existing call sites/tests).
- `cargo run` — open a table with a wide row, confirm: panel mirrors grid selection live; typing a field tints the corresponding grid cell green/underlined; Cmd+S applies it; NULL checkbox round-trips; filter box narrows fields live including on an invalid-regex pattern; footer toggle and the panel's own close button both work; a read-only connection shows every field read-only with a stated reason; tab through the panel to confirm keyboard reachability (checkboxes/buttons carry accessibility labels).
