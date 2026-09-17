# Dock-based session panels

## Context

`Session` (`src/ui/session/mod.rs`) shows exactly one tab's content at a time.
It owns `tabs: Vec<SessionTab>` plus an `active: usize`, draws the strip with
gpui-component's stateless `TabBar`/`Tab`, and renders `self.tabs[self.active]`
into a single pane. Every per-tab method — `set_status`, `send`, `show_results`,
`save`, `cancel_query` — is keyed by that index, and two of them
(`query_tab_of`, `nearest_query_tab`) exist only to re-find an index after it has
shifted.

A user cannot put two tables side by side. gpui-kit ships
`gpui_kit::component::dock` for exactly this: a `DockArea` holding a tree of tab
groups and splits, each leaf a panel entity, with drag-to-split and
drag-to-reorder already implemented and **no caller wiring at all**.

The outcome: a session's tabs become dock panels, so any tab — query, table, or
structure — can be dragged to split the view, and the index-keyed bookkeeping
goes away with the indices.

**Out of scope, by decision:**

- **No persistence** of open tabs or layout across restart — matches today (every
  reconnect starts with one blank query tab). Skips `register_panel`, `Panel::dump`
  and `DockArea::load` entirely; they can be added later without restructuring.
- **Center region only** — no left/right/bottom dock slots. The object sidebar
  stays exactly as it is, its own `h_resizable` pane in `Session::render`.
- **Drag is the only way to split** — no `Split Right`/`Split Down` actions.

## Key API facts (verified in `~/.cargo/registry/.../gpui-component-0.6.1/src/dock/` and `gpui-base-0.6.1/src/dock/`)

Everything is re-exported from `gpui_kit::component::dock`; never depend on
`gpui-base` directly.

- A panel implements `BasePanel` (only `panel_name` is required), `Panel` (all
  defaulted), `EventEmitter<PanelEvent>`, `Focusable`, `Render`.
- **Always wrap with `panel_handle(entity)`** and use `panel_view` /
  `add_panel_view`. A bare `Entity<P>` loses its presentation half and renders
  `panel_name` as its title.
- Drag-to-split and drag-to-reorder are free. Zero registration, no ids.
- **The skin's tab strip has no ✕ and no middle-click close.** Each `Tab` gets
  only `panel_title(...)`. Closing is the group's `…` menu → Close.
- `Panel::title(&mut self, window, cx) -> impl IntoElement` is what the strip
  renders, so arbitrary elements (our ✕, a middle-click handler) live inside the tab.
- **There is no `DockArea::activate_panel`.** Bring a panel forward through the
  `WeakEntity<TabGroup>` handed to `Panel::on_added_to`, then
  `group.select_tab(ix, window, cx)` (public; returns early if already displayed;
  calls `focus_active_panel`).
- `DockArea::remove_panel` ignores `closable` and every group constraint — that
  is the session's own close path.
- The `…` → Close path is `TabGroup::close_panel` → `TabGroupEvent::ClosePanel` →
  `DockArea::remove_panel_id`. It **never asks the host first**, so the only veto
  is `Panel::closable()`.
- `DockArea` emits only `DockEvent::{LayoutChanged, DragDrop}` — no panel-closed
  or active-changed event. Lifecycle is observed panel-side via `set_active` /
  `on_added_to` / `on_removed`.
- `on_removed` is dispatched synchronously at the end of `reconcile`, but a
  `cx.emit` inside it is **deferred to the next effect flush**.
- Reference: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-kit-0.6.1/tests/dock.rs` (177 lines) — read it first.

## The one rule that shapes everything

**`Session` does its own bookkeeping synchronously on every path it initiates.**
`cx.emit` is deferred to the effect flush, but every existing test asserts
*inside* the same `handle.update(...)` closure that performed the action
(`open_tab_for_test(...); assert_eq!(session.tab_titles(cx), ...)`). So
`self.panels` and `self.active` are set by `Session` directly; the panel's
`Focused`/`Removed` events exist only to catch paths the *dock* initiates — a tab
click, a drag between groups, the `…` menu's Close — and their handlers are
idempotent.

---

## 1. New file: `src/ui/session/panel.rs`

One panel type wrapping the existing three-variant `TabContent`, not three panel
types.

```rust
pub(crate) struct SessionPanel {
    /// Stable identity for element ids and for tests: assigned in creation
    /// order and never reused, so a close button keeps its id when the user
    /// drags the tabs about.
    key: usize,
    title: SharedString,
    content: TabContent,
    focus: FocusHandle,
    /// The tab group displaying this panel, for bringing it forward. `None`
    /// until the dock has taken it.
    group: Option<WeakEntity<TabGroup>>,
    /// Editor above grid. One state per panel, not one per session: a split
    /// puts two query panels on screen at once, and a single `ResizableState`
    /// cannot drive two elements.
    panes: Entity<ResizableState>,
}

pub(crate) enum SessionPanelEvent {
    Focused,            // the user is working in this panel now
    Removed,            // the panel left the dock, however it left
    CloseRequested,     // the tab's ✕, or a middle click on it
    NewTabRequested,    // the strip's + — a query tab beside this one
    Run(String),
    RunScript(String),
    ConfirmRun(String), // the status bar's Run answered a held-back write
    OpenFile,
    Save,
}
```

Constructors `query(key, title, sql, window, cx)` / `table(...)` / `schema(...)`.
`query` subscribes to its own `QueryEditor` and re-emits `QueryEditorEvent` as
the matching `SessionPanelEvent` — **this is what deletes `query_tab_of`**: the
subscription is per-panel, so there is no index to re-resolve.

**No `WeakEntity<Session>` field.** CLAUDE.md's rule is a child emits its own
event enum and the parent subscribes; inside `Panel::title(&mut self, window,
cx: &mut Context<Self>)` there is `cx.entity().downgrade()` to build the closures.

### Trait impls

```rust
impl EventEmitter<SessionPanelEvent> for SessionPanel {}
impl EventEmitter<PanelEvent> for SessionPanel {}

impl Focusable for SessionPanel {
    /// A query panel answers with its editor, so clicking its tab puts the
    /// caret where the user is about to type; the dock focuses this handle
    /// itself whenever it displays the panel.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            TabContent::Query { editor, .. } => editor.read(cx).focus_handle(cx),
            TabContent::Table { .. } | TabContent::Schema { .. } => self.focus.clone(),
        }
    }
}

impl BasePanel for SessionPanel {
    fn panel_name(&self) -> &'static str { "SessionPanel" }

    /// Permission for the dock's own Close, which never asks the session
    /// first; the ✕ goes through `DockArea::remove_panel`, which ignores this.
    fn closable(&self, cx: &App) -> bool { !self.is_dirty(cx) }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if active { cx.emit(SessionPanelEvent::Focused); }
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _window: &mut Window, _cx: &mut Context<Self>) {
        self.group = Some(group);
    }
    /// A tab that leaves takes its running query with it.
    fn on_removed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.abort_running();
        cx.emit(SessionPanelEvent::Removed);
    }
}

impl Panel for SessionPanel {
    fn title(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement;
    fn toolbar_buttons(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>>;
    fn dropdown_menu(&mut self, menu: PopupMenu, window: &mut Window, cx: &mut Context<Self>) -> PopupMenu;
    /// The bodies carry their own footers and fill the pane.
    fn inner_padding(&self, _cx: &App) -> bool { false }
    fn zoom_control(&self, _cx: &App) -> Option<PanelControl> { Some(PanelControl::Toolbar) }
}
```

`title()` is today's per-tab element, moved off `render_tab_bar`: the `●` dirty
marker, the ✕ `Button` with its `accessibility_label` and
`tooltip_with_action("Close tab", &CloseTab, Some("Session"))`, and a
**capture-phase** `capture_any_mouse_down` on the title root for middle-click —
capture phase for the same reason the grid records its right-clicked row that
way, so the ✕ inside cannot swallow the press first. The button id is
**`close-tab-{key}`, not `close-tab-{index}`**: indices are meaningless once tabs
are dragged between groups, and the title element is built into the *group's*
element scope, so two panels in one group need distinct ids.

`toolbar_buttons()` is the `+`, one per group, emitting `NewTabRequested` so the
session puts the new tab in *this* panel's group.

`dropdown_menu()` adds a disabled `Close (unsaved changes)` item when
`is_dirty`, so the `…` menu says why its Close vanished rather than just looking
broken — the same "never tell it by appearance alone" rule as the struck-through
deleted rows.

`Render` is today's `render_panes` body, with `.track_focus(&self.focus)` on the
root so focus-in fires for the grid and the table/schema views too.

### Moved onto the panel from `Session`

`render_status_bar`, `render_result_bar`, the `v_resizable("session-panes")`
body, `set_status`, `set_running`, `show_results`, `show_result`,
`result_summary`, `set_file`, `set_baseline`, the bodies of `cancel_run` and
`cancel_query`. From `tab.rs`: `SessionTab::{object, mode, is_dirty}`.
`EDITOR_HEIGHT` moves here. New: `abort_running`, `cancel_running(cx) -> bool`,
`set_connection`, `refresh`, `commit`, `key`, `is_query`, `file_name`,
`status_message`, and:

```rust
/// Bring this panel forward in whatever group holds it, and put focus in it.
pub(crate) fn bring_forward(&self, window: &mut Window, cx: &mut Context<Self>) {
    let id = PanelId::from(cx.entity_id());
    if let Some(group) = self.group.as_ref().and_then(|g| g.upgrade()) {
        group.update(cx, |group, cx| {
            if let Some(ix) = group.panels().iter().position(|p| p.panel_id(cx) == id) {
                group.select_tab(ix, window, cx);
            }
        });
    }
    self.focus_handle(cx).focus(window, cx);   // covers the already-displayed case
}
```

## 2. `src/ui/session/tab.rs`

Keep `ObjectViewMode`, `Status`, `TabContent` unchanged. **Delete `SessionTab`**
and its three methods — `title` is a `SessionPanel` field, the methods are
`SessionPanel` methods.

## 3. `src/ui/query_editor.rs`

Add one method so the panel can hand the dock a focus handle:

```rust
pub fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
    use gpui_kit::Focusable as _;
    self.state.read(cx).focus_handle(cx)
}
```

`QueryEditor::focus` stays as it is.

## 4. `src/ui/session/mod.rs`

Fields: `tabs`, `active: usize` and `panes` out; in go

```rust
dock: Entity<DockArea>,
/// Every panel this session owns, in creation order. The dock owns where
/// they are; this owns which ones exist.
panels: Vec<Entity<SessionPanel>>,
/// The panel the user is in — with a split open, several panels are
/// displayed at once, so "active" means where focus last was.
active: Option<WeakEntity<SessionPanel>>,
closing: Option<WeakEntity<SessionPanel>>,   // was Option<usize>
next_key: usize,
```

`Session::new` builds the dock before the first tab:

```rust
let (dock, skin) = DockSkin::dock_area(
    SharedString::from(format!("session-dock-{}", connection.config.id)), None, window, cx);
skin.set_panel_style(PanelStyle::TabBar, cx);   // a lone tab still gets a strip
dock.update(cx, |dock, cx| dock.set_center(DockLayout::tabs(), window, cx));
```

A per-connection id because every workspace tab builds one.

### The four new methods everything else goes through

```rust
/// The one place a panel joins the session.
fn install(&mut self, panel: Entity<SessionPanel>, window: &mut Window, cx: &mut Context<Self>) {
    cx.subscribe_in(&panel, window, Self::on_panel_event).detach();
    // `add_panel_view` appends to the *first* group in the region, so with a
    // split open a new tab would land beside the wrong one.
    let target = self.active_group(cx);
    self.dock.update(cx, |dock, cx| {
        dock.add_panel_view(panel_handle(panel.clone()), DockPlacement::Center, None, window, cx);
        if let Some(node) = target {
            dock.move_panel(PanelId::from(panel.entity_id()),
                InsertTarget::Tabs { node, ix: None, activate: true }, window, cx);
        }
    });
    self.panels.push(panel.clone());
    self.active = Some(panel.downgrade());
    self.sync_tree_selection(cx);
    cx.notify();
}

/// The node of the tab group holding the active panel — where a new tab goes.
fn active_group(&self, cx: &App) -> Option<NodeId>;

/// The one place `active` moves.
fn touch(&mut self, panel: &Entity<SessionPanel>, cx: &mut Context<Self>);

/// A panel left the dock — by the ✕, or by the `…` menu's Close, which never
/// asks the session first. Idempotent: the ✕ path has already done this by
/// the time the event arrives.
fn forget(&mut self, panel: &Entity<SessionPanel>, window: &mut Window, cx: &mut Context<Self>);
```

`on_panel_event` is the hub: `Focused` → `touch`; `Removed` → `forget`;
`CloseRequested` → `close_tab`; `NewTabRequested` → `touch` then `open_tab`;
`Run`/`RunScript`/`ConfirmRun` → `run`/`run_script`/`run_now`; `OpenFile` →
`open_file`; `Save` → `save(panel, false, cx)`.

### Re-keying the rest

The pattern: every `index: usize` parameter becomes `panel: &Entity<SessionPanel>`,
and every `self.tabs.get(index)` + `TabContent` match becomes a
`panel.read(cx)` / `panel.update(cx, ...)` accessor call.

| Today | After |
|---|---|
| `open_tab(title, sql, run, window, cx)` | same signature, **returns `Entity<SessionPanel>`**; builds `SessionPanel::query`, `install`s it, then `run` |
| `open_object` | dedupe scans `self.panels` by `(mode, object)`; hit → `activate_panel`; miss → build the view (keeping `cx.subscribe_in(&view, window, Self::on_table_navigate)` on the `TableView`), wrap, `install` |
| `open_object_filtered` | same shape; the second lookup uses `panel.read(cx).table_view()` |
| `activate_tab(index, cx)` | `activate_tab(index, window, cx)` — indexes `self.panels` for the quick switcher and tests, delegates to `activate_panel` |
| — (new) | `activate_panel(&panel, window, cx)`: `bring_forward` then `touch` |
| `close_tab(index, …)` | `close_tab(&panel, …)`: dirty → `self.closing = Some(panel.downgrade())`; else `close_tab_now` |
| `close_tab_now(index, …)` | `close_tab_now(&panel, …)`: if it is the only panel, `open_tab` a blank one **first** (so `opened` still yields "Query 3", the center is never empty, and the replacement lands in the same group); `panels.retain`; `dock.remove_panel(panel, …)`; clear `closing`/`active` if they pointed at it. **The index-shifting arithmetic on `closing` disappears.** |
| `query_tab_of(&Entity<QueryEditor>)` | **deleted** |
| `nearest_query_tab()` | `nearest_query_panel(cx) -> Option<Entity<SessionPanel>>`, rotating from the active panel's position in the registry |
| `focus(&self, window, cx)` | `self.active_panel()?.read(cx).focus_handle(cx).focus(window, cx)` — now also focuses table/schema panels, so `Cmd+W` works after switching workspace tabs onto a table tab |
| `sync_tree_selection` | reads the label off `self.active_panel()`; rest unchanged |
| `reload_active_table` | `self.active_panel()?.update(cx, |p, cx| p.refresh(cx))` |
| `save(index, ask, cx)` | `save(&panel, ask, cx)`; the table arm is `panel.update(cx, |p, cx| p.commit(cx))` |
| `write(editor, path, sql, cx)` | `write(panel: WeakEntity<SessionPanel>, …)`; the continuation pokes the panel directly — no re-resolution |
| `run` / `run_script` / `run_now` / `send` | `(&panel, sql, …)`; `send`'s spawn captures `panel.downgrade()` |
| `confirm_run` / `cancel_run` | `cancel_run` moves onto the panel; `confirm_run` becomes the `ConfirmRun(sql)` event arm |
| `cancel_query` | `self.active_panel()?.update(cx, |p, cx| p.cancel_running(cx))` |
| `switch_database` | the `for index in 0..this.tabs.len()` walk becomes `for panel in this.panels.clone() { panel.update(cx, |p, cx| p.set_connection(connection.clone(), cx)) }` |
| `report` | `nearest_query_panel` → `panel.update(cx, |p, cx| p.set_status(...))` |
| `tabs()` | `panels(&self) -> &[Entity<SessionPanel>]` |
| `active_tab_index()` | position of `active` in `self.panels`, else `0` |

**Deleted outright:** `render_tab_bar`, `render_panes`, `set_status`,
`set_running`, `show_results`, `show_result`, `result_summary`, `set_file`,
`set_baseline`, `render_status_bar`, `render_result_bar`.

### `Session::render`

The `key_context("Session")` root, its eight `on_action` handlers, the
`h_resizable("session-columns")` and the sidebar pane are **untouched** — the
root is still an ancestor of every panel, so actions dispatched from a focused
editor still bubble to it. Only the right column changes:

```rust
resizable_panel().child(
    v_flex().size_full()
        .when_some(self.closing_panel(), |this, panel| this.child(self.render_close_confirm(&panel, cx)))
        .child(div().flex_1().min_h_0().child(self.dock.clone())),
)
```

`render_close_confirm` stays a Session-level element — one at a time, and the
`keep-tab` / `close-tab-anyway` ids stay unique.

## 5. `src/ui/quick_switcher.rs`

Three edits. Drop the `TabContent` import; the match becomes accessor calls:

```rust
let tabs: Vec<(SharedString, bool, Option<String>)> = s.panels().iter()
    .map(|p| { let p = p.read(cx); (p.title().clone(), p.is_query(), p.file_name()) })
    .collect();
```

`SwitcherTarget::Tab(usize)` **stays an index** into `panels()` — the dialog is
modal and built from a snapshot taken microseconds earlier. The confirm arm gains
the `window` already in scope: `session.activate_tab(ix, window, cx)`.

## 6. `src/keymap.rs`, `src/app.rs`

**Unchanged.** Same contexts, same actions, and `Workspace::focus_active` still
calls `Session::focus(window, cx)` with the same signature.

## 7. Tests

`src/ui/session/test_support.rs` — add two primitives and express the rest
through them:

```rust
#[cfg(test)] pub(crate) fn active_panel_for_test(&self) -> Option<Entity<SessionPanel>>;
#[cfg(test)] pub(crate) fn panel_for_test(&self, index: usize) -> Entity<SessionPanel>;
```

- `tab_titles()` → **`tab_titles(&self, cx: &App)`**: the title now lives in the
  panel entity. Mechanical churn across ~15 call sites; cheaper than mirroring
  titles onto `Session` as a second source of truth.
- `closing_for_test() -> Option<usize>` → `closing_title_for_test(&self, cx) -> Option<String>`.
- `activate_tab_for_test(index, cx)` → `activate_tab_for_test(index, window, cx)`.
- `active_sql`, `active_table_view`, `active_schema_view`, `active_editor_for_test`,
  `active_grid`, `active_is_running`, `active_status_for_test`, `results_for_test`,
  `show_result_for_test`, `mark_running_for_test` — one-liners through
  `active_panel_for_test()`.
- `tab_is_dirty_for_test`, `close_tab_for_test` — signatures unchanged, bodies
  go through `panel_for_test`.
- New: `close_button_id_for_test(index, cx) -> SharedString` (→ `close-tab-{key}`)
  and `dock_remove_for_test(&panel, window, cx)`, a thin wrapper over
  `DockArea::remove_panel` for exercising the `…`-menu close path.

`src/ui/tests/session.rs` — seven of fourteen need only the `tab_titles(cx)`
argument. The rest:

| Test | Fix |
|---|---|
| `middle_clicking_a_tab_closes_it` | ids are key-based now: `let id = session.close_button_id_for_test(1, cx)`, then `window.find(id)`. The press needs no change — `capture_any_mouse_down` sees it before the `Button`. |
| `closing_the_last_tab_leaves_an_empty_editor` | `tab_titles(cx)` only. The assertions hold: closing index 0 adds "Query 3" first, then removes "Query 1". **This test is what proves the synchronous-bookkeeping rule** — it all happens inside one `handle.update` closure. |
| `closing_a_dirty_tab_asks_first` | `closing_title_for_test(cx) == Some("Query 1")` / `None`. |
| `the_sidebar_highlights_the_table_the_active_tab_shows` | add `window` to `activate_tab_for_test`. |
| `each_tab_keeps_its_own_result` | should pass as-is (`add_panel_view` activates the new tab); verify. |

Elsewhere: `files.rs` (3 × `activate_tab_for_test`, `tab_titles`),
`quick_switcher.rs` (`tabs()` → `panels()`, `activate_tab(0, window, cx)`),
`workspace.rs` / `schema.rs` (`tab_titles`). `running.rs` (`result-1`) and
`safety.rs` (`confirm-run`/`cancel-run`) are unaffected — those ids now live in
the panel's own view scope, and `window.find` searches the whole tree.

**Two new tests in `src/ui/tests/session.rs`:**

- `dragging_a_tab_splits_the_view` — async, modelled on
  `gpui-kit-0.6.1/tests/dock.rs`: open a table tab beside Query 1, `render_frame`,
  `window.drag(window.within("tab-bar").find(1usize).bounds().center(), <body
  centre>, cx)`, then `cx.wait_for(...)` until both panels are visible with
  non-overlapping bounds. Needs `use gpui_kit::test::TestAppContextExt;`.
- `closing_a_panel_from_the_dock_forgets_it` — `dock_remove_for_test` on a clean
  panel, then assert `tab_titles` shrank. Covers the `…`-menu path that never
  reaches `Session::close_tab`.

## 8. Order of work

1. `QueryEditor::focus_handle` — compiles on its own.
2. `panel.rs` + delete `SessionTab` from `tab.rs`. Move the render bodies
   verbatim, swapping `self.tabs[index]` for `self.content` and the `cx.listener`
   target from `Session` to `SessionPanel`. *(Does not compile until step 3 — one
   commit.)*
3. `mod.rs`: fields, the four new methods, then re-key the table in §4, then `render`.
4. `test_support.rs` and `quick_switcher.rs`.
5. `cargo test` — expect breakage confined to §7.
6. Fix the tests; add the two new ones.

## 9. Risks

| Risk | Mitigation |
|---|---|
| **The Root-less test window.** `session_with_objects` opens `Session` directly, not under `Root`. If the strip's `…` dropdown or drag preview needs Root's layer *at render time*, every session test panics. | Step 5's first `cargo test` tells you. `dock.rs`/`tab_panel.rs` reference no `Root` or `deferred`, and the sidebar already renders `.context_menu(...)` in these windows, so this should hold. Fallback: wrap the fixture in `Root::new` and return `(WindowHandle<Root>, Entity<Session>)` the way `WorkspaceWindow` already does — ~8 files of mechanical churn, worth knowing early. |
| **`set_active` picks the wrong panel after a split** — both groups' displayed panels are told `true` in the same frame, order arbitrary. | Focus-in is the authoritative signal and fires last (the drop focuses the moved panel through `select_tab` → `focus_active_panel`). If it still misbehaves, drop the `Focused` emit from `set_active` and rely on focus-in plus Session's own synchronous sets. |
| **`is_dirty` on the render path** — `title()` and `closable()` both call it, and it clones the whole buffer. Same cost as today's `render_tab_bar` for the title; the `closable` call is new. | If `large_results_do_not_make_frames_expensive` regresses, cache `dirty: bool` on the panel, recomputed on `InputEvent::Change` (the panel already subscribes to the editor). |
| **Element-id collisions across simultaneously visible panels** — two query panels both render `v_resizable("session-panes")`, `div().id("status")`, `Button::new("confirm-run")`. | Bodies render as the panel *entity's* view, so GPUI scopes their ids per view — which is exactly why the ✕, built into the *group's* scope, is the one thing needing a per-panel key. The split test verifies it by asserting both bodies are found with distinct bounds. |
| **The `+` is no longer keyboard-reachable** — the dock forces `tab_stop(false)` on toolbar buttons. | `Cmd+T` is bound and named in the button's tooltip, and the workspace toolbar's `new-query` button (`src/app.rs:517`) is still tab-reachable. Record the deviation in CLAUDE.md's accessibility list. |
| **Registry order drifts from visual order** once tabs are dragged, so the quick switcher lists tabs in creation order. | Accept and document. Re-sorting on `DockEvent::LayoutChanged` by walking `DockArea::layout(Center)`'s `PaneTree` is all public API, but it is a second source of truth — only do it if a user notices. |
| **Per-tab editor/grid split state.** Today one shared `panes` keeps the split put across tabs; per-panel state loses that. | Unavoidable: two query panels side by side cannot share one `ResizableState` or element id. Behavior change, worth a line in the commit message. |
| **`Cmd+W`/`Cmd+.`/`Cmd+S` act on `active`, ambiguous with splits.** | `active` is "where focus last was", which is what the user means. The keystroke reaches `Session` by bubbling from the focused editor, so focus and `active` agree by construction on every keystroke path. |

## 10. Verification

`cargo test` for the suite, then `cargo run` and walk the manual path — this is a
drag-driven feature and the tests only cover so much of it:

1. Drag a table tab to the right edge of the body → two tables side by side.
2. Drag it back onto the other group's strip → one group again, tabs reordered.
3. `Cmd+T` while focused in the right-hand split → the new tab lands **in that
   split**, not the left one.
4. Type in a query tab, then close it with the ✕ → confirm bar; `Keep tab` and
   `Close Without Saving` both behave. Open the same tab's `…` menu → Close is
   present but disabled, reading "Close (unsaved changes)".
5. Close the only remaining tab → a blank "Query N" replaces it, never an empty pane.
6. Switch database with a split open → both panels re-read.
7. Run a slow query in a background split, focus it, `Cmd+.` → that panel cancels.
8. `Cmd+K` quick switcher → picking a tab in the other split brings it forward
   and focuses it.
9. Tab through the session → the ✕ on each visible tab is reachable and named.
