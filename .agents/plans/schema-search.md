# Schema search (columns, indexes, routines, definitions)

## Context
Finding things in a large schema means scrolling the sidebar. The sidebar filter (`session/sidebar.rs`, regex over object labels) and the quick switcher (`Cmd+K`, `quick_switcher.rs`) only know table, view, and routine names, so `where is email stored?` or `what index covers created_at?` cannot be answered without opening tables. Goal: a search over the whole database that matches column names/types, indexes, routines, triggers, and optionally the text of view and routine definitions, with results that open the right place.

## Decisions
- Its own dialog rather than more modes crammed into the quick switcher: `Search schema…`, `secondary-shift-o` (verify free in `keymap.rs`), also reachable from the quick switcher's action list and the sidebar header.
- Metadata is fetched once per session and cached, refreshed by the existing `Refresh` action, so typing never hits the database. Definition-text search (bodies) is a second, on-demand stage because it is large.
- Search is plain case-insensitive substring by default, with the same regex toggle convention as the sidebar filter (`text_filter`), so behaviour is consistent.

## Design

### 1. Catalog snapshot — `src/db/catalog.rs` (new)
```rust
pub enum CatalogKind { Table, View, Column, Index, Routine, Trigger }
pub struct CatalogEntry {
    pub kind: CatalogKind,
    pub object: DatabaseObject,      // owning table/view (or routine's own identity)
    pub name: String,                // column / index / routine / trigger name
    pub detail: String,              // column type, index columns, routine arguments, trigger event
}
```
`Connection::catalog() -> Result<Vec<CatalogEntry>>` runs one bulk query per kind (not per table), SQL in each engine file:
- Postgres: `information_schema.columns` (or `pg_attribute` with `format_type`) for all non-system schemas; `pg_indexes`/`pg_index`; `pg_proc` (already used by `ROUTINES_SQL`); `pg_trigger` where not `tgisinternal`.
- MySQL: `information_schema.columns`, `statistics`, `routines`, `triggers` for `database()`.
- SQLite: `pragma_table_info`/`pragma_index_list` joined over `sqlite_master` (table-valued pragma functions), plus `sqlite_master` rows for triggers.
Exclude system schemas (`pg_catalog`, `information_schema`, `sqlite_%`, `mysql`, `sys`). Cap and warn beyond a sane size (e.g. 200,000 entries): `Showing first 200,000 of N entries.`
`objects()` and `stored_objects()` already cover tables/views/routines; the catalog reuses them for those kinds rather than re-querying, so it only adds columns, indexes, and triggers.

### 2. Definition text search (stage two, opt-in checkbox `Search definitions`)
- `Connection::definitions() -> Result<Vec<(CatalogEntry, String)>>`: `pg_get_viewdef`/`pg_get_functiondef` (Postgres), `routines.routine_definition` and `views.view_definition` (MySQL), `sqlite_master.sql` (SQLite). Loaded on first use of the checkbox, shown as a `Loading definitions…` line, cached alongside the catalog.
- Matches show a snippet: the matching line with the hit highlighted (bold plus underline, not colour alone).
- Encrypted or restricted bodies: entries the user cannot read are skipped silently.

### 3. Ranking and matching — pure function `catalog::search(&[CatalogEntry], &Query) -> Vec<Hit>`
Score: exact name > prefix > word-boundary > substring; name matches outrank type/detail matches; tables/views outrank columns on ties; stable within a score by owning table then ordinal. Query grammar kept tiny and discoverable in placeholder text: `kind:column`, `kind:index`, `type:uuid` (matches `detail`), `table:orders`; anything else is free text. Unknown prefixes are treated as text. Unit-tested without any UI.

### 4. UI — `src/ui/schema_search.rs` (new; model on `quick_switcher.rs`)
- `SchemaSearchView` uses gpui-kit `CommandState`/`Command` like the switcher, grouped by kind with headings. Each row: icon (decoration), name, muted `owner · detail` (`orders · uuid`), and the kind word (`Column`) so it is not icon-only.
- Kind filter chips (`All`, `Tables`, `Columns`, `Indexes`, `Routines`, `Triggers`) as toggle `Button`s with accessibility labels; `Search definitions` `Checkbox`.
- Activating a result emits `SchemaSearchEvent::Open(CatalogEntry)`; the session decides:
  - Table/View: open the data tab (same as the sidebar).
  - Column: open the table's data tab and scroll to and select that column (needs `DataGrid::reveal_column`; if the column is hidden by the current view, reveal it), or, with `Alt+Enter`, open the Structure tab.
  - Index/Trigger: open the table's Structure tab (`ObjectViewMode::Schema`).
  - Routine: open it in the routine view the recent "function/routine UI" commit added; check `session/mod.rs` for the entry point.
- Empty states: `Loading schema…` (first open), `No matches`, error in words (`Error: …`).
- Focus: search input focused on open; `Escape` closes; arrow keys and `Enter` via the component.

### 5. Wiring
- `SearchSchema` action in `session/mod.rs` `actions!`; handler opens the dialog if none active (`window.has_active_dialog`); binding contexts mirror the quick switcher's list (`Session`, `QueryEditor > Input`, `QueryEditor`, `TableView`, `TableView > DataTable`, `DataGrid > DataTable`).
- Session holds `catalog: Option<Arc<Vec<CatalogEntry>>>`, loaded in the background right after `objects` load (same place, via `runtime::spawn`) so it is warm by the time anyone searches; cleared and reloaded on `Refresh` and database switch.
- Menu `View > Search Schema…`; shortcuts dialog; README; TODO.md; CLAUDE.md sentence under the sidebar description.

## Sequencing
1. `catalog.rs` types, `search()` ranking + query grammar with unit tests.
2. SQLite `catalog()` and tests, then Postgres and MySQL SQL.
3. Dialog with tables/columns/indexes; session open handlers.
4. Routines and triggers; kind chips.
5. Definition search (stage two).

## Critical files
- new: `src/db/catalog.rs`, `src/ui/schema_search.rs`, `src/ui/tests/schema_search.rs`
- edit: `src/db/{mod,connection,postgres,mysql,sqlite}.rs`, `src/ui/session/{mod,sidebar}.rs`, `src/ui/data_grid/mod.rs` (`reveal_column`), `src/ui/quick_switcher.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/menu.rs`, `src/ui/shortcuts_dialog.rs`, `README.md`, `TODO.md`
- reuse: `text_filter`, `DatabaseObject`, `StoredObject`, `QuickSwitcherView` structure, `ObjectViewMode`.

## Testing
- Unit: ranking order (exact > prefix > substring), `kind:`/`type:`/`table:` parsing, regex toggle, unknown prefix as text, entry cap.
- DB (SQLite fixtures): `catalog()` returns columns with types, indexes with their columns, triggers; ignores `sqlite_%` tables.
- UI: opening the dialog after a load lists columns; typing narrows; Enter on a column opens the table tab with that column selected; Enter on an index opens Structure; `Refresh` picks up a newly created column; definition search finds text inside a view.
- Manual on a large Postgres schema (thousands of tables): dialog opens instantly, typing stays responsive (filter on a background task if a keystroke takes over ~16 ms), memory reasonable.

## Risks / open questions
- Postgres multi-schema: `objects()` shows non-`public` schemas qualified; catalog entries must use the same `DatabaseObject` shape or dedupe against open tabs breaks.
- Loading columns for a database with tens of thousands of tables is a heavy query; run it once per session in the background and never block the UI. If it times out, degrade to table/view/routine names with a notice.
- `Cmd+Shift+O` may be taken by "open file" conventions in some editors but the app's `OpenFile` binding should be checked before choosing.
