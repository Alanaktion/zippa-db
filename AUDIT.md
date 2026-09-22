# Audit — Zippa DB

**Revision audited:** `60e7002` "Add schema search dialog with full catalog" (2026-09-19), branch `main`, working tree clean.
**Audit date:** 2026-09-21.

## How this audit was done

| Check | Command | Result |
| --- | --- | --- |
| Compiler/lint warnings | `cargo clippy --all-targets` | **Clean** — zero warnings from this crate. The only notice is a transitive future-incompatibility: `block v0.1.6`. |
| Build warnings | `cargo test` | No warnings from the crate. |
| Tests | `cargo test` | **396 passed, 0 failed** (one test binary, no doc-tests, no integration tests). |

Everything below comes from reading the source, cross-checked against `gpui-kit`/`gpui-component` 0.6.4 and `sqlx` 0.9 sources where behaviour depended on them. Each finding carries a `file:line` anchor.

**Fixed since this audit:** §1 *Saving an edited connection silently deletes its stored password*, §1 *The value dialog writes the four characters `NULL` into a NULL cell*, §1 *"Disconnect" discards unsaved query buffers without asking*, §1 *A table tab's staged edits are never treated as unsaved work*, §2 *SQLite's `EXPLAIN ANALYZE` silently degrades to a plain plan*, and §3 *MySQL metadata SQL uses the engine-blind literal quoting* (the last three areas fixed in a later pass). Each carries a regression test; the anchors below describe the code as audited.

- The connection editor now tracks whether the password box was typed in (`password_edited`), and reports `Option<String>` on `EditorEvent`: `None` leaves the keychain alone, `Some("")` deletes. `Welcome::save` only writes when it is given one, and the box gains a placeholder on a saved connection saying blank keeps the stored password. Tests: `ui::tests::welcome::editing_a_connection_leaves_its_stored_password_alone`.
- The value dialog's save goes through the same `NULL` coercion as the cell editor (`staged_value`, shared by both) and stages nothing when the box was not changed from what it opened with, which is what made an untouched `NULL` become the word. Tests: `ui::tests::value_dialog::saving_an_untouched_null_leaves_it_null` and `the_dialog_coerces_a_typed_null_like_the_cell_editor`.
- The quick switcher's action items are handled by the palette itself (`QuickSwitcherView::choose`), driving the session's existing entry points rather than dispatching an action the dialog's focus path cannot reach; the items no longer carry actions that never fire. Writing the end-to-end confirm test also turned up a bug in that fix — closing the dialog dropped the view before its weak handle was upgraded, so the choice was silently discarded (`src/ui/quick_switcher.rs`). Tests: `ui::tests::quick_switcher::{the_new_query_tab_item_opens_a_tab, the_explain_item_reads_the_active_statement, the_schema_search_item_opens_the_dialog, confirming_an_item_from_the_dialog_drives_the_session}`.
- The menu bar was rebuilt to carry every bound action (`src/menu.rs`), which also closes the `Cmd+K` / `Cmd+\` gaps noted under §4 for the menu, and `src/menu.rs` now has its own tests for coverage, separator placement, and that each item answers to a keystroke. `README.md`'s layout notes the file.
- Disconnect now runs the same `has_unsaved_changes` guard the tab-close path already had (`src/app.rs`), asking before it replaces the session's tab with a fresh connection manager. The teardown is split into `disconnect` (the work) and `confirm_disconnect` (the question), mirroring `close_tab_now`/`confirm_close_tab`. Test: `ui::tests::workspace::disconnecting_with_unsaved_changes_asks_first`.
- `SessionPanel::is_dirty` counts a table tab's staged edits instead of returning `false` (`src/ui/session/panel.rs`), through a new `TableView::has_staged_edits` and `DataGrid::has_unsaved_edits` (staged cells, hand-built rows, deletions, and a cell editor open on a changed value), so closing the tab or the connection asks first. Test: `ui::tests::session::staging_an_edit_marks_a_table_tab_dirty`.
- SQLite's `EXPLAIN ANALYZE` is refused with a message naming the missing form (`src/db/connection.rs`) rather than answered with the plain plan, and the analyze button is disabled with a tooltip that says so (`src/ui/query_editor.rs`, now engine-aware). The `Cmd+Shift+E` backstop still reaches the refusal, and `README.md`'s shortcut row notes the limitation. Tests: `db::tests::explain_refuses_to_analyze_a_read_on_sqlite` and `ui::tests::explain::{the_analyze_button_is_off_on_sqlite, analyze_asks_before_running_on_a_careful_connection}`.
- MySQL's four `information_schema` builders quote a table name with `quote_literal_for(Engine::MySql, …)` (`src/db/mysql.rs`), so a name holding a backslash is escaped the way the server reads it instead of naming a different table. Test: `db::tests::mysql_metadata_escapes_a_backslash_the_way_the_server_reads_it`.

**Legend**

- **[H]** high — data loss, silently wrong data, or a feature that does not do what the UI says.
- **[M]** medium — real gap or divergence, with a workaround or a narrow trigger.
- **[L]** low — polish, naming, docs, dead code.
- ⚠︎ **inferred** — the symptom follows from the code but was not observed at runtime (no test covers it, and the app was not launched for this audit).

There are no "unfinished feature" placeholders in the usual sense: no `todo!()`/`unimplemented!()`, and only one literal `TODO` (`src/ui/table_view/row_panel.rs:336`). The incompleteness here is of the quieter kind — a control that never fires, a doc that promises more than the code does, and one engine doing less than its sibling without saying so.

---

## 1. Data-loss and wrong-data risks

### [H] Saving an edited connection silently deletes its stored password — fixed

`ConnectionEditor::load` blanks the password field on every load (`src/ui/welcome/editor.rs:125`), and `Welcome::save` passes whatever the field holds to the keychain (`src/ui/welcome/mod.rs:200-202`). `store::set_password` treats an empty string as "delete the credential" (`src/db/store.rs:109-111`). So: open a saved connection in the editor, change its name or tag, press **Save** — and its password is gone. Nothing warns, and the next connect fails or prompts. No test exercises the editor's Save path (`src/ui/tests/welcome.rs:47-49` calls `save_for_test` with `""`, and the keychain is a no-op under `#[cfg(test)]`).

### [H] The value dialog writes the four characters `NULL` into a NULL cell — fixed

`DataGrid::view_cell` seeds the dialog with `format_value(...)` (`src/ui/data_grid/mod.rs:1062-1070`), which returns the literal `"NULL"` for an empty cell (`src/ui/data_grid/format.rs:16-18`). Saving stages that text verbatim (`src/ui/value_dialog.rs:100-108` → `src/ui/data_grid/mod.rs:1082-1097`). The `coerce_null_literal` setting is applied only by the grid's own cell editor (`src/ui/data_grid/mod.rs:608-618`) and the row panel (`src/ui/table_view/row_panel.rs:223-230`) — not by `stage_cell`. Opening the value dialog on a NULL cell and pressing `Cmd+Enter` therefore converts NULL into the string `"NULL"`. The same seeding means JSON is staged pretty-printed rather than as stored. The dialog's tests only cover the *edited* case (`src/ui/tests/value_dialog.rs`).

### [H] The quick switcher's five action items were dead ⚠︎ inferred — fixed

`quick_switcher.rs` builds "New Query Tab", "Open SQL File…", "Refresh Schema & Tables", "Explain Query" and "Explain Query & Analyze" as `CommandItem`s carrying `.action(NewTab/OpenFile/Refresh/Explain/ExplainAnalyze)` (`src/ui/quick_switcher.rs:163-217`), but `on_confirm` answers all five with an explicit do-nothing arm (`src/ui/quick_switcher.rs:281-289`). The claim that the dispatched action covers them does not hold:

- `CommandState::confirm` calls `window.dispatch_action` (`gpui-component-0.6.4/src/command/state.rs:597`).
- That dispatches along the **focused node's ancestor path only** (`gpui-pre-0.3.5/src/window.rs:2418-2430`, then `dispatch_action_on_node_inner` at `:6167-6204`).
- The dialog layer is a child of the `Workspace` element (`src/app.rs:850`, `:882`), so the dialog's ancestors are `Workspace` — which registers only connection/settings/switcher actions (`src/app.rs:861-867`). `NewTab`/`OpenFile`/`Refresh` are registered on the `Session` and editor elements (`src/ui/session/mod.rs:1639-1650`), a sibling branch that the dispatch path never reaches.

The items that do work (`Tab`, `Object`, `SwitchDatabase`, and a hand-written `SearchSchema` special case at `:264-270`) are all handled manually in `on_confirm`, which is the tell. No test drives a confirm through the view; the switcher tests call session methods directly (`src/ui/tests/quick_switcher.rs`). Worth confirming by hand before fixing.

### [H] "Disconnect" discards unsaved query buffers without asking — fixed

The sidebar's Disconnect button emits `SessionEvent::Disconnected` (`src/ui/session/sidebar.rs:333-342`), and the workspace answers by draining the pool and replacing the tab with a fresh connection manager (`src/app.rs:424-438`). The `Session` entity — and every query buffer in it — is dropped. Contrast `Workspace::close_tab`, which does check `has_unsaved_changes` (`src/app.rs:275-288`). The test for this path types nothing first (`src/ui/tests/workspace.rs:192-211`).

### [H] A table tab's staged edits are never treated as unsaved work — fixed

`SessionPanel::is_dirty` returns `false` for `TabContent::Table`, documented as "a table tab has no buffer, so it is never dirty" (`src/ui/session/panel.rs:246-254`) — yet `TableView` stages `UPDATE`s, `INSERT`s and `DELETE`s (`src/ui/table_view/mod.rs:483-493`, `:598-607`, `:1232`). The structure tab *is* checked on the line below (`:252`), so the two staged-edit views disagree. Closing a table tab (✕, middle-click, `Close Tab`, or closing the connection, `src/app.rs:280-287`) throws the staged work away with no prompt. The comment on `is_dirty` contradicts the view it describes.

---

## 2. Incomplete features and stubs

### [H] SQLite's `EXPLAIN ANALYZE` silently degrades to a plain plan — fixed

The SQLite arm of `Connection::explain` never reads its `analyze` parameter (`src/db/connection.rs:455-460`) and `plan::sqlite` hardcodes `analyzed: false` (`src/db/plan.rs:411`). The button is enabled for reads (`src/ui/query_editor.rs:302`) and its tooltip promises "actual times" (`src/ui/query_editor.rs:293-301`, repeated at `README.md:174`). The user gets the same output as `Cmd+E` with no message. MySQL has the analogous case but at least falls back visibly to a classic table and a status line.

### [M] MySQL routines carry no argument list, so overloads are indistinguishable — fixed

Fixed: `mysql::ROUTINES_SQL` now reads `information_schema.parameters` (counting from ordinal position 1, so a function's return-value row is left out), and `Connection::stored_objects` fills `arguments` for MySQL as it already did for Postgres. `StoredObject::label` and the catalog's `detail` therefore carry the signature. Verified against a live MySQL 8.4 server by `db::tests::live_mysql_routines_carry_their_argument_types` (ignored by default; see its doc comment for the container and how to run it). As audited: `mysql.rs` selected a literal `NULL` for the arguments column (`src/db/mysql.rs:20-21`) and `Connection::stored_objects` only filled `arguments` for Postgres (`src/db/connection.rs:241-247`), so `StoredObject::label` and the catalog's `detail` dropped the signature (`src/db/connection.rs:77-97`, `src/db/catalog.rs:139-149`). `information_schema.parameters` is available and unused.

### [M] `run_script` has no transaction — documented, but a real correctness gap

`Connection::run_script` runs statements one at a time with no transaction (`src/db/connection.rs:469-485`); its doc says so ("there is no transaction around this yet"). A script that fails at statement *n* leaves 1..*n*−1 applied. The import path already has rollback machinery (`src/db/import/mod.rs:48-51`), so the capability exists elsewhere.

### [M] The catalog cap is applied only after everything is built

`Connection::catalog` materialises every column/index/trigger row and every entry, then calls `entries.truncate(MAX_ENTRIES)` (`src/db/connection.rs:285-307`). `MAX_ENTRIES` is 200 000 (`src/db/catalog.rs:21`) and the doc says "a database past this is truncated rather than held whole" (`src/db/catalog.rs:19-20`) — which is exactly what the code does not do. The three per-kind queries are also fully materialised `QueryResult`s first.

### [M] An `IN` list built from an all-NULL column is invalid SQL — fixed

`export::render` drops NULLs from the `in_list` shape and then joins whatever is left, so a column of only NULLs emits `()` (`src/db/export.rs:350-381`). The test at `src/db/export.rs:749-750` pins that output rather than guarding it.

### [M] Postgres `cell` has no arm for several types sqlx can decode

`postgres.rs` handles `MACADDR` but not `MACADDR8`, and none of sqlx 0.9's geometric/hstore/lrange types (`PgPoint`, `PgBox`, `PgCircle`, `PgHstore`, ranges, …). Those fall to the `_ => value!(String)` arm, fail, and render as `<POINT>`-style stand-ins (`src/db/postgres.rs:250-287`) — which export as NULL and cannot be edited. `money` is decoded with a hardcoded scale of 100 (`src/db/postgres.rs:213-215`, `:384-389`), even though the doc admits the scale comes from `lc_monetary`.

### [M] MySQL `EXPLAIN` tree parsing can read actual rows as the estimate — fixed

`plan::mysql` sets `cost_text` to everything from `(cost=` to end of line and then searches *that* for `rows=` (`src/db/plan.rs:556-563`); when the cost group omits `rows=`, the `(actual …)` group's value is reported as `estimated_rows`.

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
- A hard import error does not emit `ImportEvent::Finished`, unlike a cancellation, so a run that failed mid-way may already have applied statements without the session being told the schema is stale (`src/ui/import_dialog.rs:176-188`). — fixed: the event is now emitted for every outcome.
- Column reordering is enabled in the UI but never honoured by the delegate: `DataTable` is built without disabling it (`src/ui/data_grid/mod.rs:1763-1768`), `TableState::col_movable` defaults to `true` (`gpui-component-0.6.4/src/table/state.rs:233`, `:293`), and `ResultDelegate` never implements `TableDelegate::move_column` — whose default is an empty body (`gpui-component-0.6.4/src/table/delegate.rs:123-131`) while `TableState::move_column` still reorders its own `col_groups` (`state.rs:1194-1211`). ⚠︎ inferred; no test drags a header.

---

## 3. Inconsistencies

### Engine divergence

- **[M] MySQL metadata SQL uses the engine-blind literal quoting — fixed.** `mysql.rs` builds every `information_schema` query with `quote_literal` (`src/db/mysql.rs:100`, `:112`, `:127`, `:149`), which does not double backslashes (`src/db/sql.rs:36-38`) — so a MySQL table name containing `\` names a different table. The engine-aware `quote_literal_for` exists precisely for this and is used only by export (`src/db/sql.rs:46-52`). `quote_literal`'s own doc also claims it is only for queries that cannot take a bind ("a `PRAGMA` table function, for one", `src/db/sql.rs:31-35`), which the `information_schema` comparisons are not.
- **[M] The same schema edit is safe on Postgres and lossy on MySQL.** MySQL restates the whole column with only name/type/nullability/default (`src/ui/schema_view/sql.rs:259-275`), because `ColumnDef` models nothing else (`src/db/schema.rs:17-29`) and `mysql::columns_sql` reads only those four fields (`src/db/mysql.rs:105-113`). A nullability or type edit therefore drops `AUTO_INCREMENT`, `CHARACTER SET`/`COLLATE`, `COMMENT`, `ON UPDATE CURRENT_TIMESTAMP` and generated expressions. Postgres emits per-property `ALTER COLUMN` statements and preserves them (`src/ui/schema_view/sql.rs:214-257`). SQLite refuses what it cannot model (`src/ui/schema_view/rebuild.rs:383-433`). Three defensible choices — but the difference is neither documented at the point of edit nor refused.
- **[M] Schema changes are atomic on SQLite only.** `Change::Statements` runs one statement at a time with no transaction and the doc accepts a half-applied result (`src/ui/schema_view/mod.rs:931-943`); SQLite's `Change::Rebuild` is one atomic procedure with a foreign-key check (`src/ui/schema_view/rebuild.rs:46-49`). Postgres DDL is transactional, so a rename+retype can be left half-applied there when it need not be.
- **[M] Timestamps are labelled differently per engine.** Postgres renders `timestamptz` in the local zone with an explicit offset (`src/db/postgres.rs:266-275`); MySQL renders `TIMESTAMP` as a bare UTC wall clock (`src/db/mysql.rs:188-196`), which happens to be right only because sqlx defaults the MySQL session to `time_zone='+00:00'`. A `TIMESTAMP` and a `DATETIME` therefore look identical while meaning different things.
- **[L] The `INSERT` export is not schema-qualified** while every other generated statement is: `export_result` passes `self.object.name` (`src/ui/table_view/mod.rs:779`, `:812`) and `render_sql` quotes it bare (`src/db/export.rs:399`), whereas `TableView::target()` qualifies with the schema (`src/ui/table_view/sql.rs:29-38`). Exporting `myschema.items` writes `insert into items …`.
- **[L] The editor splitter and the dump splitter are different tools.** `db/statement.rs:37-55` has no trigger-body, `DELIMITER`, `\.`-meta-line or `COPY` handling, all of which `db/import/splitter.rs` has (`:60-80`, `:114-125`, `:191-213`). `Cmd+Shift+Enter` on a `CREATE TRIGGER … BEGIN … END;` or a `COPY … FROM stdin` header splits in the wrong place. `import/splitter.rs:1-14` notes the editor's version covers "the common cases", but nothing at the editor says so.
- **[L] The read-only classifier can refuse plain reads.** `statement.rs`'s allowlist omits `WITH`, so a CTE read counts as a write (`src/db/statement.rs:15-21`), and any `ANALYZE` anywhere in the words is a write (`:185-190`) — yet `Connection::explain` deliberately skips `refuse_write` and runs its `EXPLAIN` (`src/db/connection.rs:386-404`). The same text is allowed through one entry point and refused through the other (pinned by `src/db/tests.rs:836-855`). `EXPLAIN_OPTIONS` is also a fixed list, so an unknown option word silently becomes part of the wrapped statement (`src/db/statement.rs:79-97`, `:136-145`).

### Behaviour that differs between two paths to the same thing

- **[M] Editing a connection wipes its "last connected" stamp and reorders the launcher.** — fixed: `Welcome::save` carries the stored `last_connected` onto the edited config, so the card keeps its place in most-recent-first order. Test: `ui::tests::welcome::editing_a_connection_keeps_its_last_connected_stamp`. `ConnectionEditor::config` hardcodes `last_connected: None` (`src/ui/welcome/editor.rs:157`) and `Welcome::save` replaces the stored config wholesale (`src/ui/welcome/mod.rs:196`), while `mark_connected` is careful to preserve it (`src/ui/welcome/mod.rs:293-296`). The card drops out of most-recent-first order into the name-sorted tail and loses its "Connected … ago" line.
- **[M] Connecting from the editor ignores the stored password.** — fixed: an untouched password box on a saved, non-file connection now falls back to the keychain, as a card click does. An empty editor password becomes `None` (`src/ui/welcome/mod.rs:165-168`) and `Connection::open` has no keychain fallback (`src/db/connection.rs:130-136`), so the same connection succeeds from a card click (which reads the keychain, `src/ui/welcome/mod.rs:227`) and fails from the editor's **Connect**. Nothing in the UI explains the difference.
- **[M] The row menu's export only appears for an already-picked row.** — fixed: the export now takes the menu's row into the selection like Delete does, so it is offered and scoped to the row the menu was opened on. The `Copy as`/Export submenu is gated on `copies_rows` (`src/ui/data_grid/delegate.rs:651`) while the Delete item a few lines later takes the menu's row into the selection itself (`mark_for_menu`, `:692-693`, `:767-773`). Right-clicking a row that is not picked offers Delete but not export.
- **[M] Stand-ins are handled three ways across the copy paths.** `Cmd+C` puts the driver's description on the clipboard verbatim (`src/ui/data_grid/mod.rs:793-820`, asserted at `src/ui/tests/navigation.rs:111-116`), whereas `Cmd+Shift+C` and the `Copy as` submenu route through `export`, which writes `NULL` and counts it (`src/ui/data_grid/mod.rs:837-918`, `src/db/export.rs:98-104`).
- **[M] The copy caps are uneven and the doc overstates them.** `MAX_COPY_ROWS`'s comment says a large result "is copied a page at a time" (`src/ui/data_grid/mod.rs:1663-1666`), but truncation happens *after* `snapshot()` has cloned every row × column (`:794`/`:842` then `:884-886` vs `:1210-1276`), and `MAX_COPY_BYTES` is not applied at all to single-cell copies (`:781-791`, `:823-833`) or either column shape (`:845-876`).
- **[L] NULL is handled three ways.** The grid editor and row panel coerce a typed `null` per the setting; the row panel shows a NULL as an empty box (`src/ui/table_view/row_panel.rs:195-201`); the value dialog shows and stages the word `NULL` (see §1).
- **[L] Delete and restore are guarded differently.** `delete_rows` requires `is_editable()` (`src/ui/table_view/mod.rs:898-904`) while `restore_rows` only checks `committing` (`:907-912`).
- **[L] Three different guards for "a dialog is already open".** `window.has_active_dialog` (`src/ui/quick_switcher.rs:50`, `src/ui/schema_search.rs:106`, `src/ui/shortcuts_dialog.rs:119`), a struct field (`src/ui/session/mod.rs:808-810`), and none at all (`src/ui/welcome/mod.rs:106-144`).
- **[L] The same failure is reported two ways at once.** `Welcome::connect` sets the inline banner *and* raises a toast (`src/ui/welcome/mod.rs:276-280` + `:444-460`); `connect_saved` shows only the banner (`:229-233`); `settings::update` shows neither (`src/settings.rs:175-177`).
- **[L] A database switch on a table keeps state that belonged to the old one.** — fixed: `set_connection` now clears `confirming`, `pending`, `notice`, `error` and `reveal` along with the page/sort/row-key it already reset. `TableView::set_connection` resets page/sort/row-key (`src/ui/table_view/mod.rs:311-321`) but leaves `confirming` (whose statements were built against the previous table and are still runnable from the banner, `:923-928`), plus `pending`, `notice`, `error`, `reveal` and `limit`; it also reloads *through* the old filters, which the filter bar discards afterwards and only on success (`src/ui/filter_bar.rs:135-153`).
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

## 4. Documentation drift

The docs are unusually good, which makes the drift stand out.

- **[M] `README.md` omits two bound shortcuts and mis-describes a third.** — fixed: the table now carries `Cmd+K` and `Cmd+\`, and the `Cmd+N` row says it opens the editor dialog on the connection manager. The table (`README.md:169-205`) has no row for `Cmd+K` (quick switcher — mentioned only in prose) or `Cmd+\` (toggle row panel, `src/keymap.rs:142`, present in the in-app list at `src/ui/shortcuts_dialog.rs:110`). `Cmd+N` is described as "New connection (or another connection tab)", but on the launcher it opens the editor *dialog* instead of a tab (`src/keymap.rs:118-121`, `src/ui/welcome/mod.rs:174-181`, test at `src/ui/tests/workspace.rs:39-65`).
- **[M] The in-app shortcut list is a subset of the keymap it claims to mirror.** — fixed: `GROUPS` now carries `ctrl-pagedown`/`ctrl-pageup`, the settings-window close keys, and the connection editor's `Cmd+Enter`; `Cmd+Q`/`Cmd+H` remain in the OS menu only. Its doc says "listing every binding in `keymap.rs` … Keep it in step" (`src/ui/shortcuts_dialog.rs:3-7`), but `GROUPS` omits `ctrl-pagedown`/`ctrl-pageup` (`src/keymap.rs:114-115`), the settings-window close keys (`:213-215`), the connection editor's `escape`/`Cmd+Enter` (`:226-233`), `CloseImport`'s `escape` (`:99`), and `Cmd+Q`/`Cmd+H` (`src/menu.rs:32-35`).
- **[M] `TODO.md:42` marks the sidebar navigation tree as done** — fixed: the box is unchecked and the line says the list is flat today. ("Databases > Schemas > Tables / Views"), but the code is a flat list plus one folder per routine kind (`src/ui/session/sidebar.rs:64-98`), `DatabaseObject.schema` groups nothing, and `AGENTS.md` says the schema tree "goes" in that file — i.e. still ahead.
- **[M] `README.md`'s roadmap and status lag the code.** "[ ] Schema inspector & visual DDL builder" is unchecked (`README.md:234`) although `ui::schema_view` implements a structure tab with DDL generation; the status paragraph (`README.md:7`) omits the structure tab and the plan viewer.
- **[L] `README.md`'s project layout is missing files that exist**: `db/plan.rs`, `db/schema.rs`, `db/import/`, `ui/plan_view.rs`, `ui/import_dialog.rs`, `ui/schema_view/`, `ui/shortcuts_dialog.rs`.
- **[L] The settings list in `README.md:230`** omits "Typing NULL means SQL NULL", which the window ships (`src/ui/settings_window.rs:211-223`).
- **[L] The settings window's declared default for that switch is `false` while the real default is `true`** — fixed: the declared default is now `true`, matching `Settings::default`. (`src/ui/settings_window.rs:213-220` vs `src/settings.rs:117`, and the field doc at `src/settings.rs:95-99` says "On by default"). A reset-to-default driven by that value would silently flip the user's setting.
- **[L] `AGENTS.md` and `CLAUDE.md` are two byte-identical 28 341-byte files** (`diff` clean, neither a symlink) — a divergence risk with no automation keeping them in step.
- **[L] `AGENTS.md` says the settings window has "a gear in the title bar beside the shortcut"**, but the settings window opens a plain title bar with no gear (`src/ui/settings_window.rs:57-81`); the gear is the main window's toolbar button (`src/app.rs:827-835`).
- **[L] Doc comments that contradict their code.** `Catalog::truncated` documents "the number hidden by the cap" but returns the pre-cap total (`src/db/catalog.rs:224-227`; the call site uses it as a total, `src/ui/schema_search.rs:333-338`). `Catalog`'s `Needle::new` fallback says "match nothing keeps the function total" but returns `Any`, which matches everything (`src/db/catalog.rs:416-420`; the branch is unreachable). `plan::sqlite`'s doc says `(id, parent, detail)` but the code requires four columns (`src/db/plan.rs:347-348` vs `:356`). The export module says every format writes `NULL` for a stand-in, but TSV, CSV and Markdown write an empty field (`src/db/export.rs:8-10` vs `:98-104`, `:135-141`, `:176-182`). `plan::mysql`'s comment says the tree is "a truer answer than the flag the caller passed", then ORs the flag in anyway (`src/db/plan.rs:505-507`). `query_editor.rs:3-5` says "cancellation come[s] later" though it is bound and shipped (`src/keymap.rs:93`, `README.md:226`). The structure-test module's header says "read-only indexes and foreign keys" while the same file tests adding and dropping both (`src/ui/tests/schema.rs:1` vs `:348-495`).
- **[L] `CatalogKind::group()` and `schema_search::HEADINGS` must stay in sync by hand** (`src/db/catalog.rs:95-103` vs `src/ui/schema_search.rs:29-35`); if a label changes, results for that kind are silently dropped by the `!= heading` filter at `:158`.
- **[L] The hit-cap notice can be wrong.** `schema_search` prints "showing the first 500" whenever `matches == MAX_HITS` (`src/ui/schema_search.rs:330-332`), so a database with exactly 500 matches claims it hid some when it did not.
- **[L] Read-only refusal wording is inconsistent** — "this connection is read-only, so a dump cannot be imported" (`src/db/connection.rs:502`), "…so the {word} statement was not run" (`:570`), bare "this connection is read-only" (`:581`, `:599`); some messages end with a period, others do not (`:391` vs `:606`).

---

## 5. Code warnings and risks

### Blocking I/O on the UI thread

- **Keychain.** — fixed: `store::password` is now read on GPUI's background executor (`Welcome::connect_using_stored_password`), and the connect starts when the password comes back, so a credential-store prompt no longer blocks the frame. `store::password` is a synchronous `keyring::Entry::get_password` (`src/db/store.rs:90-104`) called inline from `Welcome::connect_saved` (`src/ui/welcome/mod.rs:227`); only the sqlx work is moved off-thread.
- **`connections.json`.** — fixed for writes: `save`, `duplicate`, `delete_confirmed` and `record_connected` now go through `store_in_background`, so the UI thread no longer waits on the file. The startup *load* (in `Welcome::new` and `app.rs`'s restore) stays synchronous, because the launcher and the restored tabs are built from it — the same way `workspace.json` is loaded at startup. Loaded and written synchronously on the UI thread (`src/db/store.rs:48-68`) from `Welcome::new`/`save`/`duplicate`/`delete_confirmed`/`record_connected` (`src/ui/welcome/mod.rs:60`, `:200`, `:334`, `:379-381`, `:297`) and from `app.rs:187` — while `app.rs:484-489` deliberately backgrounds the *same kind* of write for `workspace.json`. The two writers of app state use opposite strategies.
- **Settings.** — fixed: `settings::update` writes the file on the background executor. Every change does a synchronous `fs::write` (`src/settings.rs:168-183`, `:349-356`).
- **Import pre-flight.** — fixed: `ImportView::new` starts the read on the background executor and shows "Checking the dump…" until it lands, disabling Import until then (`preflight` is now `Option<Preflight>`). `import::preflight` opens the file and decompresses up to 64 KiB (`src/db/import/reader.rs:105-125`) inside `ImportView::new` (`src/ui/import_dialog.rs:76`)
- **Restore.** — fixed: `Session::restore` collects the file-backed tabs and reads them on the background executor, folding each baseline in when it lands; a file that cannot be read now sets an error status on the tab instead of being swallowed. Test: `ui::tests::workspace::a_restored_file_tab_reads_its_file_to_decide_dirty`. `Session::restore` reads each restored tab's file with a synchronous `read_to_string` inside a UI event (`src/ui/session/mod.rs:587-590`), and swallows a failure with `.ok()`

### Durability and races

- **`connections.json` is written non-atomically.** — fixed: `store::save` now writes a `.tmp` beside the file and renames it into place, falling back to an in-place write where a rename cannot replace. It uses a plain `fs::write` (`src/db/store.rs:61-68`) whereas `workspace_state::save` writes a `.tmp` file and renames (`src/workspace_state.rs:108-126`). A crash mid-write leaves the file unparseable, and `load` then returns an error that `Welcome::new` shows in place of every connection (`src/ui/welcome/mod.rs:59-63`).
- **`record_connected` is a read-modify-write with no locking** (`src/db/store.rs:75-82`), so it can overwrite a concurrent save from the connection editor and lose a newly added connection.
- **A checkpoint can interleave with the quit-time flush.** Both write the same `workspace.json.tmp` then rename (`src/app.rs:484-489`, `:504-506`, `src/workspace_state.rs:107-126`); a checkpoint spawned just before quit can write the older snapshot between the flush's write and its rename. ⚠︎ Inferred; the save is a no-op under `#[cfg(test)]`.
- **`save_state`/`flush_state` record the snapshot as saved before it is written** (`src/app.rs:477-490`, `:497-507`), so a failed write is never retried. Failure is only an `eprintln!`.
- **Plaintext query buffers are written with default permissions.** `workspace_state::save` uses `fs::write` with no mode (`src/workspace_state.rs:113-122`); the buffers are the user's own SQL, which `README.md:93-95` acknowledges may hold pasted literals. Nothing in the crate calls `set_permissions`/`PermissionsExt`.
- **Ignored `Result`s** (all deliberate-looking, listed for completeness): `let _ = tx.send(...)` (`src/db/runtime.rs:77`, `:99`), import `ROLLBACK`/`SET FOREIGN_KEY_CHECKS=1`/`SET UNIQUE_CHECKS=1` `.ok()` (`src/db/import/mod.rs:333`, `:354-355`), `sender.send(...)` (`:508`), SQLite `rollback()`/`PRAGMA foreign_keys = ON` `.ok()` (`src/db/sqlite.rs:199`, `:212-217`), and `update_in(...).ok()` throughout the UI (`src/app.rs:552`, `src/ui/session/mod.rs:795`, `:800`, `:833`, `:952`, `:1004`, …), which hides "the entity is gone" errors.
- **Errors that reach only stderr.** A failed settings write (`src/settings.rs:175-177`), a failed `record_connected` (`src/ui/welcome/mod.rs:297-299`), and a failed workspace save (`src/app.rs:486`, `:505`, `:149`) all leave the UI claiming success.

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

- `AssertSqlSafe` waives sqlx's guard on both the user's buffer and generated SQL (`src/db/connection.rs:639-641`, `:721-723`). The generated paths rely entirely on `quote_identifier`/`quote_literal`, and the engine-blind `quote_literal` has the MySQL backslash hole described in §3.
- DDL interpolates free-text: `column_clause` pastes `edit.default` (documented as "Raw expression text, bound for `DEFAULT` verbatim", `src/db/schema.rs:22`) and `edit.type_name` straight into the statement (`src/ui/schema_view/sql.rs:131-146`), and Postgres reuses the type in `USING {current}::{type}` (`:229-234`). A default like `0; DROP …` is not quoted or rejected client-side; the statements run through prepared `execute`, so this is injection-adjacent rather than directly exploitable.
- **[M] The dump splitter reads one line without a size bound** (`read_until(b'\n', …)` into an unbounded `Vec`, `src/db/import/splitter.rs:151-158`) before `MAX_STATEMENT` applies to the assembled statement (`:240-246`); compressed input can expand arbitrarily.
- **[L] `rebuild.rs` rewrites identifiers textually**, so renaming `items.name` also rewrites a trigger's reference to a *different* table's `name` (`src/ui/schema_view/rebuild.rs:580-609`, `:544-550`). Inherent to text-based replay, only partly guarded.

### Memory and hot paths

- **Every result is materialised before it is rendered.** `fetch_all` collects the whole row stream into `Vec<Row>` and converts each cell to an owned `String` (`src/db/connection.rs:654-679`). The grid virtualizes only the *rendering*, so memory is proportional to the result — which contradicts `README.md:5`/`:13` ("millions of rows with minimal memory usage").
- **A copy can double the result before capping it.** `snapshot(Scope::All)` clones every row × column (`src/ui/data_grid/mod.rs:1210-1276`), and `copy_as(Rows(…))` requests `All` when nothing is picked (`:837-843`, `:928-934`), so `Cmd+Shift+C` or `Cmd+A`→`Cmd+C` on a large result peaks at twice the result size before `truncate(MAX_COPY_ROWS)` runs.
- **Per-frame allocation in the row panel.** `render_field` calls `is_field_editable` (`src/ui/table_view/row_panel.rs:271`) and `needs_a_window` (`:282`) for every visible field, and both end in `query::is_binary_type`, which allocates an uppercased `String` per call (`src/db/query.rs:91-100`).
- **`is_placeholder` misclassifies genuine short bracketed values.** `src/db/query.rs:73-88` treats any `<…>` whose inner text has no brackets or space as a driver stand-in, so a real `<nil>`, `<item>` or `<3 bytes>` text value is treated as "never read back": uneditable (`src/ui/data_grid/delegate.rs:939-965`), claims so in the value dialog (`src/ui/data_grid/mod.rs:1042-1048`), dropped from copies (`:860-862`) and exported as `NULL`/empty (`src/db/export.rs:98-104`). The test only covers `<a>hi</a>` and `<>` (`src/db/query.rs:113-115`).
- **Cancellation arms nothing can reach, and work that outlives its tab.** The connect task's abort handle is never kept (`src/ui/welcome/mod.rs:281-283`), same for `reload_metadata` (`src/ui/session/mod.rs:1102-1105`) and `switch_database` (`:1210-1218`), so their "cancelled" branches can only be reached by a panic, which would be misreported. Table-view loads, writes and exports are `runtime::spawn`ed and detached with no abort handle (`src/ui/table_view/mod.rs:333-352`, `:360-373`, `:431-476`, `:786-798`, `:835-886`), so nothing stops a page load or an export, and closing the tab leaves the future running. `CancelQuery`/`Cmd+.` only reaches queries and imports (`src/ui/session/mod.rs:1512-1521`).
- **Unchecked indexing / arithmetic** (all currently safe, listed for completeness): `self.tabs[self.active]` (`src/app.rs:852`, safe because `tabs` is never empty), `self.panels[index]` with a modulo (`src/ui/session/mod.rs:1027-1035`), and `page * limit` / `limit`/`offset` interpolation (`src/ui/table_view/sql.rs:90-94`, `src/ui/table_view/mod.rs:1225-1229`), clamped by `settings::MAX_PAGE_SIZE`.
- **`describe` failures are swallowed.** `Executor::describe(pool, statement)` is called with no bound parameters and `unwrap_or_default()`ed (`src/db/connection.rs:668-674`), so a parameterised statement that returns no rows can silently lose its column list. ⚠︎ Unverified against a live Postgres; every in-tree caller casts its placeholders.

---

## 6. Test-suite gaps

396 tests pass, but coverage is concentrated in SQLite-backed and pure-logic paths. Gaps worth naming (each is a place where a bug above would have been caught):

- **The right-click menu is untested by design** — the test harness reports the `PopupMenu` as a leaked entity (`src/ui/tests/mod.rs:779-785`). That leaves `menu_row`/`menu_cell` staleness, the export-on-unpicked-row gate, and `stop_right_click` unverified.
- **The value dialog is never saved without editing** (`src/ui/tests/value_dialog.rs`), which is the NULL-corruption path in §1.
- **The quick switcher's confirm/cancel path is never exercised** (`src/ui/tests/quick_switcher.rs` asserts only that the dialog opens), so the dead-action bug in §1 is invisible.
- **The connection editor's Save path is untested**, and in `#[cfg(test)]` the keychain is a no-op (`src/db/store.rs:99-135`), so the password-deletion bug cannot be caught as written.
- **Disconnect, and closing a table tab, are never tested with unsaved/buffered work** (`src/ui/tests/workspace.rs:192-211`).
- **Column dragging, the row-limit entry/stepper, and a grid keybinding inside a query tab** have no tests (`src/ui/table_view/mod.rs:1131-1152`, `:246-252`; `src/ui/tests/rows.rs:593` calls `copy_as` directly).
- **MySQL is only tested for imports and metadata through code paths that cannot reach them** — every test in `src/db/tests.rs:653-868` opens a SQLite `TempDatabase`, and the MySQL rollback→stop fallback and session-variable restore (`src/db/import/mod.rs:242-265`, `:353-356`) are MySQL-only.
- **No test runs against a live Postgres or MySQL**, so the engine-divergence findings in §2/§3 (timestamp labelling, MySQL column restating, unsupported-type stand-ins, empty `IN` lists) are unverified end to end. Partly addressed: `db::tests::live_mysql_routines_carry_their_argument_types` is an ignored test that runs against a live MySQL server (its doc comment names the container); the other engine-divergence findings are still unverified.

## 7. CI

`.github/workflows/ci.yml` runs a single `cargo test --verbose` job on `ubuntu-latest`. There is no `cargo fmt --check`, no `cargo clippy`, and no macOS or Windows build — on a project whose stated premise is cross-platform and whose build has a documented macOS-only Metal prerequisite. `script/linux` installs the Linux build dependencies; nothing equivalent guards the other two platforms. A later pass added `cargo fmt --all -- --check` and `cargo clippy --all-targets -- -D warnings` steps to the Linux job; the cross-platform build gap remains.

## 8. Suggested triage order

1. **Password deletion on connection save** (§1) — silent credential loss, one-file fix.
2. **Value dialog NULL → `"NULL"`** (§1) — silent data corruption on a common gesture; route it through the same `coerce_null_literal` path the cell editor uses.
3. **Quick switcher actions** (§1) — confirm by hand, then either wire the items through the session (as the `Tab`/`Object` arms are) or drop them.
4. **Disconnect and table-tab close with unsaved work** (§1) — reuse the `has_unsaved_changes` guard the tab-close path already has.
5. **MySQL metadata quoting** (§3) — switch the four `information_schema` builders to `quote_literal_for(Engine::MySql, …)`.
6. **SQLite `EXPLAIN ANALYZE`** (§2) — either implement, or refuse with a message the way the import dialog refuses a read-only connection; fix the tooltip either way.
7. **Doc alignment** (§4) — the README shortcut table, the in-app shortcut list, and `TODO.md`'s sidebar-tree checkbox are all one-line fixes, and `AGENTS.md`/`CLAUDE.md` should be one file plus a link.
8. **Blocking I/O on the UI thread** (§5) — keychain and file reads/writes, following the `background_spawn` pattern already used by `sql_file` and `app.rs`'s save.

## 9. What this audit did and did not cover

- **Read in full:** every file under `src/` (78 `.rs` files) plus `README.md`, `TODO.md`, `AGENTS.md`, `CLAUDE.md`, `IDEAS.md`, `IDEAS2.md`, `Cargo.toml`, `about.toml`, `.editorconfig`, `.github/workflows/ci.yml`, and the vendored `gpui-component`/`gpui-pre` sources where a finding depended on their defaults.
- **Not run:** the app itself, and no live PostgreSQL/MySQL/SQLite server. Every UI symptom and every server-behaviour-dependent claim is marked ⚠︎ inferred or "unverified" above rather than asserted.
- **Not audited:** packaging (`cargo packager`) and the icon assets; the bundled theme files; licensing (`about.toml` lists `Apache-2.0`/`MIT` as accepted, and `LICENSE` was not cross-checked against them).
