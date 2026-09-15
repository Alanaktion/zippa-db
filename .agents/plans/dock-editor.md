# Dock-based session panels (query/table tabs → gpui-kit Dock)

## Context

Today `Session` (`src/ui/session/mod.rs`) shows exactly one tab's content at a
time behind a hand-rolled `TabBar`. The ask: let a user drag a query or table
tab to split the view, so e.g. two tables can sit side-by-side. gpui-kit ships
a `Dock` component (`gpui_kit::component::dock::*`) built exactly for this —
`DockArea` holds a tree of tab groups and splits, each leaf a "panel" entity,
with native drag-to-split/drag-to-reorder already implemented.

Two scope decisions confirmed with the user:
- **No persistence** of open tabs/layout across app restart — matches today
  (every reconnect starts with one blank query tab). Skips the
  `register_panel`/serialize-per-panel-state machinery entirely.
- **Center region only** — no left/right/bottom dock slots. The object
  sidebar stays exactly as it is today, its own `h_resizable` pane, untouched.

**Dependency note**: `Cargo.toml` pins `gpui-kit = "0.6"` from crates.io
(0.6.1, `source = "registry+..."`), not the local
`~/Developer/Repositories/gpui-kit` checkout — the user confirmed
that checkout is an unrelated reference clone they don't maintain, so the
plan targets the **published 0.6.1 API**, verified directly against
`~/.cargo/registry/src/index.crates.io-*/gpui-{base,component}-0.6.1`. That
API is structurally the same dock (same file layout: `dock.rs`, `panel.rs`,
`tab_panel.rs`, `invalid_panel.rs`, `mod.rs`, plus an unrelated `tiles.rs`
feature this plan doesn't use) with one gap: **there is no
`DockArea::select_panel`** in 0.6.1. §3 below has the verified workaround.

## Architecture shift

Business logic currently keyed by `Session.tabs[Session.active]` moves down
into two panel entities, because Dock panels must work focus-independently —
once two query tabs can be visible at once, "the active tab" stops being a
coherent index into one `Vec`. `Session` becomes a thinner shell: it owns the
`DockArea`, cross-cutting operations (open/close/switch-database), and a
"focused panel" resolver for the handful of actions that still need a target.

### 1. `QueryTab` — new entity, replaces `TabContent::Query`

New file `src/ui/session/query_tab.rs` (replaces `src/ui/session/tab.rs`;
keep the `Status` enum, drop `TabContent`/`SessionTab`).

```rust
pub(crate) struct QueryTab {
    session: WeakEntity<Session>,       // connection + safety mode, read at call time
    dock_area: WeakEntity<DockArea>,    // for self-removal
    focus_handle: FocusHandle,
    panes: Entity<ResizableState>,      // OWN state — do not share across tabs (see note)
    title: SharedString,
    editor: Entity<QueryEditor>,
    grid: Entity<DataGrid>,
    status: Status,
    path: Option<PathBuf>,
    results: Vec<QueryResult>,
    result: usize,
    running: Option<tokio::task::AbortHandle>,
    baseline: String,
    pending_close: bool,                // replaces Session.closing: Option<usize>
}
```

**Why `panes` moves per-tab**: today one `ResizableState` is shared across
all tabs "so the split stays put" — safe only because exactly one tab ever
rendered. With splits, two `QueryTab`s render simultaneously and would fight
over one state's sizes. This is a deliberate, visible behavior change (the
editor/grid divider becomes per-tab) — call it out, don't silently fix it.

Constructor takes `(session: WeakEntity<Session>, dock_area: WeakEntity<DockArea>, title, sql, window, cx)`.
It builds `editor`/`grid` as today, and moves nearly verbatim onto `self`
(scoped by `self` instead of an `index` parameter) everything currently on
`Session`: `run`, `run_script`, `confirm_run`, `cancel_run`, `run_now`,
`send`, `set_running`, `show_results`, `show_result`, `result_summary`,
`cancel_query`, `save`, `write`, `set_file`, `set_baseline`, `set_status`,
`is_dirty`, and the render helpers `render_status_bar`, `render_result_bar`,
`render_close_confirm` (mod.rs:1019-1152 today) — **keep every element id
byte-identical** (`confirm-run`, `cancel-run`, `result-N`, `keep-tab`,
`close-tab-anyway`), tests click them by id.

In the constructor, `cx.subscribe_in(&editor, window, Self::on_editor_event)`
replaces `Session::on_editor_event` + `query_tab_of` lookup (mod.rs:616-642) —
the tab now owns its editor directly, so routing `QueryEditorEvent::Run/
RunScript/Save` needs no lookup. `QueryEditorEvent::Open` still calls back
into `Session` (`self.session.update(cx, |s, cx| s.open_file(cx))`) since
opening a file creates a *new* tab.

Read the connection through `self.session.upgrade()?.read(cx).connection()`
at call time (don't cache an `Arc<Connection>` on `QueryTab`) — this makes
`switch_database` (§4) only need to clear each `QueryTab`'s grid/status, not
hand it a new connection.

New methods: `request_close(window, cx)` → `is_dirty()` ? set
`pending_close = true; cx.notify()` : `close_now`; `close_now` →
`self.dock_area.update(cx, |area, cx| area.remove_panel(cx.entity(), window, cx))`;
`cancel_close` (clears `pending_close`).

Trait impls:
- `Focusable` — own `focus_handle`; forward into the editor on direct focus
  (see `query_editor.rs` note below).
- `Render` — the existing `v_resizable` editor/grid split + status bar,
  verbatim from `Session::render_panes`'s Query arm (mod.rs:1163-1194), with
  the `pending_close` confirm bar spliced in where `Session.closing` used to
  gate it, root wrapped in `div().track_focus(&self.focus_handle)`.
- `EventEmitter<gpui_base::dock::PanelEvent>` — empty impl (nothing in
  gpui-kit consumes this event; verified in both base and component 0.6.1
  sources — no emitter, no listener).
- `gpui_base::dock::Panel` — `panel_name() -> "query"`; **`closable(cx) ->
  !self.is_dirty(cx)`** (see §5 — this is the only lever available); default
  `dump`/`zoomable`/`visible`.
- `gpui_kit::component::dock::Panel` — `title()`/`tab_name()` = the
  `●`-prefixed-when-dirty string from today's `render_tab_bar` (mod.rs:900-905);
  `inner_padding(_) -> false`.
- `EventEmitter<TabEvent>` (zippa-db's own type, §3).

### 2. `TableView` — edit in place, no wrapper

`src/ui/table_view/mod.rs` currently has no `FocusHandle` at all (root is a
plain `v_flex().key_context("TableView")`). Add: `focus_handle: FocusHandle`,
`dock_area: WeakEntity<DockArea>` (new constructor param), `.track_focus(&self.focus_handle)`
on the root. Add `request_close(window, cx)` — **no confirm dance**: today a
table tab is never dirty on close even with staged edits
(`SessionTab::is_dirty` hard-codes `false` for `Table` — an existing product
decision, not something this task should change), so `TableView`'s `closable`
stays the trait default (`true`) and `request_close` just calls
`dock_area.remove_panel` directly.

Trait impls: `EventEmitter<TabEvent>`, `EventEmitter<PanelEvent>` (empty),
`Focusable`, `gpui_base::dock::Panel` (`panel_name() -> "table"`),
`gpui_kit::component::dock::Panel` (`title()`/`tab_name()` = `self.object.label()`).
Its existing `key_context("TableView")` and eight `on_action` handlers
(ApplyEdits, DiscardEdits, EditCell, …) are untouched — the `secondary-s ->
ApplyEdits` shadowing of Session's `secondary-s -> SaveFile` at the more
specific `"TableView"` context keeps working exactly as today.

### 3. Panel enumeration + "reveal an already-open panel" (the 0.6.1 gap)

There's no `DockArea::panels()` enumerating everything open. Walk it:

```rust
fn panels(&self, cx: &App) -> Vec<Arc<dyn gpui_base::dock::PanelView>> {
    self.dock_area.read(cx).layout(DockPlacement::Center)
        .into_iter()
        .flat_map(|tree| tree.panels())              // Iterator<Item = PanelId>, pre-order
        .filter_map(|id| self.dock_area.read(cx).panel(id).cloned())
        .collect()
}
```

Downcast a handle to a concrete type with `.view().downcast::<QueryTab>()` /
`::<TableView>()` — this works through the `PanelHandle` wrapper because its
`view()` delegates to the inner `Entity<T>` (verified in
`gpui-component-0.6.1/src/dock/panel.rs`).

**No `select_panel` in 0.6.1** — verified against
`gpui-base-0.6.1/src/dock/dock_area.rs`'s full `pub fn` list (`add_panel`,
`add_panel_view`, `add_tile*`, `remove_panel`, `move_panel`, `split_at`,
zoom, `load`/`dump` — nothing else). Build the equivalent from public pieces,
verified to compose correctly:

```rust
fn reveal_panel(&mut self, id: PanelId, window: &mut Window, cx: &mut Context<Self>) {
    let (node, ix) = {
        let area = self.dock_area.read(cx);
        let Some(tree) = area.layout(DockPlacement::Center) else { return };
        let Some(node) = tree.find_panel_node(id) else { return };
        let ix = match tree.find_node(node).map(|n| n.kind()) {
            Some(PaneRef::Tabs { panels, .. }) => panels.iter().position(|&p| p == id),
            _ => None,
        };
        (node, ix)
    };
    self.dock_area.update(cx, |area, cx| {
        area.move_panel(id, InsertTarget::Tabs { node, ix, activate: true }, window, cx);
    });
    self.last_focused = Some(id);
    // then focus the concrete panel's own FocusHandle (move_panel does NOT
    // move keyboard focus — verified: it only edits the tree and calls
    // commit_changed, no focus() call anywhere in its body)
}
```

Verified this is not a no-op even when `node`/`ix` are unchanged:
`PaneTree::move_panel` always does `detach_panel` (returns `true`, panel was
found) then `apply_insert` for `InsertTarget::Tabs` (returns `true`
unconditionally — it always inserts and, when `activate`, always sets
`active_ix`), so `changed` is always `true` and the commit always runs.
Passing the panel's **current** index back (not `None`, which would always
append at the end and reorder tabs) keeps its position stable while still
re-activating it.

`InsertTarget`, `NodeId`, `PaneRef`, `PanelId`, `PaneTree` are all reachable
via `gpui_kit::component::dock::*` — confirmed against 0.6.1's own re-export
list in `dock/mod.rs`.

### 4. `Session`'s reduced responsibilities

Replace `tabs: Vec<SessionTab>`, `active: usize`, `closing: Option<usize>`,
`panes: Entity<ResizableState>` with `dock_area: Entity<DockArea>`,
`skin: Rc<DockSkin>`, `last_focused: Option<PanelId>`. Keep `connection`,
`opened`, `columns`, `databases`, `objects`, `filter`, `matcher`,
`objects_tree`, `metadata_error`, `switching`.

`Session::new`: `DockSkin::dock_area("session-dock", None, window, cx)` →
`cx.subscribe_in(&dock_area, window, Self::on_dock_event).detach()` →
`self.open_tab(...)` for the first blank tab (`add_panel_view` on an empty
center creates the tab group itself — no `set_center` call needed).

Rewritten methods:
- `open_tab`/`new_query_tab` → build `QueryTab`, subscribe to its `TabEvent`,
  `dock_area.add_panel_view(panel_handle(tab), DockPlacement::Center, None, window, cx)`
  (this merges into the *first* tab group in Center — acceptable v1 default,
  same as gpui-kit's own recipe; a user who wants the new tab in a specific
  split drags it there), set `last_focused`, focus it.
- `open_object` → scan panels for a `TableView` whose `.object()` matches;
  found ⇒ `reveal_panel` + focus; else build + `add_panel_view` + focus.
  Replaces the `tabs.iter().position(...)` dedup (mod.rs:167-174).
- `switch_database` (mod.rs:555-615) → walk panels: each `QueryTab` clears
  its grid + sets `Status::Idle` (no reconnect needed, it reads the
  connection live through `session`); each `TableView` gets
  `set_connection`.
- `on_dock_event(DockEvent::LayoutChanged, ...)` → re-run
  `sync_tree_selection` (§5); if `dock_area.read(cx).is_empty(DockPlacement::Center, cx)`,
  open a fresh blank `QueryTab` — replaces the "session always shows one
  editor" invariant from `close_tab_now` (mod.rs:235-239). `is_empty` is a
  real public method on 0.6.1's `DockArea` (verified).
- `render` → unchanged shell (`key_context("Session")`, the eight
  `on_action`s, `h_resizable` sidebar split); `render_panes` becomes
  `resizable_panel().child(self.dock_area.clone())`.

Deleted: `render_tab_bar`, `render_panes`, `render_status_bar`,
`render_result_bar`, `render_close_confirm`, `close_tab`, `close_tab_now`,
`cancel_close`, `close_confirmed`, `activate_tab`, `tabs()`,
`active_tab_index()`, `on_editor_event`, `query_tab_of`, `set_status`,
`set_running`, `set_file`, `set_baseline`, the `run*`/`send`/`show_result*`/
`result_summary`/`cancel_query`/`save`/`write`/`open_file_tab` bodies (moved
into `QueryTab`, §1).

New public surface: `open_panels(cx) -> Vec<PanelSummary>` and
`reveal_panel(id, window, cx)` (§3) for quick switcher + tests, where
`PanelSummary { id: PanelId, title: SharedString, is_query: bool, path: Option<String>, focused: bool }`.

### 5. Close semantics — route through Session, don't intercept in the panel

**Correction to an earlier draft of this plan**: don't try to have
`QueryTab` intercept zippa-db's own `CloseTab` action via GPUI's bubble-up
dispatch. That fails two real cases: `src/ui/tests/session.rs`'s
`the_platform_shortcut_opens_and_closes_tabs` clicks into the **sidebar**
(outside any panel) before pressing `secondary-w`, and `src/menu.rs:51` puts
`CloseTab` on the macOS app menu — both dispatch from wherever focus already
is, which may not be inside any panel at all. So **keep `CloseTab`,
`SaveFile`, `SaveFileAs`, `CancelQuery` handled at `Session`'s existing
`key_context("Session")` root exactly as today** (no keymap.rs or menu.rs
changes), and have each handler resolve "which panel" through
`focused_panel(cx)` (§6) before calling e.g. `panel.update(cx, |p, cx| p.request_close(window, cx))`.

**gpui-kit's own `ClosePanel` action (Cmd+W via the dock skin, or the
tab-group's `⋯` menu "Close" entry) genuinely cannot be intercepted** —
verified: the `⋯` menu's "Close" item dispatches `window.dispatch_action`
from wherever focus currently is, which bubbles through the tab group's own
chrome, not through panel content; `TabGroupSkin::frame`'s `on_action::<ClosePanel>`
(handled at that chrome level) calls `DockArea::remove_panel_id` directly,
with no veto hook — `Panel::on_removed` fires after the fact. **Mitigation**:
`QueryTab::closable(cx) -> bool { !self.is_dirty(cx) }`. Verified safe: in
0.6.1, `closable` only gates whether the `⋯` menu's "Close" item is offered
and whether `TabGroup`'s own close path refuses — it does not gate
`DockArea::remove_panel`, so `request_close`'s own dirty-confirm flow (driven
through zippa-db's `CloseTab` action, per above) still works regardless. Net
behavior: a clean tab's `⋯ Close` works instantly; a dirty tab's `⋯ Close`
disappears from the menu (best available signal in this API — no silent data
loss either way). Document this quirk; it's a real, if minor, UX gap from
today's explicit confirm-on-any-close-path.

The dock skin also has **no per-tab "×" and no middle-click-to-close** —
verified by reading the tab-strip render loop (click-to-select, drag, drop —
nothing else). `src/ui/tests/session.rs`'s
`middle_clicking_a_tab_closes_it` test exercises a behavior that no longer
exists — **delete it**. `Cmd+W`/the existing "+" new-tab button/`Cmd+T`
remain the way to open and close tabs; optionally restore an explicit
close/new affordance via `Panel::toolbar_buttons` later if wanted (out of
scope here — not asked for).

### 6. Focus tracking (resolves `refresh`/`focus`/sidebar-highlight)

Add `pub(crate) enum TabEvent { Focused }`. Both `QueryTab` and `TableView`
register `cx.on_focus_in(&self.focus_handle, window, |this, window, cx| { .. ; cx.emit(TabEvent::Focused) })`
in their constructors. `Session` subscribes to each panel at creation time
and records `last_focused: Option<PanelId>` — subscriptions to a since-closed
entity are reaped automatically, so no manual bookkeeping on close.

Also set `last_focused` **imperatively** in `open_tab`, `open_object`/
`reveal_panel`, and the `LayoutChanged` empty-refill, so tests that never
draw a frame still see the right target (this is what keeps `test_support.rs`
helpers deterministic, §7).

`fn focused_panel(&self, cx) -> Option<Arc<dyn PanelView>>` = `last_focused`
filtered to "still in the dock", falling back to the first panel in Center
pre-order (matches today's `active = 0` start state).

Resolved decisions (new situations Dock introduces — there was always
exactly one active tab before):

| Consumer | Decision | Why |
|---|---|---|
| `refresh`/`reload_active_table` (mod.rs:288-299) | metadata always reloads; reload the **focused** panel only if it's a `TableView` | Reloading every open table at once could pop several "discard staged edits?" prompts simultaneously and re-query tables the user isn't even looking at. |
| `Session::focus()` (mod.rs:507-516) | focus the focused panel; if it's not a `QueryTab`, focus the first `QueryTab` in tree order; no-op if none | Same "one coherent target" rule. |
| Sidebar highlight (`sidebar.rs`'s `sync_tree_selection`) | keyed off the focused panel only (clear when it's a query tab) | Forced by the component: `TreeState::set_selected_index` is single-selection, so "highlight every open table" isn't expressible. Re-run from `TabEvent::Focused` and `LayoutChanged`. |
| `report`/`nearest_query_tab` (mod.rs:474-487) | focused panel if it's a `QueryTab`, else first `QueryTab` in tree order, else drop | Same scan, same fallback order as today. |

### 7. `src/ui/quick_switcher.rs`

Drop `use crate::ui::session::tab::TabContent;`. Replace the
`s.tabs().iter().map(...)` block and `s.active_tab_index()` read with one
`session.read(cx).open_panels(cx)` call (`PanelSummary` already carries
`title`, `is_query` for icon choice, `path` for the fuzzy-match keyword, and
`focused` for the `.checked(...)` marker). `SwitcherTarget::Tab(usize)`
becomes `SwitcherTarget::Tab(PanelId)`; the confirm arm calls
`session.reveal_panel(id, window, cx)` instead of `activate_tab(ix, cx)`.

### 8. Test fallout

**`src/ui/session/test_support.rs`** — redefine "active" as "the focused
panel (first-panel fallback)" and keep every existing helper *signature*
where possible (a few gain `cx: &App` since the data now lives on a panel
entity, not inline on `Session`):

| Helper | New implementation | Signature change |
|---|---|---|
| `tab_titles()` | titles of `panels(cx)` in dock order | gains `cx` |
| `tab_is_dirty_for_test(ix, cx)` | `panel_at(ix)` → downcast → `is_dirty` | none |
| `closing_for_test()` | index of the first `QueryTab` with `pending_close` | gains `cx` |
| `activate_tab_for_test(ix, cx)` | `reveal_panel(panel_at(ix).id, window, cx)` | gains `window` |
| `close_tab_for_test(ix, window, cx)` | `panel_at(ix)` → `request_close` | none |
| `open_tab_for_test` | unchanged wrapper over `open_tab` | none |
| `active_sql`/`active_grid`/`active_editor_for_test`/`active_table_view`/`active_status_for_test`/`active_is_running`/`results_for_test`/`mark_running_for_test`/`prepare_active_editor_for_test`/`show_result_for_test` | resolve via `focused_panel(cx)` + downcast, same body after | a few gain `cx` |
| `set_filter_for_test`, `visible_object_labels`, `set_metadata_for_test`, `selected_object_label_for_test` | untouched | none |

New: `panel_ids_for_test(cx) -> Vec<PanelId>`, `focused_index_for_test(cx)`,
and a `split_for_test(ix, placement, cx)` wrapper over `DockArea::split_at`
so at least one test actually covers two panels visible side by side.

**Test files, grouped by how much they change**:
- *Mechanical only* (add `cx`/`window` to a helper call, no semantic change):
  `files.rs`, `running.rs`, `safety.rs`, `sorting.rs`, `value_dialog.rs`,
  `paging.rs`, `rows.rs`, `editing.rs`, `filters.rs`, `preferences.rs`,
  `workspace.rs`, and `mod.rs`'s `table_view()` fixture. Every zippa-owned
  element id these click (`keep-tab`, `close-tab-anyway`, `confirm-run`,
  `cancel-run`, `result-1`, `object-*`) survives the move into `QueryTab`.
- *Rewritten against the new API*: `src/ui/tests/quick_switcher.rs` — calls
  `Session::tabs()`/`active_tab_index()`/`open_tab()`/`open_object()`/
  `activate_tab()` directly (not through `test_support`); port to
  `open_panels`/`reveal_panel`.
- *Deleted*: `middle_clicking_a_tab_closes_it` (`session.rs:468`) — the
  affordance is gone (§5).
- *Check for geometry drift*: `src/ui/tests/layout.rs` — the tab bar is now
  the dock skin's own (different height/padding); id-based assertions
  (`sidebar`, `table`, `object-*`) are unaffected, but any hard-coded pixel
  bounds near the top of the pane may need adjusting.
- *New coverage*: a split test (two `TableView`s visible at once via
  `split_for_test`, confirm both render and `refresh` only touches the
  focused one), a "close the last panel refills the center" test through
  `request_close`, and a "dirty tab hides ⋯ Close" test if you want §5
  pinned down.

## Supporting edits

- `src/ui/query_editor.rs`: add `impl Focusable for QueryEditor` delegating
  to its inner `state` (today it only has an inherent `focus()`), so
  `QueryTab` can forward focus into it cleanly.
- `src/ui/data_grid/mod.rs`: promote the existing `focus_for_test` to a real
  `pub fn focus(&self, window, cx)` for `TableView`'s focus forwarding.
- `src/keymap.rs`, `src/menu.rs`, `src/app.rs`: **no changes** — `Workspace`
  only ever holds an opaque `Entity<Session>` and calls already-stable public
  methods (`connection()`, `focus()`, `refresh()`, `new_query_tab()`,
  `render_database_picker()`), confirmed by reading `src/app.rs` in full.

## Implementation order + verification

1. `TableView` becomes focusable + dockable (focus handle, `track_focus`,
   trait impls, `TabEvent`) — still rendered by the old tab bar.
   `cargo test` — nothing should move.
2. Extract `QueryTab` with all its logic, rendered standalone. Checkpoint:
   `cargo run`, type SQL, `Cmd+Enter` runs it, results/status bar look
   identical to today.
3. Swap in the dock: `dock_area` field, `open_tab`/`open_object` via
   `add_panel_view`, `render` swap, delete `render_tab_bar`/`render_panes`.
   `cargo run` — confirm two tabs open, drag-to-split works, both panels
   render and run independently. **This and step 6 are the only places a
   real `cargo run` check matters** — drag/drop and dirty-close aren't
   reachable from headless tests.
4. Focus tracking + routing (`TabEvent`, `last_focused`, Session's action
   handlers delegating through `focused_panel`). Re-point `refresh`,
   `focus()`, `sync_tree_selection`, `report`.
5. Close semantics: `request_close`, `pending_close` bar, `closable() =
   !dirty`, `LayoutChanged` empty-refill.
6. `test_support.rs` + test files, in the grouped order from §8 (mechanical
   files first — cheapest way to flush out signature mistakes — then
   `quick_switcher.rs`, then new split/close coverage).
7. `quick_switcher.rs` production rework (can land alongside step 6).

Run `cargo test` after every step; do a real `cargo run` pass after steps 3
and 5 specifically.
