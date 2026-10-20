# UI design audit — buttons, toolbars, and menus

Source: the [gpui-kit design guides](https://gpui-kit.com/docs/design-guides.md),
applied to a full button/toolbar inventory of the app
(`zippa-db-button-inventory.md`, compiled 2026-10-19; the workspace title-bar
buttons in `src/app.rs` were inventoried separately).

The guide's command-placement rule, in short:

- primary / frequent action → visible labeled Button or familiar toolbar control
- secondary actions for the current region → visible `DropdownMenu` trigger
- object-under-pointer commands → `ContextMenu`
- natural keyboard form → Action / key binding (shown in menus and tooltips)
- hover-revealed icon → only as a shortcut to a command reachable elsewhere
- do **not** strip everything into menus: discovery and speed matter; the main
  action stays visible

And: icon-only buttons need an accessible name plus a tooltip when the meaning
isn't obvious; `…` goes on anything that opens a dialog/sheet (not on things
that open a tab); button variants carry meaning (primary = the emphasized
commitment, ghost = quiet toolbar action).

## Phase 0 — menu bar on all platforms (prerequisite)

**Finding:** the menu bar is macOS-only in practice. `menu::init` calls
`cx.set_menus(...)`, which builds the native bar on macOS — but on
Linux/Windows the platform backend only *stores* the menus
(`gpui-pre-linux/src/linux/platform.rs`: `common.menus = …`); nothing renders
them, and zippa-db never reads them back. The `menu.rs` doc comment ("a
per-window menu bar on Linux and Windows") is aspirational. So today the menu
system — which the keymap tests treat as the shortcut-discovery surface — is
invisible on two of three platforms. Demoting toolbar buttons to menus is only
safe once this is fixed.

**The fix is small; no new components needed.** `gpui-component` already ships
`AppMenuBar` ("the application menu bar, for Windows and Linux"), and its
keybindings are already initialized through `gpui_kit::init`. What's missing:

1. Nothing populates the list `AppMenuBar` reads. After `menu::init(cx)` in
   `main.rs`, copy the stored menus over:
   `gpui_kit::base::GlobalState::global_mut(cx).set_app_menus(cx.get_menus().unwrap_or_default())`.
2. Render the bar under the custom title bar on non-macOS:
   `#[cfg(not(target_os = "macos"))]` → an `Entity<AppMenuBar>` created with
   `AppMenuBar::new(&mut *cx)` (`Context` derefs to `App`) and rendered in
   `Workspace::render` between the title bar and the tab strip.
3. Adapt the "Zippa DB" app menu for Linux/Windows: drop the macOS-isms
   (Hide, Hide Others, Show All, Services). Settings… needs a conventional
   home — propose File > Settings… after a separator (Quit stays at the
   bottom of File, labeled "Quit" rather than "Quit Zippa DB"). The Edit menu
   (Undo/Redo/Cut/Copy/Paste/Select All) works as-is everywhere.

Tests to add: the bar renders the same item count as `menus()` on a
non-macOS test build; the app menu contains no `OsAction`/macOS-only items
off macOS.

## Phase 1 — quick wins (no restructuring)

Tooltips and ellipses. Each is a one-line change; all are guide violations
today.

**Add "…" to dialog-openers** (the menu items already have it; the buttons
don't): query-editor Open/Save file tooltips ("Open a SQL file…" /
"Save to a SQL file…"), ERD "Export SVG" → "Export SVG…", sidebar Import
tooltip ("Import a SQL dump…"), schema-search button ("Find in schema…").
Not needed: buttons that open *tabs* (ER diagram, New table, View structure)
— the guide's ellipsis is for dialogs.

**Add missing tooltips**: `view-structure` (table footer), ERD zoom in/out
(accessibility labels exist; visible tooltips don't), transaction
Commit/Roll Back, all four user-management toolbar buttons, all schema-view
editing buttons, Refresh/Clear on console/process-list/server-variables/
query-digest.

## Phase 2 — query editor toolbar (the densest surface: 9 buttons, 8 icon-only)

Usage tiers: Run (1, primary — stays) · Run script / Run script ignoring
errors / Explain / Explain Analyze (2, frequent — stay visible) · Open/Save
file (2, conventional in editor toolbars — stay) · Format / Variables (3).

- **Explain + Explain Analyze → one "Explain" dropdown.** They're one
  workflow with a modifier; the dropdown items keep their shortcuts. Frees a
  slot, keeps both discoverable.
- **Format → Query menu** (it has `secondary-shift-f` and is already there).
  Safest demotion in the app: shortcut + menu + low frequency.
- **Variables → Query menu as "Edit Variables…"**, which requires binding it
  to a key: `menu.rs`'s `every_menu_item_answers_to_a_keystroke` test fails
  for unbound menu items. Pick a shortcut (needs a free one; `secondary-u`
  is taken by user management on server connections). Alternative: keep the
  toolbar button. Decision needed.
- Keep: Open, Save, Run script, Run script ignoring errors, Run. The
  run-script button's transaction-blocker warning icon is meaningful state
  that a dropdown would hide, so the two script buttons stay separate.

Result: 9 icon-only + primary → 6 controls + primary, all still visible,
nothing demoted beyond what's already in the menu with a shortcut.

A more aggressive alternative (for discussion): a Run split-button holding
all three run variants plus an overflow "···" for Format/Variables/Explain.
Not recommended — it buries the statement-vs-script distinction, which is
the app's core editing concept.

## Phase 3 — table view footer and placement tweaks

The footer mixes navigation (paging, limit) on the left with object actions
(New row, Discard, Apply, Export ▾, View structure, row-panel toggle) on the
right — up to 6 visible when edits are staged. Mostly right; two tweaks:

- **"View structure" is navigation sitting among edit actions.** (Attempted:
  moved it to the left group beside the paging controls, but in narrow
  windows it overflowed the footer row and clipped the pending-navigation
  guard buttons — `filtering_asks_before_it_discards_staged_edits` caught
  it. Reverted; the placement is mildly illogical but the real estate is
  not worth a broken footer.)
- **Paging has no shortcuts.** Consider PageUp/PageDown-style bindings when
  the grid has focus (needs a keymap check for conflicts). Optional.

Everything else in the footer earns its place: paging is Tier 1, the edit
trio is the staged-edit workflow (Apply correctly primary), Export is
already the guide's DropdownMenu pattern.

Other placement notes (all minor, all "right place, polish needed"):

- Transaction Commit/Roll Back in the query-tab status bar: right place
  (they're the primary action while a transaction is open) — tooltips only.
- Database picker in the title bar: Tier 1, right place.
- Sidebar's 4 icon-only buttons: fine as a group, but `Network` (ER diagram)
  is the least guessable icon in the app — worth a better metaphor if one
  exists in the icon set.
- SQLite maintenance's 7 outline buttons: keep. Each button *is* the
  feature; the guide's dropdown rule is for secondary actions, and none of
  these is secondary to the others.
- Schema view's Preview (primary, footer) → Apply (primary, banner): two
  primaries is correct here — they're sequential decision areas, not one.

## Phase 4 — menu additions

The menu bar is already comprehensive for bound actions (tests enforce it).
Only one addition is proposed, and only if Phase 2 demotes the button:

- Query > "Edit Variables…" (bound to a new shortcut).

Deliberately **not** adding: exports, ERD controls, paging, maintenance
tasks, user-management actions — they're contextual to their view, and the
guide warns against menu-bloating for its own sake. The row context menu
already covers grid actions.

## Suggested order

1. Phase 0 (unblocks everything else; also fixes shortcut discovery on
   Linux/Windows by itself).
2. Phase 1 (trivial, shippable immediately).
3. Phase 2 (the one real redesign).
4. Phase 3 + 4 (small, can ride along).

## Open questions

1. Query editor: the conservative Phase-2 plan, or something more aggressive?
2. Variables: bind a shortcut + Query menu, or keep the toolbar button?
3. Format: demote to the Query menu, or keep it visible?
4. Paging shortcuts: worth adding?

## Feedback and overlays (added 2026-10-20, same PR)

From the guide's "Feedback and overlays" section: pick the smallest surface
that fits — notification for asynchronous status needing no decision, the
region itself for state that belongs to it, a dialog only for decisions.

**Query run errors moved from the status bar to the result pane.** A run
that failed outright (single statement, script start, variable
substitution, explain) now leaves its error in the tab's own result pane
(`TabContent::Query.error`, rendered by `render_pane_error`): full text,
in the space the rows would have filled, attached to the tab that produced
it — so two tabs with two failures each show their own. The status bar
names the state ("Error") without repeating the message it used to cap at
72px and scroll. The toast stays: it carries the failure to wherever
attention is when the tab is in the background. A fresh run, a successful
run, or a plan clears the pane error; the pane also gives up the last run's
results, completing what `show_run_error`'s comment already claimed.
Scripts are unchanged — their failures are already a result in the pane,
with the status as summary. The workspace-restore "could not read …" error
takes the same pane path: it was status-bar-only in a tab that might sit
unopened all session.

**Audited and deliberately kept:**
- The shared-connection guard refusal ("another query tab is using the
  shared connection") stays as the tab's status: it answers the user's own
  Run press in the tab they're looking at, and the pane must keep the
  still-valid results.
- Table-view and schema-view footer messages are inline validation next
  to the action that produced them ("this row cannot be addressed",
  "nothing has changed"); the async failures there (load, commit, export)
  already toast. Schema apply failures are persistent in the view's own
  footer, the same in-place principle as the query pane.
- The script-failure alert dialog and the ConfirmWrites banner are
  decisions, so dialogs/banners are the right surface per the guide.
- Success toasts stay only where the outcome isn't otherwise visible
  ("Copied …", "Reconnected.").
