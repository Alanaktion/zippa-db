# Foreign keys: "Referenced by" (inbound navigation)

## Context
Outbound navigation already exists: a foreign-key cell's row menu jumps to the referenced row (`TableView::on_grid_navigate`, `table_view/mod.rs`, emitting `TableViewEvent::NavigateToForeignKey` with a `FilterSpec`; the session opens the target table and calls `apply_external_filter`). Tests are in `src/ui/tests/rows.rs`. Two gaps remain:
1. The other direction: from a `customers` row, find the `orders` rows that point at it.
2. Composite foreign keys: `on_grid_navigate` only handles `fk.columns.len() == 1`, and `apply_external_filter` takes one filter.

Decision (from user): fill the gaps only; do not rework the existing jump.

## Design

### 1. Inbound key discovery — `src/db/schema.rs` + per-engine SQL
`ForeignKeyDef` describes a key owned by the table. Add a type for the reverse view rather than overloading it:
```rust
pub struct InboundKey {
    pub table: DatabaseObject,       // the referencing (child) table
    pub name: String,
    pub columns: Vec<String>,        // child columns
    pub referenced_columns: Vec<String>, // columns on this table, same order
    pub on_delete: ReferentialAction,
}
```
`Connection::referencing_keys(&DatabaseObject) -> Result<Vec<InboundKey>>`, one round trip per call, SQL in `postgres.rs`/`mysql.rs`/`sqlite.rs` beside `foreign_keys_sql`:
- Postgres: `pg_constraint` where `contype = 'f'` and `confrelid = $table::regclass`, with `conkey`/`confkey` unnested in order, joined to `pg_class`/`pg_namespace` for the child's schema and name; `confdeltype` for the action.
- MySQL: `information_schema.key_column_usage` where `referenced_table_schema = database()` and `referenced_table_name = ?`, grouped by constraint like the existing outbound query; `referential_constraints` for the action.
- SQLite: no reverse catalog. `SELECT m.name, f.* FROM sqlite_master m, pragma_foreign_key_list(m.name) f WHERE m.type = 'table' AND f."table" = ? COLLATE NOCASE`, grouping rows by `id`. Requires `pragma_foreign_key_list` table-valued function (SQLite 3.16+, fine with the bundled sqlx build; verify).
Reuse the object-qualification rule (`schema` omitted when `public`) so results match sidebar `DatabaseObject`s and open the existing tab instead of a duplicate.

### 2. Loading — `TableView`
Load alongside `load_foreign_keys` (`table_view/mod.rs:322`): field `inbound: Option<Vec<InboundKey>>`, reset and reloaded whenever the table changes, same error handling (an error just means no inbound entries). A key whose child table is a view or is not in `objects` is dropped.

### 3. Menu and panel entries
- Row menu (built in `data_grid/delegate.rs` where the outbound "Go to referenced row" lives): a `Referenced by` submenu listing `orders (customer_id)` per inbound key. Each item opens the child table filtered to rows whose FK columns equal this row's referenced-column values. Only offered when the row's referenced-column values are all present (not NULL) and the table has inbound keys. The grid stays ignorant of keys: like the outbound jump, it emits an event with the row and an index into the list, and `TableView` resolves it.
- Row panel (`table_view/row_panel.rs`): a `Referenced by` section under the fields for the focused row: one line per inbound key with a button `Open`, and `Count` (lazy, on demand) that runs `SELECT count(*) FROM child WHERE fk = ?` with a 5 s timeout and prints `12 rows` or `at least 10,000`. Never counts automatically: a count on a large child table is expensive.
- Keyboard: the row menu item is reachable through the existing menu; the panel buttons are tab stops. Optionally a `secondary-alt-down` "Follow reference" for the single-inbound-key case; skip in v1 (conflicts risk).
- Labels say the direction in words: `Referenced by orders.customer_id`, `References customers.id`. Icons (`ArrowUp`/`ArrowDown` if present) are decoration only.

### 4. Composite keys (both directions)
- Extend `TableViewEvent::NavigateToForeignKey.filter: FilterSpec` to `filters: Vec<FilterSpec>`; `apply_external_filter` becomes `apply_external_filters` and sets one filter line per column (AND semantics already in `FilterBar`). Existing single-key call sites pass a one-element vec; update the two callers and the `rows.rs` tests.
- `on_grid_navigate` accepts a key whose columns include the clicked column (not only single-column keys) and collects every referenced column value from the same row; a NULL in any part of a composite key means there is nothing to follow (matches the existing NULL rule and test `a_null_foreign_key_has_nothing_to_follow`).
- The outbound menu label for a composite key reads `Go to customers (a, b)`.

### 5. Session side — `src/ui/session/mod.rs`
The handler for `NavigateToForeignKey` already opens or focuses the target table tab and applies the filter; generalise it to `filters` and reuse it for inbound jumps (same event; the "object" is the child table). No new tab kind.

### 6. Docs
README feature line, TODO.md (this is not a listed item; add a checked line under Data Browser), CLAUDE.md "Filtering" gets a sentence on jumps in both directions.

## Sequencing
1. `InboundKey` + `referencing_keys` per engine with SQLite tests.
2. Multi-filter generalisation (`filters: Vec<FilterSpec>`) with existing tests passing unchanged in behaviour.
3. Composite outbound jumps.
4. Inbound menu submenu.
5. Row panel section with lazy count.

## Critical files
- edit: `src/db/{schema,postgres,mysql,sqlite,mod}.rs`, `src/ui/table_view/{mod,row_panel,sql}.rs`, `src/ui/data_grid/{mod,delegate}.rs`, `src/ui/session/mod.rs`, `src/ui/tests/rows.rs`, `src/ui/tests/mod.rs` (fixture with parent/child/composite tables)
- reuse: `FilterSpec`, `Operator::Equals`, `apply_external_filter`, `ObjectKind`, existing FK test fixtures.

## Testing
- DB (SQLite): `referencing_keys` finds `orders.customer_id` for `customers`, finds two children, finds a composite key with columns in order, returns nothing for an unreferenced table, is case-insensitive on names.
- UI (SQLite): inbound jump opens `orders` filtered to the parent's id; second jump reuses the tab and replaces the filter; NULL referenced value offers nothing; composite outbound jump sets two filters; a row with no children opens an empty filtered table with `0 rows`; row panel `Count` shows the number.
- Manual Postgres/MySQL: self-referencing table (`employees.manager_id`), cross-schema child on Postgres, key with `ON DELETE CASCADE`.

## Risks / open questions
- Self-referencing keys: the inbound and outbound target are the same table; the jump should open a new filtered tab rather than replace the current one. Check how the session decides reuse versus new (probably by object identity) and add the filter to that identity if needed.
- A child table in a schema the sidebar does not list yet (Postgres schemas other than the default): `DatabaseObject` qualification must match `objects()` output or the tab will not dedupe.
- Wide fan-out (a `users` table referenced by 40 tables): the submenu needs scrolling or a search; cap the menu at ~15 with `More…` opening the row panel section.
