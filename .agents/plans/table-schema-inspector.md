# Table Schema Inspector & DDL Generator (TODO.md §4)

## Context

TODO.md's Table Schema Inspector & DDL Generator has four bullets:

```
* [ ] View and modify table structure (Columns, Types, Nullability, Default values)
* [ ] Index manager (Primary Keys, Unique indexes, Composite indexes)
* [ ] Foreign Key relationship builder
* [ ] Live SQL preview generator (shows the `ALTER TABLE` statement before executing changes)
```

Today the sidebar (`src/ui/session/sidebar.rs`) opens exactly one thing per
object: clicking a row calls `Session::open_object`
(`src/ui/session/mod.rs:161-185`), which opens a `TableView` — the *data*
browser, paged/filtered/edited row-by-row through `sql.rs`'s generated
`UPDATE`/`INSERT`/`DELETE`. There is no notion of the table's own definition
anywhere in the app: `Connection` only ever asks the server for its primary
key (`Connection::row_key`, `src/db/connection.rs:216-250`, backed by each
engine's `primary_key_sql`). Column *types* are known only as the driver's
own type names, carried on `QueryResult::column_types` for casting bound
parameters — never surfaced as user-facing "structure".

This plan adds that missing piece: a schema-metadata layer in `db/`, and a
new session tab kind that shows it and, from phase 2 on, edits it by
generating and previewing `ALTER TABLE`/`CREATE INDEX`/etc. before running
it — exactly the "Live SQL preview" bullet, which this plan treats as a
cross-cutting requirement of every edit rather than its own phase.

## Design decisions (flagging for confirmation before phase 2)

**DDL always previews and always asks, regardless of `SafetyMode`.** The
existing `SafetyMode` scale (`src/db/config.rs:49-100`) is about *row* writes:
`Staged`/`AutoApply` let a table's inline edits skip a per-write confirmation
because a wrong cell is a bounded, single-row mistake. A dropped column or a
retyped `NOT NULL` is not bounded the same way — it can lose data outright and
often cannot be undone by re-running the inverse statement (a dropped column's
data is gone; SQLite's rebuild procedure, see Phase 3, briefly holds the whole
table's data in a scratch copy). So this plan has the schema editor show the
generated statement and ask "Apply?" on every save, even under `AutoApply` —
`ReadOnly` still refuses outright via the existing `Connection::refuse_write`
classifier (`ALTER`/`CREATE INDEX`/`DROP INDEX` are not in `statement::READING`,
so they already come back as a write with no classifier changes needed). This
mirrors `ConfirmWrites`' `Confirm` status/footer panel pattern but is not
gated by `SafetyMode` at all. Worth confirming with the user before Phase 2's
first `execute()` call ships, since it's a deliberate deviation from "the
safety mode already says how careful to be."

**Engines diverge sharply on what `ALTER TABLE` can do**, which is why this is
split into more phases than the four TODO bullets:

- **Postgres**: `ALTER TABLE ... ALTER COLUMN TYPE`, `SET/DROP NOT NULL`,
  `SET/DROP DEFAULT`, `RENAME COLUMN`, `ADD COLUMN`, `DROP COLUMN` are all
  independent, running statements. Indexes: `CREATE [UNIQUE] INDEX`,
  `DROP INDEX`; a `PRIMARY KEY` is `ADD/DROP CONSTRAINT`. FKs:
  `ADD/DROP CONSTRAINT ... FOREIGN KEY`.
- **MySQL**: no separate "retype" — `MODIFY COLUMN name type [NULL|NOT NULL]
  [DEFAULT ...]` restates the whole column in one statement, and `CHANGE
  COLUMN old new type ...` is the same for a rename. Indexes are per-table
  namespaced (`DROP INDEX name ON table`, unlike Postgres/SQLite where an
  index name is unique per-schema). Primary key is
  `ADD/DROP PRIMARY KEY` (no name). FKs: `ADD CONSTRAINT ... FOREIGN KEY`,
  `DROP FOREIGN KEY name`.
- **SQLite**: `ALTER TABLE` only supports `ADD COLUMN`, `DROP COLUMN`,
  `RENAME COLUMN`, `RENAME TO` (3.35+, needed anyway since sqlx's bundled
  driver is recent) — retyping a column, changing nullability/default, adding
  a primary key/foreign key to an existing table, or adding anything beyond a
  simple index requires SQLite's documented 12-step rebuild (new table under a
  temp name with the desired schema, `INSERT INTO new SELECT * FROM old`,
  drop old, rename new, recreate indexes/triggers/views,
  `PRAGMA foreign_keys` toggled off/on around it). That procedure is enough
  work and enough risk (it moves every row) that it is its own phase (Phase 3)
  rather than folded into Phase 2 — Phase 2 ships full editing for
  Postgres/MySQL and the SQLite-safe subset (add/drop/rename column, add/drop
  a plain index) first.

## Phase 1 — Schema metadata layer + read-only inspector

Read-only: satisfies the "View ... table structure" half of bullet 1 plus lets
Phases 2-5 build on real data before any write path exists.

### 1a. `src/db/schema.rs` (new)

Mirrors the shape of `connection.rs`'s `row_key` split: one `pub async fn
table_schema(&self, object: &DatabaseObject) -> Result<TableSchema>` on
`Connection`, dispatching per-engine like `Connection::row_key` does
(`src/db/connection.rs:216-250`) — three SQL-string functions in
`postgres.rs`/`mysql.rs`/`sqlite.rs` alongside the existing `primary_key_sql`,
run through the same `run_query`/`fetch_all` path everything else uses (no new
plumbing in `connection.rs::fetch_all` needed — these are just more
`information_schema`/`pragma_*` reads).

```rust
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,       // driver-reported, e.g. "character varying(255)"
    pub nullable: bool,
    pub default: Option<String>, // raw expression text, shown verbatim
    pub is_primary_key: bool,    // cross-referenced from the PK columns below
}

pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,    // in index order
    pub unique: bool,
    pub is_primary_key: bool,
}

pub struct ForeignKeyDef {
    pub name: String,            // SQLite has none; synthesize "fk_<n>"
    pub columns: Vec<String>,
    pub referenced_schema: Option<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
}

pub enum ReferentialAction { NoAction, Restrict, Cascade, SetNull, SetDefault }

pub struct TableSchema {
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKeyDef>,
}
```

Per-engine sources, each queried separately (three round trips, run
concurrently with `futures::join!` or sequentially — table metadata is small,
sequential is fine and matches how `row_key` is already just one more
query):

- **Postgres**: `information_schema.columns` for columns (already know the
  pattern from `primary_key_sql`'s use of `information_schema
  .key_column_usage`); `pg_indexes`/`pg_index`+`pg_class` (or
  `information_schema.table_constraints`/`key_column_usage` filtered to
  `UNIQUE`/`PRIMARY KEY`, plus a separate non-constraint index read from
  `pg_indexes`) for indexes; `information_schema.table_constraints` joined to
  `key_column_usage` and `constraint_column_usage`/`referential_constraints`
  for foreign keys, same join shape `primary_key_sql` already uses.
- **MySQL**: `information_schema.columns` (has `IS_NULLABLE`,
  `COLUMN_DEFAULT`, `COLUMN_TYPE`); `information_schema.statistics` for
  indexes (`INDEX_NAME`, `NON_UNIQUE`, `SEQ_IN_INDEX`, `COLUMN_NAME`);
  `information_schema.key_column_usage` joined to
  `information_schema.referential_constraints` for FKs (same shape as
  `primary_key_sql`, scoped to `DATABASE()` the same way).
- **SQLite**: `pragma_table_info(table)` for columns (already the source of
  `primary_key_sql`'s `pk > 0` filter — reuse the same pragma, read every
  column this time); `pragma_index_list(table)` + `pragma_index_info(name)`
  for indexes; `pragma_foreign_key_list(table)` for FKs (has `table`, `from`,
  `to`, `on_update`, `on_delete` columns already named for this).

### 1b. New tab kind

`src/ui/session/tab.rs`'s `TabContent` (`src/ui/session/tab.rs:42-63`) grows a
third arm:

```rust
pub(crate) enum TabContent {
    Query { .. },
    Table { view: Entity<TableView> },
    Schema { view: Entity<SchemaView> },
}
```

`SchemaTab::object`/`is_dirty` (`src/ui/session/tab.rs:70-88`) grow the third
arm too — `is_dirty` returns `view.read(cx).is_dirty(cx)` from Phase 2 on
(false in Phase 1, nothing to lose yet). `Session::open_object` becomes
`open_object(&self, object, mode: ObjectViewMode, ...)` where `ObjectViewMode`
is `Data | Schema`, so the same "already open, bring forward" logic
(`src/ui/session/mod.rs:161-185`) applies per-mode — a table's data tab and
its schema tab are different tabs, distinguished by tab title suffix (" —
Structure").

### 1c. Sidebar entry point

`src/ui/session/sidebar.rs`'s tree currently answers a row click only
(`tree(&self.objects_tree, move |...| ... .on_click(...))`,
`src/ui/session/sidebar.rs:126-166`). `gpui_kit::component::tree::Tree`
already carries a `.context_menu(f)` builder (verified in
`gpui-component-0.6.1/src/tree.rs:54-62`, signature `Fn(usize, &TreeEntry,
PopupMenu, &mut Window, &mut Context<TreeState>) -> PopupMenu`) — the same
menu machinery `DataGrid`'s row menu uses
(`.context_menu(self.row_menu())`, `src/ui/data_grid/mod.rs:931`, documented
in AGENTS.md's row-menu section). Add `.context_menu(...)` to the `tree(...)`
call with two items, "Open" (today's default, now explicit) and "Inspect
structure", both resolving the clicked row back to a `DatabaseObject` the same
way the existing `on_click` does (`entry.item().label` → look up in
`self.objects`) and calling `session.update(cx, |s, cx| s.open_object(&object,
ObjectViewMode::Schema, window, cx))`. The context menu closure hands back
`Context<TreeState>`, not `Context<Session>`, so it captures a cloned
`Entity<Session>` the same way the existing item closure captures
`session = cx.entity()` (`src/ui/session/sidebar.rs:104`) rather than trying
to thread a weak handle through `Context<TreeState>`.

### 1d. `SchemaView` (new, `src/ui/schema_view/mod.rs`)

Read-only in this phase: loads `TableSchema` the way `TableView::load_row_key`
loads the primary key (`src/ui/table_view/mod.rs:218-244` — same
`runtime::spawn` + `cx.spawn` + `this.update` shape), and renders three
sections (Columns / Indexes / Foreign keys) as plain `v_flex` rows rather than
`gpui_kit::component::table` — the virtualized `Table`/`TableDelegate` pairing
`DataGrid` uses is built around a `QueryResult`'s row/column grid and its
overlay-edit model (`delegate.rs`'s `pub(super)` overlay fields the AGENTS.md
architecture note describes); a table's own structure is a handful of rows
with heterogeneous per-row controls (a type dropdown here, a checkbox there),
which is a form, not a data grid — plain flex rows match how
`render_confirm`/`render_footer` in `table_view/mod.rs` already build
structured panels without reaching for the table component.

New module registered in `src/ui/mod.rs` (`pub mod schema_view;`, next to
`pub mod table_view;`, `src/ui/mod.rs:13`).

Tests: new `src/ui/tests/schema.rs`, added to the file-per-area convention
AGENTS.md's Tests section describes, fixtures shared through `mod.rs` per the
same rule as every other area.

## Phase 2 — Column editing (Postgres, MySQL, and SQLite's safe subset)

Turns `SchemaView`'s Columns section editable and adds the `ALTER TABLE`
generator, in `src/ui/schema_view/sql.rs` (mirrors how `table_view/sql.rs`
keeps every generated statement in one file per AGENTS.md's "put export and
DDL generation there too" instruction — same rule applies here: DDL generation
belongs beside the schema view, not scattered).

- Each `ColumnDef` row gets an edit affordance: name (`Input`), type (a
  `Button::dropdown_menu` populated with a fixed, per-engine common-type list
  — matching how `render_database_picker` already builds a dropdown from a
  `Vec<String>`, `src/ui/session/mod.rs:955-1017` — rather than standing up a
  full `Select`/`SearchableListDelegate` for a short static list), nullable
  (`Checkbox`), default (`Input`, raw expression text). "Add column" appends a
  blank row (mirrors `TableView::insert_row`'s draft-row pattern
  conceptually, but the row is schema, not data). "Drop column" marks a row
  struck-through — same red/strikethrough pairing `DataGrid`'s deleted-row
  state uses, per AGENTS.md's colour-is-never-the-only-cue rule.
- `SchemaView` diffs the edited column list against the loaded baseline
  (added / dropped / renamed — a rename is detected by matching a dropped
  name against an added one with a stable row id, not by name equality) and
  turns the diff into one or more `ALTER TABLE` statements through
  `schema_view/sql.rs`, engine-dispatched:
  - Postgres: one statement per changed property (`RENAME COLUMN`,
    `ALTER COLUMN ... TYPE ... USING ...`, `SET/DROP NOT NULL`,
    `SET/DROP DEFAULT`) plus `ADD COLUMN`/`DROP COLUMN` — all combinable in
    one `ALTER TABLE table <clause>, <clause>, ...` per Postgres's
    multi-clause `ALTER TABLE` syntax, or as separate statements; separate
    statements are simpler to preview line-by-line and match `run_script`'s
    one-statement-at-a-time execution (`Connection::run_script`,
    `src/db/connection.rs:200-210`), so prefer separate statements over
    packing clauses.
  - MySQL: `MODIFY COLUMN` (retype/nullability/default in one) or
    `CHANGE COLUMN old new ...` (rename, restating the full definition since
    MySQL has no standalone rename).
  - SQLite: `ADD COLUMN`/`DROP COLUMN`/`RENAME COLUMN` only; a retype,
    nullability, or default change on SQLite is refused here with a message
    pointing at "not supported without rebuilding the table" — Phase 3 lifts
    this.
- **Live preview**: every generated statement renders above the Apply button
  in a read-only text block (reuse `Textarea`/`TextareaState` the way
  `ValueView` does for its body, `src/ui/value_dialog.rs:78-93` +
  `readonly(true)`, rather than inventing new syntax highlighting — this is
  a preview, not an editor). Per the design decision above, this shows and
  asks "Apply?" unconditionally; `ReadOnly` connections never reach it because
  `is_editable` (mirroring `TableView::is_editable`,
  `src/ui/table_view/mod.rs:247-251`) is false and the edit affordances are
  disabled with the same `read_only_reason()` footer message pattern.
- Apply runs each statement through `Connection::execute` in order (same
  "no transaction yet, first failure stops the run and says which" contract
  `run_write` already documents, `src/ui/table_view/mod.rs:538-552`), then
  reloads `TableSchema` — no optimistic patch-in-place, since a rename or
  retype changes identity `SchemaView`'s own state does not try to track
  incrementally.

Tests: `src/ui/tests/schema.rs` grows generator-only unit tests per engine
(given a before/after `Vec<ColumnDef>`, assert the exact SQL — same style as
`table_view/sql.rs`'s tests reachable through `TableView::query`/`query_with_params`
`#[cfg(test)]` accessors) plus one UI test per engine exercising the preview
→ Apply → reload round trip against the existing test harness's in-process
databases (see whatever `ui/tests/mod.rs` already spins up for `running.rs`/
`safety.rs` — reuse that fixture rather than adding a new one).

## Phase 3 — SQLite full column editing (rebuild procedure)

Only needed for SQLite retype / nullability / default / add-or-drop-primary-
key-or-foreign-key on an existing column, which plain `ALTER TABLE` cannot
express. Implements SQLite's own documented procedure
(sqlite.org "Making Other Kinds Of Table Schema Changes"):

1. `PRAGMA foreign_keys=OFF` (must run outside a transaction on the same
   connection right before the rest).
2. Begin a transaction (first real use of one — everything up to here in the
   app has run statement-by-statement with no transaction wrapper, per
   `Connection::run_script`'s own doc comment,
   `src/db/connection.rs:194-199` — this phase is the exception, and needs a
   `Connection` method that holds one connection for a multi-statement
   transaction rather than going back through the pool per statement, since
   the whole rebuild must be atomic).
3. `CREATE TABLE new_<name> (...)` with the edited column list.
4. `INSERT INTO new_<name> SELECT <mapped columns> FROM <name>` — a dropped
   column is left out of the `SELECT` list; a renamed column is aliased.
5. `DROP TABLE <name>`.
6. `ALTER TABLE new_<name> RENAME TO <name>`.
7. Recreate every index/trigger/view that pointed at the old table (read via
   `sqlite_master` `sql` column before step 3, replayed after step 6 — a
   generated index recreates cleanly from `IndexDef`; a trigger/view is
   opaque here, so its stored `CREATE ...` text is replayed verbatim).
8. `PRAGMA foreign_key_check` — abort (roll back) if it reports a violation
   the rebuild introduced.
9. Commit, then `PRAGMA foreign_keys=ON`.

This needs a new `Connection::run_transaction(statements: Vec<String>) ->
Result<()>` (or similar) since every existing write path in `connection.rs`
opens per-statement against the pool. Flag for confirmation before starting:
this is the riskiest phase in the plan (it moves every row of the table
through a scratch copy) and probably wants its own explicit go-ahead, plus a
decision on whether it's in scope at all for a first pass versus documenting
the limitation and leaving SQLite on the Phase 2 safe subset.

## Phase 4 — Index manager

Read/write over `IndexDef`, in the same `SchemaView`, a new "Indexes" section
next to Columns:

- List existing indexes (name, columns in order, unique?, is the primary
  key?). "Add index": column multi-select (order matters — composite index
  column order changes what it can serve), unique checkbox, name (defaulted,
  editable). "Drop index" per row.
- Generator (`schema_view/sql.rs`, same file as Phase 2's column generator):
  - Postgres/SQLite: `CREATE [UNIQUE] INDEX name ON table (cols)`;
    `DROP INDEX name` (Postgres: schema-qualified if the table's schema
    isn't the search-path default, same `quote_identifier`-driven qualifying
    `table_view/sql.rs::target()` already does, `src/ui/table_view/sql.rs:24-34`).
    A **primary key** add/drop is `ALTER TABLE ... ADD/DROP CONSTRAINT`
    (Postgres) since SQLite cannot add a primary key to an existing table at
    all without the Phase 3 rebuild — disable that specific action on SQLite
    with an inline reason, same `read_only_reason()`-style messaging.
  - MySQL: `CREATE INDEX name ON table (cols)` or
    `ALTER TABLE table ADD UNIQUE (cols)`; `DROP INDEX name ON table`
    (index names are table-scoped, unlike the other two engines — the drop
    statement needs the table name where Postgres/SQLite's doesn't).
    `ALTER TABLE table ADD PRIMARY KEY (cols)` / `DROP PRIMARY KEY`.
- Same live-preview-then-Apply flow as Phase 2, same unconditional confirm.

Tests: generator unit tests per engine (uniqueness of a table-scoped MySQL
drop vs a schema-scoped Postgres/SQLite drop is exactly the kind of thing
worth a regression test), plus one integration test per engine.

## Phase 5 — Foreign key relationship builder

Read/write over `ForeignKeyDef`:

- List existing FKs (name, local columns, referenced table.columns, on
  delete/update actions). "Add foreign key": local column picker, referenced
  table picker (sourced from `Connection::objects()` — reuse the already-
  loaded `Session::objects` list, `src/ui/session/mod.rs:262-264`, rather
  than a fresh query), referenced column picker (needs that table's own
  `TableSchema`, so picking a referenced table triggers a second
  `table_schema` read scoped to it), on-delete/on-update
  dropdowns (`ReferentialAction` variants, same `Button::dropdown_menu`
  pattern as the type picker in Phase 2).
- Generator: Postgres/MySQL `ALTER TABLE ... ADD CONSTRAINT name FOREIGN KEY
  (cols) REFERENCES table (cols) ON DELETE <action> ON UPDATE <action>`;
  drop is `DROP CONSTRAINT name` (Postgres) / `DROP FOREIGN KEY name`
  (MySQL). SQLite cannot add or drop a foreign key on an existing table at
  all (not even via plain `ALTER TABLE`) — every FK change on SQLite routes
  through Phase 3's rebuild, so this phase is gated on Phase 3 landing first
  if SQLite support matters; Postgres/MySQL FK editing has no such
  dependency and could ship even if Phase 3 is deferred.
- Same live-preview-then-Apply flow, same unconditional confirm.

## Cutting the scope down

If the full five-phase build is more than wanted right now, Phases 1-2
(read-only inspector, then Postgres/MySQL + SQLite-safe-subset column
editing) already deliver bullet 1 and the "Live SQL preview" bullet for the
two engines with straightforward `ALTER TABLE`, which is most of the value.
Phases 3-5 (SQLite rebuild, index manager, FK builder) are independent of
each other and can ship in any order after Phase 2 — Phase 3 is flagged above
as worth a separate go/no-go given its risk.
