# Turn written conventions into tests

## Context
AGENTS.md states rules a reviewer has to remember: every icon-only `Button` and `Checkbox` has an accessibility label, every setting has a way in that is not a keystroke, tooltips on bound actions show the key. `menu.rs` already asserts that every bound action is in the menu bar, which shows the pattern works.

## Plan
1. **Accessibility labels.** A GPUI test helper in `src/ui/tests/mod.rs` that renders a view and walks the element/accessibility tree for icon-only buttons and bare checkboxes with no label. Run it over the main views (welcome, session with each tab kind, data grid, dialogs). Start report-only, list known deviations (the connection editor's caret, the dock `+`) in an allowlist, then fail on anything new. If gpui-kit does not expose the tree, add a lint-style source check instead (grep for `Button::new(...).icon(...)` without `accessibility_label`) as a test over `src/ui/**`.
2. **Settings reachability.** Test that every field of `Settings` appears in the settings window's rendered rows (or a `Settings::UI_EXEMPT` list with a reason).
3. **Keymap vs menu vs docs.** Extend `menu.rs`'s tests: every binding in `keymap.rs` is listed in README's shortcut table (parse the table), so docs cannot drift.
4. Each check carries a message naming the rule and where AGENTS.md states it.

## Risks
Walking rendered output can be brittle across gpui-kit upgrades; keep each helper small and in one place.
