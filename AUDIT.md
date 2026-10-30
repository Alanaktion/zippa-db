# Audit — Zippa DB — remaining issues

**Revision audited:** `60e7002` "Add schema search dialog with full catalog" (2026-09-19), branch `main`.
**Audit date:** 2026-09-21. **Last revised:** 2026-10-29, §13 (a read of everything merged since the 2026-10-20 cleanup), then pruned of items since fixed.
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

There are no "unfinished feature" placeholders in the usual sense: no `todo!()`/`unimplemented!()` and no literal `TODO` in `src/`. The incompleteness here is of the quieter kind — one engine doing less than its sibling without saying so, and memory that grows with a result.

---

## 1. Incomplete features and stubs

### [M] The catalog's source rows are read whole

`Connection::catalog` now stops building entries at `MAX_ENTRIES` (200 000) and only counts the rest, but the three per-kind queries (columns, indexes, triggers) are still fully materialised `QueryResult`s first. A very large schema costs its rows once in memory before the cap applies.

### [L] Postgres `cell` still has no arm for a few types

The geometric types, `hstore`, the six built-in ranges and `macaddr8` now read back as the text Postgres prints (pinned by `live_postgres_geometry_ranges_hstore_and_macaddr8_read_back_as_text`). What is left falls to the `_ => value!(String)` arm and renders as a `<TYPE>` stand-in: arrays of ranges (sqlx reads an empty element as unbounded), `box[]` (its elements are `;`-separated), `macaddr8[]`, and types sqlx has no mapping for that are not text on the wire, such as `tsvector`, `pg_lsn`, and multiranges (`src/db/postgres.rs`, `cell`). A `numeric[]` still shows each element widened to whole base-10000 digits (`1.5000`), since only a single `numeric` and a `numrange` read their display scale from the raw value.

### [L] Import pre-flight heuristics

The "destructive statement" count only sees the first 64 KiB of the dump (`src/db/import/mod.rs`, `HEAD`), so a `DROP`/`TRUNCATE`/unbounded `DELETE` further in is missed. (The `WHERE`-detection gap this used to also list — `" WHERE "` as a literal substring missing a boundary-punctuated clause like `WHERE(id>0)` — is fixed: `destructive_count` now checks for `WHERE` as a standalone word via `contains_word`.)

### [M] Export reads the whole table and the whole rendering into memory

`TableView::export` runs `select_sql(paged: false)` into one `QueryResult` and then builds the entire output `String` before one write (`src/ui/table_view/export.rs`). streaming is a known open item — noted because it is the largest unbounded allocation in the app.

### [L] Other partial edges

- Stray `COPY` data outside a `COPY` header is silently dropped with no error and nothing in the summary (`src/db/import/mod.rs`).
- The SQLite rebuild hardcodes the scratch name `new_{name}` (`src/ui/schema_view/rebuild.rs`); a table already called `new_items` fails the rebuild.
- The column type picker is a fixed per-engine list with no free-text entry (`src/ui/schema_view/sql.rs`, `src/ui/schema_view/columns.rs` `render_type_picker`) — `bytea`, `inet`, enums, `year` can be displayed but never set. The doc says "Not exhaustive".
- The plan viewer keeps only a node's total cost, dropping the startup cost it stores (`src/db/plan.rs` `PlanNode` vs `src/ui/plan_view.rs`).

---

## 2. Inconsistencies

### Engine divergence

- **[M] Timestamps are labelled differently per engine.** Postgres renders `timestamptz` in the local zone with an explicit offset (`src/db/postgres.rs`); MySQL renders `TIMESTAMP` as a bare UTC wall clock (`src/db/mysql.rs`), which happens to be right only because sqlx defaults the MySQL session to `time_zone='+00:00'`. A `TIMESTAMP` and a `DATETIME` therefore look identical while meaning different things.
- **[L] The `INSERT` export is not schema-qualified** while every other generated statement is: `export_result` passes `self.object.name` (`src/ui/table_view/export.rs`) and `render_sql` quotes it bare (`src/db/export.rs`), whereas `TableView::target()` qualifies with the schema (`src/ui/table_view/sql.rs`). Exporting `myschema.items` writes `insert into items …`.
- **[L] The editor splitter and the dump splitter are different tools.** `db/statement.rs` has no trigger-body, `DELIMITER`, `\.`-meta-line or `COPY` handling, all of which `db/import/splitter.rs` has. `Cmd+Shift+Enter` on a `CREATE TRIGGER … BEGIN … END;` or a `COPY … FROM stdin` header splits in the wrong place. `import/splitter.rs` notes the editor's version covers "the common cases", but nothing at the editor says so.
- **[L] `EXPLAIN_OPTIONS` is a fixed list**, so an unknown option word silently becomes part of the wrapped statement (`src/db/statement.rs`).

### Behaviour that differs between two paths to the same thing

- **[L] A NULL is shown differently in the grid and the row panel.** The grid renders it as the word `NULL` (`src/ui/data_grid/format.rs`); the row panel shows an empty box (`src/ui/table_view/row_panel.rs`). Both coerce a typed `null` per the setting, and the value dialog does too.
- **[L] Three different guards for "a dialog is already open".** `window.has_active_dialog` (`src/ui/quick_switcher.rs`, `src/ui/schema_search.rs`, `src/ui/shortcuts_dialog.rs`, `src/ui/user_management.rs`), a struct field (`Session::import`, `Session::create_database`, `Session::variables_dialog`), and none at all (`src/ui/welcome/mod.rs`, `src/ui/session/save_dialog.rs`).
- **[L] The same failure is reported two ways at once.** `Welcome::connect` sets the inline banner *and* raises a toast; `connect_saved` shows only the banner; `settings::update` shows neither (`src/settings.rs`).
- **[L] `FilterBar::set_columns` wipes its rows while `RowPanel::sync_columns` keeps its filter**, so a stale column pattern can outlive the columns it was written for.

### Duplication and dead code

- **[L] `GridEdit::Staged` fires even when nothing changed** — after `add_draft`, `stage_cell`, `set_selected_deleted`, `reset_cell`, `discard`, `apply_staged`, each of which can return early (`src/ui/data_grid/mod.rs`). The owner cannot distinguish a real change from a no-op.
- **[L] `store.rs`'s keychain functions are `#[cfg(test)]` no-op stubs** (`src/db/store.rs`), so no test exercises the real contract, and each call site has two code paths to keep right.
- **[L] `SessionPanel::commit`, `TableView::commit`, and `ApplyEdits` are three names for one operation** (`src/ui/session/panel.rs`, `src/ui/table_view/mod.rs`, `src/keymap.rs`).
- **[L] "A session always shows one editor" is implemented in three places** with three different orderings — `Session::forget` (`src/ui/session/mod.rs`), `close_tab_now` (`src/ui/session/tabs.rs`), `restore` (`src/ui/session/state.rs`).
- **[L] `WorkspaceState::version` is written but never read** (`src/workspace_state.rs`) — so nothing can detect a file this build cannot safely read.

### Accessibility (audited against `AGENTS.md` § Accessibility)

The checklist largely holds: every icon-only button in `app.rs`, `welcome/*`, `session/panel.rs` and `session/sidebar.rs` carries an `accessibility_label`, the sidebar object list is a `Tree`, and shortcuts are discoverable via `tooltip_with_action` in the main chrome. The gaps:

- **[L] New/changed rows in the structure tab are tinted only** (`src/ui/schema_view/columns.rs`, `indexes.rs`, `foreign_keys.rs`, each section's row renderer), whereas dropped rows also get `line_through()`.
- **[L] The table view's limit `NumberInput` is labelled only by an adjacent `div`** (`src/ui/table_view/footer.rs`); `NumberInput` has no accessible-name setter in `gpui-component` 0.6. ⚠︎ Not verified with a screen reader.

---

## 3. Documentation drift

The README, `AGENTS.md`, and the doc comments the audit found contradicting their code were brought in line in the 0.1 cleanup; `AGENTS.md` is a symlink to `CLAUDE.md`, so the two cannot drift.

---

## 4. Code warnings and risks

### Blocking I/O on the UI thread

The keychain read, the `connections.json`/settings/dump-pre-flight writes, and the restored-tab reads have all moved to GPUI's background executor. What is left:

- **The startup loads are synchronous.** `Welcome::new`, `Workspace`'s restore, and `settings::init` read their files inline (`src/db/store.rs`, `src/settings.rs`). The launcher and the restored tabs are built from those reads, so this is inherent to the startup path rather than a stray call — noted so the difference from the backgrounded writes is deliberate.

### Durability and races

- **Ignored `Result`s** (all deliberate-looking, listed for completeness): `let _ = tx.send(...)` (`src/db/runtime.rs`), import `ROLLBACK`/`SET FOREIGN_KEY_CHECKS=1`/`SET UNIQUE_CHECKS=1` `.ok()` and `sender.send(...)` (`src/db/import/mod.rs`), SQLite `rollback()`/`PRAGMA foreign_keys = ON` `.ok()` (`src/db/sqlite.rs`), and `update_in(...).ok()` throughout the UI, which hides "the entity is gone" errors.
- **Errors that reach only stderr.** A failed settings write (`src/settings.rs`) leaves the UI claiming success.

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
- **`is_placeholder` misclassifies genuine short bracketed values.** `src/db/query.rs` treats any `<…>` whose inner text has no brackets, parentheses, comma, or space as a driver stand-in, so a real `<nil>`, `<item>` or `<3 bytes>` text value is treated as "never read back": uneditable (`src/ui/data_grid/delegate.rs`), claims so in the value dialog, dropped from copies, and exported as `NULL`/empty (`src/db/export.rs`). The test only covers `<a>hi</a>`, `<>`, and a `circle`.
- **Cancellation arms nothing can reach, and work that outlives its tab.** The connect task's abort handle is never kept (`src/ui/welcome/mod.rs`), same for `reload_metadata` and `switch_database` (`src/ui/session/metadata.rs`), so their "cancelled" branches can only be reached by a panic, which would be misreported. Table-view loads, writes and exports are `runtime::spawn`ed and detached with no abort handle (`src/ui/table_view/mod.rs`, `export.rs`), so nothing stops a page load or an export, and closing the tab leaves the future running. `CancelQuery`/`Cmd+.` only reaches queries and imports (`src/ui/session/running.rs`).
- **Unchecked indexing / arithmetic** (all currently safe, listed for completeness): `self.tabs[self.active]` (`src/app.rs`, safe because `tabs` is never empty), `self.panels[index]` with a modulo (`src/ui/session/files.rs`), and `page * limit` / `limit`/`offset` interpolation (`src/ui/table_view/sql.rs`, `footer.rs`), clamped by `settings::MAX_PAGE_SIZE`.
- **`describe` failures are swallowed.** `Executor::describe(pool, statement)` is called with no bound parameters and `unwrap_or_default()`ed (`src/db/connection.rs`), so a parameterised statement that returns no rows can silently lose its column list. ⚠︎ Unverified against a live Postgres; every in-tree caller casts its placeholders.

---

## 5. Test-suite gaps

Coverage is concentrated in SQLite-backed and pure-logic paths. Gaps worth naming:

- **The right-click menu is untested by design** — the test harness reports the `PopupMenu` as a leaked entity (see the note in `src/ui/tests/mod.rs`). That leaves `menu_row`/`menu_cell` staleness, the export gate, and `stop_right_click` unverified.
- **In `#[cfg(test)]` the keychain is a no-op** (`src/db/store.rs`), so the real credential-store contract is never exercised and each call site has two code paths to keep right.
- **Column dragging, the row-limit entry/stepper, and a grid keybinding inside a query tab** have no tests (`src/ui/table_view/mod.rs` `on_limit_event`/`on_limit_step`; `src/ui/tests/rows.rs` calls `copy_as` directly).
- **MySQL is only tested for metadata and imports through paths that cannot reach most of them** — every pure test under `src/db/tests/` opens a SQLite `TempDatabase`, and the MySQL rollback→stop fallback and session-variable restore (`src/db/import/mod.rs`) are MySQL-only.
- **Live-server tests are `#[ignore]`d by default** and run only in `.github/workflows/live-tests.yml` (nightly, on push to `main`, and on dispatch); each doc comment names its container. The timestamp-labelling divergence is still unverified end to end.

## 6. CI

`.github/workflows/ci.yml` runs `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` in one `ubuntu-latest` job. `.github/workflows/live-tests.yml` runs the ignored live tests against `compose.yaml`'s MySQL and Postgres; it is not a required check, which is how a MySQL failure once sat red for two days while `ci.yml` stayed green. Release packaging (`.github/workflows/release.yml`) builds installers on macOS, Linux, and Windows on version tags, so the platforms are covered at release time; the `ci.yml` test jobs themselves still run on Linux only. `script/linux` installs the Linux build dependencies; nothing equivalent guards the other two platforms for local development.

## 7. Suggested triage order

1. **Agent reads that are not bounded** (§13) — the last place model-written SQL can make the server read without limit.
2. **Export streaming** (§1) and the **catalog's source rows** (§1) — the two largest unbounded allocations.
3. **The open design calls in §11.**

## 8. What this audit did and did not cover

- **Read in full (at the audit revision):** every file under `src/` (78 `.rs` files) plus `README.md`, `TODO.md`, `AGENTS.md`, `CLAUDE.md`, the since-merged `IDEAS.md`/`IDEAS2.md`, `Cargo.toml`, `about.toml`, `.editorconfig`, `.github/workflows/ci.yml`, and the vendored `gpui-component`/`gpui-pre` sources where a finding depended on their defaults.
- **Not run for the audit:** the app itself and no live server; every UI symptom and server-behaviour claim above is marked ⚠︎ inferred or "unverified" rather than asserted. MySQL and Postgres containers were later used for the three ignored live-server tests (see §5).
- **Not audited:** packaging (`cargo packager`) and the icon assets; the bundled theme files; licensing (`about.toml` lists `Apache-2.0`/`MIT` as accepted, and `LICENSE` was not cross-checked against them).

## 9. Stability audit — hangs and crashes (2026-09-26, revision `3303ae4`)

Prompted by UI hangs and crashes on macOS dev builds; static reading of all non-test code for panic sources, UI-thread stalls, and GPUI/macOS misuse. Everything it found was fixed in `08d8c38` except:

- **[L] Query editor clones and re-splits the whole buffer on re-render** (`src/ui/query_editor.rs`, `compute_statement` → `statement::at_cursor`). No per-keystroke subscription, so only a hitch with multi-MB buffers. The toolbar's script-blocker check is memoized on a hash of the buffer, but still hashes the whole text per render.

## 11. Polish audit — crashes, hangs, and inconsistencies (2026-10-05, revision `6558db1`)

A second full read of `src/` after #4–#9 landed, looking for panics, work that hangs or outlives what started it, lost data, and places where two paths to the same thing disagree. The fixes made in the same change are in the git history.

Left open, because each needs a design call:

- **[M] A script run without a transaction, inside an open one** (`script::ScriptRun`, `ScriptMode::Autocommit` on a pinned connection whose `TxnState` is `Open`): the statements join the tab's transaction rather than committing, the question's wording says otherwise, and on Postgres the first failure aborts the whole transaction so every later statement fails with `25P02`. Either refuse the mode while a transaction is open or word the question for it.
- **[L] Re-running straight after cancelling a paused script** can say "still running something" until the close on the runtime finishes (`panel::close_script`).
- **[L] `EXPLAIN` runs on the pool, not the tab's pinned connection**, so it cannot see the tab's temporary tables, `SET`s, or `USE`.
- **[L] Row panel text is lost on `Cmd+S` or a row change** if the field has not been committed by a blur (`src/ui/table_view/row_panel.rs`).
- **[L] A binary value's preview is dropped while the value dialog is open** over it.
- **[L] The structure tab cannot be refreshed** from the server without closing and reopening it.
- **[L] The sidebar filters every object per visible row per frame** (`ui/session/sidebar.rs`); fine at hundreds of objects, noticeable at tens of thousands.
- **[L] A page reload that lands while edits are being written** still drops the edits typed during the write.
- **[L] Environment the driver still reads.** `PGOPTIONS` and, in `Prefer` mode, `PGSSLROOTCERT` reach sqlx from the environment; sqlx offers no way to opt out.
- **[L] `~/.pgpass` is no longer read** (a side effect of the fix above). If it is wanted back, it should be an explicit per-connection option.
- **[L] Grid commands bound to plain keys are not in the menu bar** (`EditCell` on `Enter`, `ToggleRow` on `Space`, `ExtendSelectionUp`/`Down` on `Shift+↑`/`↓`): on macOS a menu key equivalent fires before the focused view sees the key, so listing them would take `Enter` and `Space` from text boxes. They are listed in the shortcuts dialog.
- **[L] Smaller notes.** `REINDEX SCHEMA` and `SET SESSION autocommit` are not in `transaction_blocker`; SQLite maintenance confirmations can stack on a double click; the structure tab's per-row checkboxes share one accessible name; the foreign-key jump uses the loaded value rather than a staged edit; the editor still says "Leave blank to keep the saved one" after the SSH auth method changes.

## 12. Cleanup audit (2026-10-15, revision `64954cc`)

A read of what landed after §11 — query variables, saved queries, create database, new-table creation, user management, the query formatter, the command palette's ranking, and the ER diagram — plus `cargo update`. The fixes made in the same change are in the git history.

Left open:

- **[L] The Postgres user list shows every role,** the built-in `pg_*` roles included (`postgres::USERS_SQL`).

## 13. Feature audit (2026-10-29, revision `0177721`)

A read of what was merged after the §12 cleanup and the 2026-10-20 backlog revision: the Agents panel (`db/agent/`, `ui/agents/`), backups through `pg_dump`/`mysqldump` (`db/backup.rs`, `ui/backup_dialog.rs`), dashboards and charts (`ui/session/dashboard.rs`, `chart.rs`), the implicit `LIMIT` (`session/limit.rs`, `statement::with_limit`/`clamp_limit`), staging a binary value from a file, the structure tab's read-only definition (`db/ddl.rs`, `schema_view/definition.rs`), the table view's quick search, the Messages pane, and connection information. The checks were the three CI commands plus the live tests against `compose.yaml`. The fixes made in the same change (agent reads on a read-only session, hex encoding, clippy, a timezone-dependent test, a doc comment) are in the git history.


Left open:

### Agents panel

- **[M] An agent read is not always bounded.** `run_select` relies on `statement::clamp_limit`, which returns `None`, leaving the SQL as written, for an outermost `OFFSET` with no `LIMIT`, a `LIMIT` given as an expression, or a locking clause. The comment in `run_select` says "the cap is enforced in SQL", but in these cases the server reads the whole result, and `fetch_all` holds all of it in memory before `format_result` trims it to 100 rows. Wrapping the statement as `SELECT * FROM (…) LIMIT n` would cover these cases, but MySQL refuses a derived table with duplicate column names (`a.id, b.id`), so that wrapper would break joins that work now.
- **[M] A read-only session does not stop every side effect.** Postgres still allows `pg_terminate_backend`, `pg_cancel_backend`, `pg_advisory_lock`, `dblink_exec` (which writes over another connection), `lo_export` for a superuser, and `set_config`. MySQL still allows `GET_LOCK` and `SLEEP`. A read-only session keeps the agent from changing data, not from affecting the server. `gate::FORBIDDEN` only looks at a statement's first word, so a denylist of function names would be needed to close this.
- **[L] The keychain is used on the UI thread from Settings.** `settings_window::key_set` reads the API key the first time the Agents page is drawn. The key box's change callback writes `store::set_api_key` on every change, and a failed write is ignored, so the box keeps showing the key as saved (`src/ui/settings_window.rs`). A turn's own read (`ui/agents/mod.rs`) runs on the Tokio runtime and blocks one of its two threads while the keychain answers.
- **[L] New connections get `Schema` access by default** (`AgentAccess::default`). No schema is sent anywhere until the user sets an endpoint, and the Settings page says what will be sent. But once an endpoint is set, every saved connection, production ones included, offers its table and column names to the model unless it was set to `None` one by one.

### Backups

- **[L] A database name is passed to `pg_dump` through `-d`, which also accepts a connection string,** so a database named `host=… dbname=…` would connect somewhere else (`dump_commands`, `src/db/backup.rs`). `mysqldump` now gets its names after `--`. The names come from the server's own lists, so this needs someone who can already create objects there. Refusing names that contain `=` would close it.

### Binary values

- **[L] There is no size limit on a file loaded into the value dialog.** `sql_file::read_bytes` reads it whole. Hex encoding doubles it, and the staged text is held in the grid's edits and sent as one bound parameter, so a multi-gigabyte file takes several times its size in memory and may exceed the server's packet limit (`max_allowed_packet` on MySQL) only when the edit is applied.

### Dashboards

- **[L] Refreshing a dashboard asks only on a `ConfirmWrites` connection.** That matches a query tab. But a dashboard runs every widget again on each refresh, so on a `Staged` or `AutoApply` connection a widget that writes repeats its write on every refresh with no question. A dashboard restored at startup does not run (`open_dashboard_inner(run: false)`), so the gap is limited to opening a dashboard from the sidebar or the palette, the Refresh button, and an edited widget.

### Documentation drift

- **[L] `CLAUDE.md`/`AGENTS.md` do not describe the features added since 0.1.2.** There is no architecture paragraph for the Agents panel (its three gates, `QuerySource::Agent`, the API key's keychain entry), backups, dashboards and charts, the quick search, or the structure tab's definition view, and the `src/ui/tests/` file list is missing `agents`, `chart`, `connection_info`, `dashboards`, `messages`, and `table_search`. `README.md` does not mention the Agents panel or that it sends schema and rows to a configurable endpoint.
