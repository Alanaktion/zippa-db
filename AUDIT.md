# Audit — Zippa DB — remaining issues

**Revision audited:** `60e7002` "Add schema search dialog with full catalog" (2026-09-19), branch `main`.
**Audit date:** 2026-09-21. **Last revised:** 2026-09-23, after the 0.1 cleanup.
**This revision:** findings that have since been fixed are removed, so only what is still open is listed. The 0.1 cleanup split the largest UI files into submodules (`ui/session/`, `ui/data_grid/`, `ui/table_view/`, `ui/schema_view/`), so anchors are given as `file` plus the function name rather than a line number. The resolved findings and their regression tests are in the git history.

## How this audit was done

| Check | Command | Result |
| --- | --- | --- |
| Compiler/lint warnings | `cargo clippy --all-targets` | **Clean** — zero warnings from this crate (at audit time). The only notice is a transitive future-incompatibility: `block v0.1.6`. |
| Build warnings | `cargo test` | No warnings from the crate. |
| Tests | `cargo test` | **396 passed, 0 failed** at audit time (one test binary, no doc-tests, no integration tests); see `CHANGELOG.md` for the count at the latest release. |

Everything below comes from reading the source, cross-checked against `gpui-kit`/`gpui-component` 0.6.4 and `sqlx` 0.9 sources where behaviour depended on them.

**Legend**

- **[H]** high — data loss, silently wrong data, or a feature that does not do what the UI says.
- **[M]** medium — real gap or divergence, with a workaround or a narrow trigger.
- **[L]** low — polish, naming, docs, dead code.
- ⚠︎ **inferred** — the symptom follows from the code but was not observed at runtime.

There are no "unfinished feature" placeholders in the usual sense: no `todo!()`/`unimplemented!()`, and only one literal `TODO` (`src/ui/table_view/row_panel.rs`). The incompleteness here is of the quieter kind — one engine doing less than its sibling without saying so, and memory that grows with a result.

---

## 1. Incomplete features and stubs

### [M] The catalog's source rows are read whole

`Connection::catalog` now stops building entries at `MAX_ENTRIES` (200 000) and only counts the rest, but the three per-kind queries (columns, indexes, triggers) are still fully materialised `QueryResult`s first. A very large schema costs its rows once in memory before the cap applies.

### [M] Postgres `cell` has no arm for several types sqlx can decode

`postgres.rs` handles `MACADDR` but not `MACADDR8`, and none of sqlx 0.9's geometric/hstore/lrange types (`PgPoint`, `PgBox`, `PgCircle`, `PgHstore`, ranges, …). Those fall to the `_ => value!(String)` arm, fail, and render as `<POINT>`-style stand-ins (`src/db/postgres.rs`, `cell`) — which export as NULL and cannot be edited.

### [L] Import pre-flight heuristics

The "destructive statement" count only sees the first 64 KiB of the dump (`src/db/import/mod.rs`, `HEAD`), so a `DROP`/`TRUNCATE`/unbounded `DELETE` further in is missed. (The `WHERE`-detection gap this used to also list — `" WHERE "` as a literal substring missing a boundary-punctuated clause like `WHERE(id>0)` — is fixed: `destructive_count` now checks for `WHERE` as a standalone word via `contains_word`.)

### [M] Export reads the whole table and the whole rendering into memory

`TableView::export` runs `select_sql(paged: false)` into one `QueryResult` and then builds the entire output `String` before one write (`src/ui/table_view/export.rs`). `TODO.md` lists streaming as open, so this is known — noted because it is the largest unbounded allocation in the app.

### [L] Other partial edges

- Stray `COPY` data outside a `COPY` header is silently dropped with no error and nothing in the summary (`src/db/import/mod.rs`).
- The SQLite rebuild hardcodes the scratch name `new_{name}` (`src/ui/schema_view/rebuild.rs`); a table already called `new_items` fails the rebuild.
- The column type picker is a fixed per-engine list with no free-text entry (`src/ui/schema_view/sql.rs`, `src/ui/schema_view/columns.rs` `render_type_picker`) — `bytea`, `inet`, enums, `year` can be displayed but never set. The doc says "Not exhaustive".
- `foreign_keys_editable` is a no-op seam returning `is_editable()` (`src/ui/schema_view/foreign_keys.rs`).
- The plan viewer keeps only a node's total cost, dropping the startup cost it stores (`src/db/plan.rs` `PlanNode` vs `src/ui/plan_view.rs`).

---

## 2. Inconsistencies

### Engine divergence

- **[M] Timestamps are labelled differently per engine.** Postgres renders `timestamptz` in the local zone with an explicit offset (`src/db/postgres.rs`); MySQL renders `TIMESTAMP` as a bare UTC wall clock (`src/db/mysql.rs`), which happens to be right only because sqlx defaults the MySQL session to `time_zone='+00:00'`. A `TIMESTAMP` and a `DATETIME` therefore look identical while meaning different things.
- **[L] The `INSERT` export is not schema-qualified** while every other generated statement is: `export_result` passes `self.object.name` (`src/ui/table_view/export.rs`) and `render_sql` quotes it bare (`src/db/export.rs`), whereas `TableView::target()` qualifies with the schema (`src/ui/table_view/sql.rs`). Exporting `myschema.items` writes `insert into items …`.
- **[L] The editor splitter and the dump splitter are different tools.** `db/statement.rs` has no trigger-body, `DELIMITER`, `\.`-meta-line or `COPY` handling, all of which `db/import/splitter.rs` has. `Cmd+Shift+Enter` on a `CREATE TRIGGER … BEGIN … END;` or a `COPY … FROM stdin` header splits in the wrong place. `import/splitter.rs` notes the editor's version covers "the common cases", but nothing at the editor says so.
- **[L] The read-only classifier can refuse plain reads.** `statement.rs`'s allowlist omits `WITH`, so a CTE read counts as a write (`src/db/statement.rs`), and any `ANALYZE` anywhere in the words is a write — yet `Connection::explain` deliberately skips `refuse_write` and runs its `EXPLAIN`. The same text is allowed through one entry point and refused through the other (pinned by a test in `src/db/tests.rs`). `EXPLAIN_OPTIONS` is also a fixed list, so an unknown option word silently becomes part of the wrapped statement.

### Behaviour that differs between two paths to the same thing

- **[M] Stand-ins are handled two ways across the copy paths.** `Cmd+C` puts the driver's description on the clipboard verbatim (`src/ui/data_grid/clipboard.rs` `copy`, asserted in `src/ui/tests/navigation.rs`), whereas `Cmd+Shift+C` and the `Copy as` submenu route through `export`, which writes `NULL` and counts it.
- **[L] `MAX_COPY_BYTES` is not applied to every copy.** Row copies are now capped at `MAX_COPY_ROWS` before any row is cloned, but the byte cap still skips single-cell copies and both column shapes (`src/ui/data_grid/clipboard.rs`).
- **[L] A NULL is shown differently in the grid and the row panel.** The grid renders it as the word `NULL` (`src/ui/data_grid/format.rs`); the row panel shows an empty box (`src/ui/table_view/row_panel.rs`). Both coerce a typed `null` per the setting, and the value dialog does too.
- **[L] Three different guards for "a dialog is already open".** `window.has_active_dialog` (`src/ui/quick_switcher.rs`, `src/ui/schema_search.rs`, `src/ui/shortcuts_dialog.rs`), a struct field (`Session::import`, `src/ui/session/files.rs`), and none at all (`src/ui/welcome/mod.rs`).
- **[L] The same failure is reported two ways at once.** `Welcome::connect` sets the inline banner *and* raises a toast; `connect_saved` shows only the banner; `settings::update` shows neither (`src/settings.rs`).
- **[L] `FilterBar::set_columns` wipes its rows while `RowPanel::sync_columns` keeps its filter**, so a stale column pattern can outlive the columns it was written for.

### Duplication and dead code

- **[L] Identifier-target SQL is duplicated.** `schema_view/sql.rs` re-implements `TableView::target` (its own doc admits it), and `columns_list` exists twice with different signatures (`src/ui/schema_view/sql.rs`, `src/ui/schema_view/rebuild.rs`).
- **[L] `GridEdit::Staged` fires even when nothing changed** — after `add_draft`, `stage_cell`, `set_selected_deleted`, `reset_cell`, `discard`, `apply_staged`, each of which can return early (`src/ui/data_grid/mod.rs`). The owner cannot distinguish a real change from a no-op.
- **[L] `store.rs`'s keychain functions are `#[cfg(test)]` no-op stubs** (`src/db/store.rs`), so no test exercises the real contract, and each call site has two code paths to keep right.
- **[L] `SessionPanel::commit`, `TableView::commit`, and `ApplyEdits` are three names for one operation** (`src/ui/session/panel.rs`, `src/ui/table_view/mod.rs`, `src/keymap.rs`).
- **[L] "A session always shows one editor" is implemented in three places** with three different orderings — `Session::forget` (`src/ui/session/mod.rs`), `close_tab_now` (`src/ui/session/tabs.rs`), `restore` (`src/ui/session/state.rs`).
- **[L] Two `#[allow]` suppressions**, both justified in comments: `#[allow(deprecated)]` on `fetch_many` (`src/db/connection.rs`) and `#[allow(clippy::large_enum_variant)]` on `TabContent` (`src/ui/session/tab.rs`).
- **[L] `WorkspaceState::version` is written but never read** (`src/workspace_state.rs`) — so nothing can detect a file this build cannot safely read.

### Accessibility (audited against `AGENTS.md` § Accessibility)

The checklist largely holds: every icon-only button in `app.rs`, `welcome/*`, `session/panel.rs` and `session/sidebar.rs` carries an `accessibility_label`, the sidebar object list is a `Tree`, and shortcuts are discoverable via `tooltip_with_action` in the main chrome. The gaps:

- **[L] New/changed rows in the structure tab are tinted only** (`src/ui/schema_view/columns.rs`, `indexes.rs`, `foreign_keys.rs`, each section's row renderer), whereas dropped rows also get `line_through()`.
- **[L] The table view's limit `NumberInput` is labelled only by an adjacent `div`** (`src/ui/table_view/footer.rs`); `NumberInput` has no accessible-name setter in `gpui-component` 0.6. ⚠︎ Not verified with a screen reader.

---

## 3. Documentation drift

- **[L] Read-only refusal wording is inconsistent** — "this connection is read-only, so a dump cannot be imported", "…so the {word} statement was not run", and a bare "this connection is read-only" (`src/db/connection.rs`); some messages end with a period, others do not.

The README, `AGENTS.md`, and the doc comments the audit found contradicting their code were brought in line in the 0.1 cleanup; `AGENTS.md` is a symlink to `CLAUDE.md`, so the two cannot drift.

---

## 4. Code warnings and risks

### Blocking I/O on the UI thread

The keychain read, the `connections.json`/settings/dump-pre-flight writes, and the restored-tab reads have all moved to GPUI's background executor. What is left:

- **The startup loads are synchronous.** `Welcome::new`, `Workspace`'s restore, and `settings::init` read their files inline (`src/db/store.rs`, `src/settings.rs`). The launcher and the restored tabs are built from those reads, so this is inherent to the startup path rather than a stray call — noted so the difference from the backgrounded writes is deliberate.

### Durability and races

- **`record_connected` is a read-modify-write with no locking** (`src/db/store.rs`), so it can overwrite a concurrent save from the connection editor and lose a newly added connection. (It now runs in the background, but the race remains.)
- **A checkpoint can interleave with the quit-time flush.** Both write the same `workspace.json.tmp` then rename (`Workspace::save_state`/`flush_state` in `src/app.rs`, `src/workspace_state.rs`); a checkpoint spawned just before quit can write the older snapshot between the flush's write and its rename. ⚠︎ Inferred; the save is a no-op under `#[cfg(test)]`.
- **`save_state`/`flush_state` record the snapshot as saved before it is written** (`src/app.rs`), so a failed write is never retried. Failure is only an `eprintln!`.
- **Plaintext query buffers are written with default permissions.** `workspace_state::save` uses `fs::write` with no mode; the buffers are the user's own SQL, which `README.md` acknowledges may hold pasted secrets. Nothing in the crate calls `set_permissions`/`PermissionsExt`.
- **Ignored `Result`s** (all deliberate-looking, listed for completeness): `let _ = tx.send(...)` (`src/db/runtime.rs`), import `ROLLBACK`/`SET FOREIGN_KEY_CHECKS=1`/`SET UNIQUE_CHECKS=1` `.ok()` and `sender.send(...)` (`src/db/import/mod.rs`), SQLite `rollback()`/`PRAGMA foreign_keys = ON` `.ok()` (`src/db/sqlite.rs`), and `update_in(...).ok()` throughout the UI, which hides "the entity is gone" errors.
- **Errors that reach only stderr.** A failed settings write (`src/settings.rs`), a failed `record_connected` (`src/ui/welcome/mod.rs`), and a failed workspace save (`src/app.rs`) all leave the UI claiming success.

### Panics in non-test code

Small in number and mostly guarded, but they are panics in paths that process user input:

- `src/main.rs` — `.expect("failed to open window")`.
- `src/db/runtime.rs` — `Builder::…build().expect("failed to start the database runtime")` inside a `OnceLock` initialiser, i.e. on first database use rather than at a point where the user could be told.
- `src/ui/shortcuts_dialog.rs` — `panic!` on an unparsable keystroke in the constant table (self-checked by the `every_listed_keystroke_parses` test, so a build-time-style guard).
- `src/ui/filter_bar.rs` — `.expect("every operator is in ALL")` (the only `expect` outside tests in the grid/table-view area).
- `src/db/statement.rs` — `dollar_tag(...).expect(...)` / `word_at(...).expect(...)`, guarded by the arms above but on the editor's per-keystroke path.
- `src/ui/schema_view/sql.rs`, `src/ui/schema_view/rebuild.rs` — infallible-guard expects.
- `src/ui/schema_view/sql.rs` — `unreachable!()` in `generate_foreign_key_statements`, guarded ~55 lines earlier by the `Sqlite && any(wants_a_statement)` early return.

### Injection-adjacent and safety waivers

- `AssertSqlSafe` waives sqlx's guard on both the user's buffer and generated SQL (`src/db/connection.rs`). The generated paths rely entirely on `quote_identifier`/`quote_literal`.
- DDL interpolates free-text: `column_clause` pastes `edit.default` (documented as "Raw expression text, bound for `DEFAULT` verbatim", `src/db/schema.rs`) and `edit.type_name` straight into the statement (`src/ui/schema_view/sql.rs`), and Postgres reuses the type in `USING {current}::{type}`. A default like `0; DROP …` is not quoted or rejected client-side; the statements run through prepared `execute`, so this is injection-adjacent rather than directly exploitable.
- **[L] `rebuild.rs` rewrites identifiers textually**, so renaming `items.name` also rewrites a trigger's reference to a *different* table's `name` (`src/ui/schema_view/rebuild.rs`). Inherent to text-based replay, only partly guarded.

### Memory and hot paths

- **Every result is materialised before it is rendered.** `fetch_all` collects the whole row stream into `Vec<Row>` and converts each cell to an owned `String` (`src/db/connection.rs`). The grid virtualizes only the *rendering*, so memory is proportional to the result; the README says so.
- **Per-frame allocation in the row panel.** `render_field` calls `is_field_editable` and `needs_a_window` for every visible field (`src/ui/table_view/row_panel.rs`), and both end in `query::is_binary_type`, which allocates an uppercased `String` per call (`src/db/query.rs`).
- **`is_placeholder` misclassifies genuine short bracketed values.** `src/db/query.rs` treats any `<…>` whose inner text has no brackets or space as a driver stand-in, so a real `<nil>`, `<item>` or `<3 bytes>` text value is treated as "never read back": uneditable (`src/ui/data_grid/delegate.rs`), claims so in the value dialog, dropped from copies, and exported as `NULL`/empty (`src/db/export.rs`). The test only covers `<a>hi</a>` and `<>`.
- **Cancellation arms nothing can reach, and work that outlives its tab.** The connect task's abort handle is never kept (`src/ui/welcome/mod.rs`), same for `reload_metadata` and `switch_database` (`src/ui/session/metadata.rs`), so their "cancelled" branches can only be reached by a panic, which would be misreported. Table-view loads, writes and exports are `runtime::spawn`ed and detached with no abort handle (`src/ui/table_view/mod.rs`, `export.rs`), so nothing stops a page load or an export, and closing the tab leaves the future running. `CancelQuery`/`Cmd+.` only reaches queries and imports (`src/ui/session/running.rs`).
- **Unchecked indexing / arithmetic** (all currently safe, listed for completeness): `self.tabs[self.active]` (`src/app.rs`, safe because `tabs` is never empty), `self.panels[index]` with a modulo (`src/ui/session/files.rs`), and `page * limit` / `limit`/`offset` interpolation (`src/ui/table_view/sql.rs`, `footer.rs`), clamped by `settings::MAX_PAGE_SIZE`.
- **`describe` failures are swallowed.** `Executor::describe(pool, statement)` is called with no bound parameters and `unwrap_or_default()`ed (`src/db/connection.rs`), so a parameterised statement that returns no rows can silently lose its column list. ⚠︎ Unverified against a live Postgres; every in-tree caller casts its placeholders.

---

## 5. Test-suite gaps

Coverage is concentrated in SQLite-backed and pure-logic paths. Gaps worth naming:

- **The right-click menu is untested by design** — the test harness reports the `PopupMenu` as a leaked entity (see the note in `src/ui/tests/mod.rs`). That leaves `menu_row`/`menu_cell` staleness, the export gate, and `stop_right_click` unverified.
- **In `#[cfg(test)]` the keychain is a no-op** (`src/db/store.rs`), so the real credential-store contract is never exercised and each call site has two code paths to keep right.
- **Column dragging, the row-limit entry/stepper, and a grid keybinding inside a query tab** have no tests (`src/ui/table_view/mod.rs` `on_limit_event`/`on_limit_step`; `src/ui/tests/rows.rs` calls `copy_as` directly).
- **MySQL is only tested for metadata and imports through paths that cannot reach most of them** — every pure test in `src/db/tests.rs` opens a SQLite `TempDatabase`, and the MySQL rollback→stop fallback and session-variable restore (`src/db/import/mod.rs`) are MySQL-only.
- **Only three tests run against a live server, all ignored by default.** `db::tests::live_mysql_routines_carry_their_argument_types`, `live_mysql_table_schema_carries_auto_increment_collation_comment_and_on_update`, and `live_postgres_money_scales_by_the_servers_locale_not_always_by_100` (each doc comment names the container); the other engine-divergence findings (timestamp labelling, unsupported-type stand-ins) are still unverified end to end.

## 6. CI

`.github/workflows/ci.yml` runs `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` in one `ubuntu-latest` job. There is still no macOS or Windows build — on a project whose stated premise is cross-platform and whose build has a documented macOS-only Metal prerequisite. `script/linux` installs the Linux build dependencies; nothing equivalent guards the other two platforms.

## 7. Suggested triage order

1. **Postgres `cell` arms** (§1) — geometric/hstore/range/`MACADDR8` values currently stand in as unreadable and uneditable.
2. **Copy-path consistency** (§2) — make `Cmd+C` treat a stand-in the way `Copy as` does, and apply the byte cap to every copy.
3. **Export streaming** (§1) and the **catalog's source rows** (§1) — the two largest unbounded allocations.
4. **Durability** (§4) — the `record_connected` race and the checkpoint/flush interleaving on `workspace.json`.
5. **CI on macOS and Windows** (§6).

## 8. What this audit did and did not cover

- **Read in full (at the audit revision):** every file under `src/` (78 `.rs` files) plus `README.md`, `TODO.md`, `AGENTS.md`, `CLAUDE.md`, the since-merged `IDEAS.md`/`IDEAS2.md`, `Cargo.toml`, `about.toml`, `.editorconfig`, `.github/workflows/ci.yml`, and the vendored `gpui-component`/`gpui-pre` sources where a finding depended on their defaults.
- **Not run for the audit:** the app itself and no live server; every UI symptom and server-behaviour claim above is marked ⚠︎ inferred or "unverified" rather than asserted. MySQL and Postgres containers were later used for the three ignored live-server tests (see §5).
- **Not audited:** packaging (`cargo packager`) and the icon assets; the bundled theme files; licensing (`about.toml` lists `Apache-2.0`/`MIT` as accepted, and `LICENSE` was not cross-checked against them).
