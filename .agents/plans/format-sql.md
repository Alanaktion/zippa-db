# Format SQL in the editor

## Context
The query editor (`src/ui/query_editor.rs`) runs text verbatim and has syntax highlighting through gpui-kit's `tree-sitter-sql` feature, but no formatter. Long generated or pasted queries stay unreadable. Goal: a `Format SQL` action for the selection or the whole buffer that is safe, undoable, and predictable.

Decision (from user): use the `sqlformat` crate. Tree-sitter-based printing is not worth building now.

## Design

### 1. Formatter module — `src/db/format.rs` (new; not UI-specific so it is unit-testable)
`pub fn format(sql: &str, engine: Engine, options: &FormatOptions) -> Result<String, FormatError>`.
- Split with `statement::split` and format each statement separately. This reuses the comment- and quote-aware scanner already in `statement.rs` and keeps the text between statements (blank lines, standalone comments) in place. Join with one blank line between statements, keeping a trailing `;` on each as it was.
- Skip, leaving verbatim, any statement `sqlformat` is known to handle badly: dollar-quoted bodies (`$$ … $$`, `$tag$`), `CREATE FUNCTION/PROCEDURE/TRIGGER … BEGIN … END`, MySQL `DELIMITER` lines, and psql `\` meta-lines. Report the count (`2 statements left as written`) instead of mangling routine bodies.
- Options (`FormatOptions { indent: u8 (2 or 4), keyword_case: Upper | Lower | Preserve }`): defaults 2 spaces, upper-case keywords. Store in `Settings` (`format_indent`, `format_keywords`, `#[serde(default)]`) and expose in the settings window, per the rule that every setting needs a non-keystroke way in.
- Dialect handling: `sqlformat` is dialect-agnostic; the `engine` argument only affects the skip list (MySQL backslash escapes, Postgres `::` casts and `$n` parameters, JSON operators `->`, `->>`, `#>`) and a few post-fixes if testing shows it splits `::` or `->>` into separate tokens. Pin each with a test.

### 2. Safety net (the important part)
A formatter that changes meaning is worse than none. After formatting, compare the token stream of the original and the result using the same scanner: same sequence of tokens (identifiers, literals, operators), ignoring whitespace and keyword case, comments compared verbatim. If they differ, do not apply; show `Error: could not format safely; nothing changed.` and log the first differing token to stderr for bug reports. This also makes the "skip known-bad statements" list less critical. Idempotence is a second check in tests: `format(format(x)) == format(x)`.

### 3. Editor integration — `src/ui/query_editor.rs`
- `actions!` adds `FormatSql`. Handler: if the selection is non-empty format just it, else format the whole buffer (statement-at-caret formatting is a follow-up; whole buffer is the least surprising and matches most editors).
- Apply through the input's replace-range API so it is a single undo step (`Cmd+Z` restores). Verify gpui-kit's `InputState` exposes range replacement that lands on the undo stack; if it only offers `set_value`, undo history is lost and the plan needs a workaround (keep the previous text in the tab and offer `Undo format` in the status bar) — decide after checking, before building the UI.
- Preserve the caret: map the old caret to the new text by counting non-whitespace characters before it; place it at the same count. Selection formatting keeps the replaced range selected.
- Empty buffer or unchanged result: status `Already formatted` / nothing to format; no edit, no dirty flag.
- Read-only: formatting edits the buffer, not the database, so it works on every safety mode.
- Status text after: `Formatted 3 statements (1 left as written)`.

### 4. Discoverability and keys
- Toolbar icon button with `.accessibility_label("Format SQL")` and `.tooltip_with_action("Format SQL", &FormatSql, Some("QueryEditor"))`.
- Keymap: `secondary-shift-f` in `QueryEditor > Input` and `QueryEditor`. Check the input component's own bindings for a clash (the editor's `Input` context claims keys first, which is why `RunQuery` is bound at `QueryEditor > Input`).
- Menu (`menu.rs`): `Edit > Format SQL`; shortcuts dialog and README entries; quick switcher action `Format SQL`.
- Settings window: indent (2/4) and keyword case controls.

### 5. Sequencing
1. Add dependency; `format.rs` with per-statement formatting, skip list, token-equivalence check, unit tests.
2. Spike the editor's replace/undo API; decide undo approach.
3. Action, handler, caret mapping, status message.
4. Settings, toolbar/menu/keymap/docs.

## Critical files
- new: `src/db/format.rs`, `src/ui/tests/formatting.rs`
- edit: `Cargo.toml` (`sqlformat`; confirm current version and its `FormatOptions` fields at implementation time), `src/db/{mod,statement}.rs` (expose the token scanner if it is private), `src/settings.rs`, `src/ui/settings_window.rs`, `src/ui/query_editor.rs`, `src/ui/session/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `README.md`, `TODO.md` (add under SQL Query Editor > Text Editing)
- reuse: `statement::split`, `settings::update`, editor toolbar button pattern.

## Testing
- Unit: simple select with joins/where/group by; nested subqueries; CTE; `INSERT … VALUES` with several rows; comments (line and block) survive in place; string literals with `--` and `;` untouched; Postgres `::` casts, `->>`, `$1` params, dollar-quoted function body left verbatim; MySQL backticks and `#` comments; already-formatted input unchanged; two statements keep their separation; idempotence over all fixtures; the token-equivalence check rejects a deliberately corrupting fake formatter.
- Editor: selection formats only the selection; whole-buffer format; undo restores (if the API allows); caret position after format lands on the same token; format of an empty buffer is a no-op with a message.
- Manual: paste a 200-line generated query from an ORM log, format, run, confirm identical results and row counts.

## Risks / open questions
- `sqlformat` opinions (line breaks before `AND`, how it indents `CASE`, comma placement) may not match user taste; only indent and keyword case are exposed in v1. Revisit if feedback demands more.
- Very large buffers (multi-MB dumps) would freeze the UI thread; cap formatting at ~1 MB with `Buffer is too large to format`, or move to a background task if a cap is unpopular.
- If the editor's undo cannot integrate, formatting becomes destructive without a safety net; that would justify holding this feature until the undo path exists.
