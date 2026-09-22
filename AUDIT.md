# Audit — Zippa DB — remaining issues

**Revision audited:** `60e7002` "Add schema search dialog with full catalog" (2026-09-19), branch `main`.
**Audit date:** 2026-09-21.
**This revision:** the findings that have since been fixed are removed, so only what is still open is listed. The `file:line` anchors date from the audit revision and have drifted with the fixes; treat them as a starting point, not an exact position. The resolved findings and their regression tests are in the git history (the commits touching `src/app.rs`, `src/db/mysql.rs`, `src/settings.rs`, `src/ui/welcome/mod.rs`, `src/ui/data_grid/`, `src/ui/import_dialog.rs`, `src/ui/session/mod.rs`, `src/menu.rs`, `src/db/connection.rs`, `src/db/schema.rs`, `src/db/postgres.rs`, `src/db/import/splitter.rs`, and `src/ui/schema_view/`).

## How this audit was done

| Check | Command | Result |
| --- | --- | --- |
| Compiler/lint warnings | `cargo clippy --all-targets` | **Clean** — zero warnings from this crate (at audit time). The only notice is a transitive future-incompatibility: `block v0.1.6`. |
| Build warnings | `cargo test` | No warnings from the crate. |
| Tests | `cargo test` | **396 passed, 0 failed** at audit time (one test binary, no doc-tests, no integration tests). |

Everything below comes from reading the source, cross-checked against `gpui-kit`/`gpui-component` 0.6.4 and `sqlx` 0.9 sources where behaviour depended on them.

**Legend**

- **[H]** high — data loss, silently wrong data, or a feature that does not do what the UI says.
- **[M]** medium — real gap or divergence, with a workaround or a narrow trigger.
- **[L]** low — polish, naming, docs, dead code.
- ⚠︎ **inferred** — the symptom follows from the code but was not observed at runtime.

There are no "unfinished feature" placeholders in the usual sense: no `todo!()`/`unimplemented!()`, and only one literal `TODO` (`src/ui/table_view/row_panel.rs`). The incompleteness here is of the quieter kind — a doc that promises more than the code does, and one engine doing less than its sibling without saying so.

---

## 1. Incomplete features and stubs

### [M] The catalog cap is applied only after everything is built

`Connection::catalog` materialises every column/index/trigger row and every entry, then calls `entries.truncate(MAX_ENTRIES)` (`src/db/connection.rs:285-307`). `MAX_ENTRIES` is 200 000 (`src/db/catalog.rs:21`) and the doc says "a database past this is truncated rather than held whole" (`src/db/catalog.rs:19-20`) — which is exactly what the code does not do. The three per-kind queries are also fully materialised `QueryResult`s first.

### [M] Postgres `cell` has no arm for several types sqlx can decode

`postgres.rs` handles `MACADDR` but not `MACADDR8`, and none of sqlx 0.9's geometric/hstore/lrange types (`PgPoint`, `PgBox`, `PgCircle`, `PgHstore`, ranges, …). Those fall to the `_ => value!(String)` arm, fail, and render as `<POINT>`-style stand-ins (`src/db/postgres.rs:250-287`) — which export as NULL and cannot be edited.

### [M] Import pre-flight heuristics

The "destructive statement" count treats a `DELETE` as destructive only when the uppercased text contains `" WHERE "` (`src/db/import/mod.rs:580-592`), so `DELETE FROM t WHERE(id>0)` under-counts; the scan only sees the first 64 KiB of the dump (`src/db/import/mod.rs:37`).

### [M] Export reads the whole table and the whole rendering into memory

`TableView::export` runs `select_sql(paged: false)` into one `QueryResult` and then builds the entire output `String` before one write (`src/ui/table_view/mod.rs:778-800`, `:856-868`). `TODO.md:56` still lists streaming as open, so this is known — noted because it is the largest unbounded allocation in the app.

### [L] Other partial edges

- Stray `COPY` data outside a `COPY` header is silently dropped with no error and nothing in the summary (`src/db/import/mod.rs:281-287`).
- The SQLite rebuild hardcodes the scratch name `new_{name}` (`src/ui/schema_view/rebuild.rs:200`); a table already called `new_items` fails the rebuild.
- The column type picker is a fixed per-engine list with no free-text entry (`src/ui/schema_view/sql.rs:91-127`, `src/ui/schema_view/mod.rs:1000-1036`) — `bytea`, `inet`, enums, `year` can be displayed but never set. The doc says "Not exhaustive".
- `foreign_keys_editable` is a no-op seam returning `is_editable()` (`src/ui/schema_view/mod.rs:716-718`).
- The plan viewer keeps only a node's total cost, dropping the startup cost it stores (`src/db/plan.rs:42` vs `src/ui/plan_view.rs:451`, `:541`).
- Column reordering is enabled in the UI but never honoured by the delegate: `DataTable` is built without disabling it (`src/ui/data_grid/mod.rs:1763-1768`), `TableState::col_movable` defaults to `true` (`gpui-component-0.6.4/src/table/state.rs:233`, `:293`), and `ResultDelegate` never implements `TableDelegate::move_column` — whose default is an empty body (`gpui-component-0.6.4/src/table/delegate.rs:123-131`) while `TableState::move_column` still reorders its own `col_groups` (`state.rs:1194-1211`). ⚠︎ inferred; no test drags a header.

---

## 2. Inconsistencies

### Engine divergence

- **[M] Timestamps are labelled differently per engine.** Postgres renders `timestamptz` in the local zone with an explicit offset (`src/db/postgres.rs:266-275`); MySQL renders `TIMESTAMP` as a bare UTC wall clock (`src/db/mysql.rs:188-196`), which happens to be right only because sqlx defaults the MySQL session to `time_zone='+00:00'`. A `TIMESTAMP` and a `DATETIME` therefore look identical while meaning different things.
- **[L] The `INSERT` export is not schema-qualified** while every other generated statement is: `export_result` passes `self.object.name` (`src/ui/table_view/mod.rs:779`, `:812`) and `render_sql` quotes it bare (`src/db/export.rs:399`), whereas `TableView::target()` qualifies with the schema (`src/ui/table_view/sql.rs:29-38`). Exporting `myschema.items` writes `insert into items …`.
- **[L] The editor splitter and the dump splitter are different tools.** `db/statement.rs:37-55` has no trigger-body, `DELIMITER`, `\.`-meta-line or `COPY` handling, all of which `db/import/splitter.rs` has (`:60-80`, `:114-125`, `:191-213`). `Cmd+Shift+Enter` on a `CREATE TRIGGER … BEGIN … END;` or a `COPY … FROM stdin` header splits in the wrong place. `import/splitter.rs:1-14` notes the editor's version covers "the common cases", but nothing at the editor says so.
- **[L] The read-only classifier can refuse plain reads.** `statement.rs`'s allowlist omits `WITH`, so a CTE read counts as a write (`src/db/statement.rs:15-21`), and any `ANALYZE` anywhere in the words is a write (`:185-190`) — yet `Connection::explain` deliberately skips `refuse_write` and runs its `EXPLAIN` (`src/db/connection.rs:386-404`). The same text is allowed through one entry point and refused through the other (pinned by `src/db/tests.rs:836-855`). `EXPLAIN_OPTIONS` is also a fixed list, so an unknown option word silently becomes part of the wrapped statement (`src/db/statement.rs:79-97`, `:136-145`).

### Behaviour that differs between two paths to the same thing

- **[M] Stand-ins are handled three ways across the copy paths.** `Cmd+C` puts the driver's description on the clipboard verbatim (`src/ui/data_grid/mod.rs:793-820`, asserted at `src/ui/tests/navigation.rs:111-116`), whereas `Cmd+Shift+C` and the `Copy as` submenu route through `export`, which writes `NULL` and counts it (`src/ui/data_grid/mod.rs:837-918`, `src/db/export.rs:98-104`).
- **[M] The copy caps are uneven and the doc overstates them.** `MAX_COPY_ROWS`'s comment says a large result "is copied a page at a time" (`src/ui/data_grid/mod.rs:1663-1666`), but truncation happens *after* `snapshot()` has cloned every row × column (`:794`/`:842` then `:884-886` vs `:1210-1276`), and `MAX_COPY_BYTES` is not applied at all to single-cell copies (`:781-791`, `:823-833`) or either column shape (`:845-876`).
- **[L] A NULL is shown differently in the grid and the row panel.** The grid renders it as the word `NULL` (`src/ui/data_grid/format.rs:16-18`); the row panel shows an empty box (`src/ui/table_view/row_panel.rs:195-201`). Both coerce a typed `null` per the setting, and the value dialog now does too.
- **[L] Delete and restore are guarded differently.** `delete_rows` requires `is_editable()` (`src/ui/table_view/mod.rs:898-904`) while `restore_rows` only checks `committing` (`:907-912`).
- **[L] Three different guards for "a dialog is already open".** `window.has_active_dialog` (`src/ui/quick_switcher.rs:50`, `src/ui/schema_search.rs:106`, `src/ui/shortcuts_dialog.rs:119`), a struct field (`src/ui/session/mod.rs:808-810`), and none at all (`src/ui/welcome/mod.rs:106-144`).
- **[L] The same failure is reported two ways at once.** `Welcome::connect` sets the inline banner *and* raises a toast (`src/ui/welcome/mod.rs:276-280` + `:444-460`); `connect_saved` shows only the banner; `settings::update` shows neither (`src/settings.rs`).
- **[L] `FilterBar::set_columns` wipes its rows while `RowPanel::sync_columns` keeps its filter**, so a stale column pattern can outlive the columns it was written for (`src/ui/filter_bar.rs:141-151` vs `src/ui/table_view/row_panel.rs:99-147`).

### Duplication and dead code

- **[L] The "drop `public`" qualification rule is written three times** — `src/db/connection.rs:197-204`, `:231-238`, `:321-325` — each with the same comment. `DatabaseObject::label` and `StoredObject::label` are near-duplicates too (`:77-87`, `:92-97`).
- **[L] Identifier-target SQL is duplicated.** `schema_view/sql.rs:75-85` re-implements `TableView::target` (its own doc admits it), and `columns_list` exists twice with different signatures (`src/ui/schema_view/sql.rs:382-388`, `src/ui/schema_view/rebuild.rs:524-530`).
- **[L] Two sentinels for one idea.** `static MISSING` (`src/ui/data_grid/delegate.rs:104`) and `static ABSENT` (`:457`) are both `Cell::None`.
- **[L] `pending` duplicates `pending_counts`.** The same filter is recomputed twice (`src/ui/data_grid/mod.rs:687-695` vs `:699-707`).
- **[L] `GridEdit::Staged` fires even when nothing changed** — after `add_draft`, `stage_cell`, `set_selected_deleted`, `reset_cell`, `discard`, `apply_staged`, each of which can return early (`src/ui/data_grid/mod.rs:653-663`, `:1082-1097`, `:1109-1120`, `:1290-1301`, `:1364-1381`, `:1319-1347`). The owner cannot distinguish a real change from a no-op.
- **[L] `Query::is_empty` is `pub` but `#[cfg(test)]`** (`src/db/catalog.rs:289-296`), so the public surface differs between builds for a method with no production caller.
- **[L] `store.rs`'s keychain functions are `#[cfg(test)]` no-op stubs** (`src/db/store.rs:99-135`), so no test exercises the real contract, and each call site has two code paths to keep right.
- **[L] `SessionPanel::commit`, `TableView::commit`, `ApplyEdits` and `Session::run_now` are four names for one operation** (`src/ui/session/panel.rs:599-606`, `src/ui/table_view/mod.rs:598-607`, `src/keymap.rs:133`, `src/ui/session/mod.rs:1318-1320`).
- **[L] "A session always shows one editor" is implemented in three places** with three different orderings — `Session::forget` (`src/ui/session/mod.rs:251-255`), `close_tab_now` (`:488-492`), `restore` (`:612-620`).
- **[L] Two `#[allow]` suppressions**, both justified in comments: `#[allow(deprecated)]` on `fetch_many` (`src/db/connection.rs:651-654`) and `#[allow(clippy::large_enum_variant)]` on `TabContent` (`src/ui/session/tab.rs:51-53`).
- **[L] `WorkspaceState::version` is written but never read** (`src/workspace_state.rs:28`, `:34`, `:92-101`) — so nothing can detect a file this build cannot safely read.

### Accessibility (audited against `AGENTS.md` § Accessibility)

The checklist largely holds: every icon-only button in `app.rs`, `welcome/*`, `session/panel.rs` and `session/sidebar.rs` carries an `accessibility_label`, the sidebar object list is a `Tree`, and shortcuts are discoverable via `tooltip_with_action` in the main chrome. The gaps:

- **[L] Colour is the only cue for a new row in the grid.** A draft row gets the blue backdrop and nothing else (`src/ui/data_grid/delegate.rs:273-274`); the staged-cell underline is explicitly skipped for drafts (`:384-388`), so a typed value in a new row carries no mark beyond the tint (the word "default" at `:395-399` marks only *untouched* cells). Deleted rows correctly also get `.line_through()`, and staged cells `.underline()`.
- **[L] New/changed rows in the structure tab are tinted only** (`src/ui/schema_view/mod.rs:1064-1074`, `:1269-1277`, `:1689-1695`), whereas dropped rows also get `line_through()` (`:1094`, `:1363`, `:1785`).
- **[L] Some actions with keybindings do not show them.** The "View value" row-menu item lacks `.action(...)` while "Copy value" has it (`src/ui/data_grid/delegate.rs:532-545`); the row panel's Set NULL / Set Default / Revert change omit `SetNull` (`src/ui/table_view/row_panel.rs:288-327`); the value dialog's Save/Close use plain `.tooltip(...)` although `SaveValue` is bound (`src/ui/value_dialog.rs:196`, `:205`); the import dialog's Close buttons do not surface `escape` (`src/ui/import_dialog.rs:368-372`, `:478-482`).
- **[L] The filter bar's `Select`s and value `Input` fall back to their placeholders (`"column"`, `"is"`, `"value"`) as accessible names** (`src/ui/filter_bar.rs:368-393`), although `Select::accessibility_label` exists; the table view's limit `NumberInput` is labelled only by an adjacent `div` (`src/ui/table_view/mod.rs:1440-1449`). ⚠︎ Not verified with a screen reader.
- **[L] The sidebar's `Search schema…` / `Import SQL dump…` buttons set a visible `label` and a differently-worded `accessibility_label`** (`src/ui/session/sidebar.rs:301-330`).

---

## 3. Documentation drift

The docs are unusually good, which makes the drift stand out.

- **[M] `README.md`'s roadmap and status lag the code.** "[ ] Schema inspector & visual DDL builder" is unchecked (`README.md:234`) although `ui::schema_view` implements a structure tab with DDL generation; the status paragraph (`README.md:7`) omits the structure tab and the plan viewer.
- **[L] `README.md`'s project layout is missing files that exist**: `db/plan.rs`, `db/schema.rs`, `db/import/`, `ui/plan_view.rs`, `ui/import_dialog.rs`, `ui/schema_view/`, `ui/shortcuts_dialog.rs`.
- **[L] The settings list in `README.md:230`** omits "Typing NULL means SQL NULL", which the window ships (`src/ui/settings_window.rs:211-223`).
- **[L] `AGENTS.md` and `CLAUDE.md` are two byte-identical 28 341-byte files** (`diff` clean, neither a symlink) — a divergence risk with no automation keeping them in step.
- **[L] `AGENTS.md` says the settings window has "a gear in the title bar beside the shortcut"**, but the settings window opens a plain title bar with no gear (`src/ui/settings_window.rs:57-81`); the gear is the main window's toolbar button (`src/app.rs:827-835`).
- **[L] Doc comments that contradict their code.** `Catalog::truncated` documents "the number hidden by the cap" but returns the pre-cap total (`src/db/catalog.rs:224-227`; the call site uses it as a total, `src/ui/schema_search.rs:333-338`). `Catalog`'s `Needle::new` fallback says "match nothing keeps the function total" but returns `Any`, which matches everything (`src/db/catalog.rs:416-420`; the branch is unreachable). `plan::sqlite`'s doc says `(id, parent, detail)` but the code requires four columns (`src/db/plan.rs:347-348` vs `:356`). The export module says every format writes `NULL` for a stand-in, but TSV, CSV and Markdown write an empty field (`src/db/export.rs:8-10` vs `:98-104`, `:135-141`, `:176-182`). `plan::mysql`'s comment says the tree is "a truer answer than the flag the caller passed", then ORs the flag in anyway (`src/db/plan.rs:505-507`). `query_editor.rs:3-5` says "cancellation come[s] later" though it is bound and shipped (`src/keymap.rs:93`, `README.md:226`). The structure-test module's header says "read-only indexes and foreign keys" while the same file tests adding and dropping both (`src/ui/tests/schema.rs:1` vs `:348-495`).
- **[L] `CatalogKind::group()` and `schema_search::HEADINGS` must stay in sync by hand** (`src/db/catalog.rs:95-103` vs `src/ui/schema_search.rs:29-35`); if a label changes, results for that kind are silently dropped by the `!= heading` filter at `:158`.
- **[L] The hit-cap notice can be wrong.** `schema_search` prints "showing the first 500" whenever `matches == MAX_HITS` (`src/ui/schema_search.rs:330-332`), so a database with exactly 500 matches claims it hid some when it did not.
- **[L] Read-only refusal wording is inconsistent** — "this connection is read-only, so a dump cannot be imported" (`src/db/connection.rs:502`), "…so the {word} statement was not run" (`:570`), bare "this connection is read-only" (`:581`, `:599`); some messages end with a period, others do not (`:391` vs `:606`).

---

## 4. Code warnings and risks

### Blocking I/O on the UI thread

The keychain read, the `connections.json`/settings/dump-pre-flight writes, and the restored-tab reads have all moved to GPUI's background executor. What is left:

- **The startup loads are synchronous.** `Welcome::new`, `Workspace`'s restore, and `settings::init` read their files inline (`src/db/store.rs:48-58`, `src/settings.rs:343-352`). The launcher and the restored tabs are built from those reads, so this is inherent to the startup path rather than a stray call — noted so the difference from the backgrounded writes is deliberate.

### Durability and races

- **`record_connected` is a read-modify-write with no locking** (`src/db/store.rs:75-82`), so it can overwrite a concurrent save from the connection editor and lose a newly added connection. (It now runs in the background, but the race remains.)
- **A checkpoint can interleave with the quit-time flush.** Both write the same `workspace.json.tmp` then rename (`src/app.rs:484-489`, `:504-506`, `src/workspace_state.rs:107-126`); a checkpoint spawned just before quit can write the older snapshot between the flush's write and its rename. ⚠︎ Inferred; the save is a no-op under `#[cfg(test)]`.
- **`save_state`/`flush_state` record the snapshot as saved before it is written** (`src/app.rs:477-490`, `:497-507`), so a failed write is never retried. Failure is only an `eprintln!`.
- **Plaintext query buffers are written with default permissions.** `workspace_state::save` uses `fs::write` with no mode (`src/workspace_state.rs:113-122`); the buffers are the user's own SQL, which `README.md:93-95` acknowledges may hold pasted literals. Nothing in the crate calls `set_permissions`/`PermissionsExt`.
- **Ignored `Result`s** (all deliberate-looking, listed for completeness): `let _ = tx.send(...)` (`src/db/runtime.rs:77`, `:99`), import `ROLLBACK`/`SET FOREIGN_KEY_CHECKS=1`/`SET UNIQUE_CHECKS=1` `.ok()` (`src/db/import/mod.rs:333`, `:354-355`), `sender.send(...)` (`:508`), SQLite `rollback()`/`PRAGMA foreign_keys = ON` `.ok()` (`src/db/sqlite.rs:199`, `:212-217`), and `update_in(...).ok()` throughout the UI (`src/app.rs:552`, `src/ui/session/mod.rs:795`, `:800`, `:833`, `:952`, `:1004`, …), which hides "the entity is gone" errors.
- **Errors that reach only stderr.** A failed settings write (`src/settings.rs:175-177`), a failed `record_connected` (`src/ui/welcome/mod.rs`), and a failed workspace save (`src/app.rs:486`, `:505`, `:149`) all leave the UI claiming success.

### Panics in non-test code

Small in number and mostly guarded, but they are panics in paths that process user input:

- `src/main.rs:101` — `.expect("failed to open window")`.
- `src/db/runtime.rs:17-24` — `Builder::…build().expect("failed to start the database runtime")` inside a `OnceLock` initialiser, i.e. on first database use rather than at a point where the user could be told.
- `src/ui/shortcuts_dialog.rs:163-164` — `panic!` on an unparsable keystroke in the constant table (self-checked by the `every_listed_keystroke_parses` test, so a build-time-style guard).
- `src/ui/filter_bar.rs:218` — `.expect("every operator is in ALL")` (the only `expect` outside tests in the grid/table-view area).
- `src/db/statement.rs:257-259`, `:352-353` — `dollar_tag(...).expect(...)` / `word_at(...).expect(...)`, guarded by the arms above but on the editor's per-keystroke path.
- `src/ui/schema_view/sql.rs:202-205`, `src/ui/schema_view/rebuild.rs:145-148`, `:463-466` — infallible-guard expects.
- `src/ui/schema_view/sql.rs:557` — `unreachable!()` in `generate_foreign_key_statements`, guarded ~55 lines earlier by the `Sqlite && any(wants_a_statement)` early return at `:502-507`.

### Injection-adjacent and safety waivers

- `AssertSqlSafe` waives sqlx's guard on both the user's buffer and generated SQL (`src/db/connection.rs:639-641`, `:721-723`). The generated paths rely entirely on `quote_identifier`/`quote_literal`.
- DDL interpolates free-text: `column_clause` pastes `edit.default` (documented as "Raw expression text, bound for `DEFAULT` verbatim", `src/db/schema.rs:22`) and `edit.type_name` straight into the statement (`src/ui/schema_view/sql.rs:131-146`), and Postgres reuses the type in `USING {current}::{type}` (`:229-234`). A default like `0; DROP …` is not quoted or rejected client-side; the statements run through prepared `execute`, so this is injection-adjacent rather than directly exploitable.
- **[L] `rebuild.rs` rewrites identifiers textually**, so renaming `items.name` also rewrites a trigger's reference to a *different* table's `name` (`src/ui/schema_view/rebuild.rs:580-609`, `:544-550`). Inherent to text-based replay, only partly guarded.

### Memory and hot paths

- **Every result is materialised before it is rendered.** `fetch_all` collects the whole row stream into `Vec<Row>` and converts each cell to an owned `String` (`src/db/connection.rs:654-679`). The grid virtualizes only the *rendering*, so memory is proportional to the result — which contradicts `README.md:5`/`:13` ("millions of rows with minimal memory usage").
- **A copy can double the result before capping it.** `snapshot(Scope::All)` clones every row × column (`src/ui/data_grid/mod.rs:1210-1276`), and `copy_as(Rows(…))` requests `All` when nothing is picked (`:837-843`, `:928-934`), so `Cmd+Shift+C` or `Cmd+A`→`Cmd+C` on a large result peaks at twice the result size before `truncate(MAX_COPY_ROWS)` runs.
- **Per-frame allocation in the row panel.** `render_field` calls `is_field_editable` (`src/ui/table_view/row_panel.rs:271`) and `needs_a_window` (`:282`) for every visible field, and both end in `query::is_binary_type`, which allocates an uppercased `String` per call (`src/db/query.rs:91-100`).
- **`is_placeholder` misclassifies genuine short bracketed values.** `src/db/query.rs:73-88` treats any `<…>` whose inner text has no brackets or space as a driver stand-in, so a real `<nil>`, `<item>` or `<3 bytes>` text value is treated as "never read back": uneditable (`src/ui/data_grid/delegate.rs:939-965`), claims so in the value dialog (`src/ui/data_grid/mod.rs:1042-1048`), dropped from copies (`:860-862`) and exported as `NULL`/empty (`src/db/export.rs:98-104`). The test only covers `<a>hi</a>` and `<>` (`src/db/query.rs:113-115`).
- **Cancellation arms nothing can reach, and work that outlives its tab.** The connect task's abort handle is never kept (`src/ui/welcome/mod.rs`), same for `reload_metadata` (`src/ui/session/mod.rs:1102-1105`) and `switch_database` (`:1210-1218`), so their "cancelled" branches can only be reached by a panic, which would be misreported. Table-view loads, writes and exports are `runtime::spawn`ed and detached with no abort handle (`src/ui/table_view/mod.rs:333-352`, `:360-373`, `:431-476`, `:786-798`, `:835-886`), so nothing stops a page load or an export, and closing the tab leaves the future running. `CancelQuery`/`Cmd+.` only reaches queries and imports (`src/ui/session/mod.rs:1512-1521`).
- **Unchecked indexing / arithmetic** (all currently safe, listed for completeness): `self.tabs[self.active]` (`src/app.rs:852`, safe because `tabs` is never empty), `self.panels[index]` with a modulo (`src/ui/session/mod.rs:1027-1035`), and `page * limit` / `limit`/`offset` interpolation (`src/ui/table_view/sql.rs:90-94`, `src/ui/table_view/mod.rs:1225-1229`), clamped by `settings::MAX_PAGE_SIZE`.
- **`describe` failures are swallowed.** `Executor::describe(pool, statement)` is called with no bound parameters and `unwrap_or_default()`ed (`src/db/connection.rs:668-674`), so a parameterised statement that returns no rows can silently lose its column list. ⚠︎ Unverified against a live Postgres; every in-tree caller casts its placeholders.

---

## 5. Test-suite gaps

Coverage is concentrated in SQLite-backed and pure-logic paths. Gaps worth naming:

- **The right-click menu is untested by design** — the test harness reports the `PopupMenu` as a leaked entity (`src/ui/tests/mod.rs:779-785`). That leaves `menu_row`/`menu_cell` staleness, the export gate, and `stop_right_click` unverified.
- **In `#[cfg(test)]` the keychain is a no-op** (`src/db/store.rs:99-135`), so the real credential-store contract is never exercised and each call site has two code paths to keep right.
- **Column dragging, the row-limit entry/stepper, and a grid keybinding inside a query tab** have no tests (`src/ui/table_view/mod.rs:1131-1152`, `:246-252`; `src/ui/tests/rows.rs:593` calls `copy_as` directly).
- **MySQL is only tested for metadata and imports through paths that cannot reach most of them** — every pure test in `src/db/tests.rs` opens a SQLite `TempDatabase`, and the MySQL rollback→stop fallback and session-variable restore (`src/db/import/mod.rs:242-265`, `:353-356`) are MySQL-only.
- **Only three tests run against a live server, all ignored by default.** `db::tests::live_mysql_routines_carry_their_argument_types`, `live_mysql_table_schema_carries_auto_increment_collation_comment_and_on_update`, and `live_postgres_money_scales_by_the_servers_locale_not_always_by_100` (each doc comment names the container); the other engine-divergence findings (timestamp labelling, unsupported-type stand-ins) are still unverified end to end.

## 6. CI

`.github/workflows/ci.yml` runs `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` in one `ubuntu-latest` job. There is still no macOS or Windows build — on a project whose stated premise is cross-platform and whose build has a documented macOS-only Metal prerequisite. `script/linux` installs the Linux build dependencies; nothing equivalent guards the other two platforms.

## 7. Suggested triage order

1. **Postgres `cell` arms** (§1) — geometric/hstore/range/`MACADDR8` values currently stand in as unreadable and uneditable. (`money`'s hardcoded scale, the same finding's other half, is fixed — verified against a live server with `lc_monetary` set to a zero-fraction-digit locale.)
2. **Copy-path consistency** (§2) — stand-ins and the uneven caps; make every copy path agree on what a stand-in is and where the cap applies.
3. **Export streaming** (§1) and the **catalog cap** (§1) — the two largest unbounded allocations.
4. **Doc alignment** (§3) — the README roadmap/status and the `AGENTS.md`/`CLAUDE.md` duplication.
5. **Accessibility gaps** (§2) — the missing keybindings on tooltips, the colour-only new-row cue, and the placeholder accessible names.

## 8. What this audit did and did not cover

- **Read in full:** every file under `src/` (78 `.rs` files) plus `README.md`, `TODO.md`, `AGENTS.md`, `CLAUDE.md`, `IDEAS.md`, `IDEAS2.md`, `Cargo.toml`, `about.toml`, `.editorconfig`, `.github/workflows/ci.yml`, and the vendored `gpui-component`/`gpui-pre` sources where a finding depended on their defaults.
- **Not run for the audit:** the app itself and no live server; every UI symptom and server-behaviour claim above is marked ⚠︎ inferred or "unverified" rather than asserted. MySQL and Postgres containers were later used for the three ignored live-server tests (see §5).
- **Not audited:** packaging (`cargo packager`) and the icon assets; the bundled theme files; licensing (`about.toml` lists `Apache-2.0`/`MIT` as accepted, and `LICENSE` was not cross-checked against them).
