# EXPLAIN / query plan viewer

## Context
There is no way to see a query plan short of typing `EXPLAIN` by hand and reading a wall of text. `statement.rs` already classifies `EXPLAIN` as a read and already treats `EXPLAIN ANALYZE` as running what it explains (see the classifier tests around `explain select` and the CTE case), so the safety groundwork exists. Goal: `Explain` and `Explain analyze` actions in the editor that show the plan as a readable tree with cost, rows, time, and warnings.

Decisions (from user): plain `EXPLAIN` is always available; `ANALYZE` runs only for statements the classifier says are reads. For writes it is disabled with a tooltip in v1. Running `ANALYZE` on a write inside an auto-rolled-back transaction (Postgres/SQLite) becomes possible once `pinned-connections-transactions.md` lands and is a follow-up.

## Design

### 1. Plan model and parsers — `src/db/plan.rs` (new, re-exported)
```rust
pub struct PlanNode {
    pub label: String,                 // "Seq Scan on orders", "SEARCH items USING INDEX …"
    pub details: Vec<(String, String)>, // Filter, Index Cond, Sort Key, …
    pub estimated_rows: Option<f64>,
    pub actual_rows: Option<f64>,
    pub cost: Option<(f64, f64)>,       // startup, total
    pub actual_ms: Option<f64>,         // per-node total, per loop
    pub loops: Option<u64>,
    pub children: Vec<PlanNode>,
    pub warnings: Vec<String>,
}
pub struct Plan { pub root: PlanNode, pub planning_ms: Option<f64>, pub execution_ms: Option<f64>, pub analyzed: bool }
```
- Postgres: `EXPLAIN (FORMAT JSON)` / `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`. The single cell arrives as text; parse with `serde_json`. Map `Node Type`, `Relation Name`, `Alias`, `Index Name`, `Startup/Total Cost`, `Plan Rows`, `Actual Rows`, `Actual Total Time`, `Actual Loops`, `Plans`, and pass through `Filter`, `Index Cond`, `Hash Cond`, `Sort Key`, `Sort Method`, `Rows Removed by Filter`, and buffer counts as `details`. Exclusive time of a node = total × loops − sum of children's total × loops, so the viewer can show where time actually goes.
- SQLite: `EXPLAIN QUERY PLAN` returns `(id, parent, notused, detail)`; build the tree from `parent` ids. No costs; the label is `detail`.
- MySQL: 8.0.16+ `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE` return an indented text tree (`-> Filter: … (cost=… rows=…) (actual time=…s rows=… loops=…)`); parse indentation and the two parenthesised groups. Where that is unavailable (older MySQL, MariaDB, which uses `ANALYZE FORMAT=JSON`), fall back to classic tabular `EXPLAIN` and show it in the ordinary grid instead of the tree. Detect by trying and falling back on the server error, not by parsing the version string.
- Parsers are pure functions over `&str`/rows, unit-tested with captured fixtures per engine. A parse failure never hides the plan: show the raw text output in the same panel with `Could not read this plan format; showing raw output.`

### 2. Warnings (heuristics, shown with the word `Warning:` and an icon, not colour alone)
- Postgres: `Seq Scan` with a `Filter` on an estimate over 10,000 rows; estimated vs actual rows off by more than 10×; `Sort Method: external merge` (spilled to disk); `Nested Loop` with a high `loops` on an inner seq scan; `Rows Removed by Filter` exceeding rows returned by 10×.
- SQLite: `SCAN` of a table (versus `SEARCH`), `USE TEMP B-TREE`.
- MySQL tree: `Table scan`, `Using temporary`/`filesort` when present in classic output.
Thresholds are constants in `plan.rs` so tests can pin them; keep the list short and only include warnings a reader can act on.

### 3. Running it — `Connection::explain`
`Connection::explain(sql: &str, analyze: bool) -> Result<Plan>`:
- Take one statement (the selection, else `statement::at_cursor`, same rule as Run). Strip a trailing `;`. A script or empty buffer says `Select one statement to explain.`
- If the statement already starts with `EXPLAIN`, run it as written and parse the result (do not double-wrap).
- Wrap per engine (`EXPLAIN (FORMAT JSON)` etc.) and run through `run_query`. Use the tab's `PinnedConnection` once it exists so `search_path`/temp tables match what the user ran; until then the pool.
- `analyze` true: require `statement::first_write(stmt).is_none()`; otherwise return `Error: EXPLAIN ANALYZE would run this statement; it changes data.` The UI disables the button in that case so the error is only a backstop. ReadOnly connections allow plain `EXPLAIN` (the classifier already reads it as a read) and allow `ANALYZE` only on reads.
- Known gap to document: a `SELECT` calling a volatile function (`nextval`, a user function with side effects) counts as a read, so `ANALYZE` will run it. The confirmation text says `EXPLAIN ANALYZE runs the query.` and, on `ConfirmWrites`/`Staged`, requires one click through before running.

### 4. UI
- `src/ui/plan_view.rs` (new): `PlanView` entity. Tree rows use gpui-kit `Tree` (as the sidebar does) so keyboard navigation and ARIA roles come free. Columns beside the label: est. rows, actual rows, cost, time, and a share-of-total-time bar with the percentage written next to it. Expand/collapse all, and `Copy plan` (raw text) and `Copy JSON`. A summary line: `Planning 0.4 ms · Execution 12.8 ms · analyzed`.
- Editor toolbar: `Explain` and `Explain analyze` icon buttons with `.accessibility_label` and `tooltip_with_action`; analyze disabled with tooltip `Analyze runs the query; this statement changes data` for writes.
- Result area gets a two-way `Results | Plan` switch, shown only after an explain, so a plan does not replace the result grid the user was reading. `TabContent::Query` gains `plan: Option<Entity<PlanView>>` and `show_plan: bool`.
- Actions `Explain`, `ExplainAnalyze` in `query_editor.rs` `actions!`; keymap: `secondary-e` and `secondary-shift-e` in `QueryEditor > Input` and `QueryEditor` (check `keymap.rs` and the input's own bindings for conflicts first); shortcuts dialog, README, and quick switcher entries.
- Status bar text after a run: `Plan for 1 statement in 3 ms`, or the error in words.

### 5. Sequencing
1. `plan.rs` types, SQLite parser (testable end to end), fixtures for Postgres/MySQL JSON and text.
2. `Connection::explain` with the analyze gate and classifier tests.
3. `PlanView` and tab wiring; plain EXPLAIN first, then analyze.
4. Warnings, keybindings, docs, TODO/README.

## Critical files
- new: `src/db/plan.rs`, `src/ui/plan_view.rs`, `src/ui/tests/explain.rs`
- edit: `src/db/{connection,mod,statement}.rs`, `src/ui/query_editor.rs`, `src/ui/session/{mod,tab}.rs`, `src/ui/mod.rs`, `src/keymap.rs`, `src/ui/shortcuts_dialog.rs`, `src/menu.rs`, `README.md`, `TODO.md`
- reuse: `statement::{at_cursor, first_write}`, `Tree`/`TreeState` as in `session/sidebar.rs`, `Status`, `notify_error`.

## Testing
- Parsers: PG JSON fixture (with and without analyze, nested plans, loops) yields expected tree, exclusive times, and warnings; malformed JSON falls back to raw; SQLite QUERY PLAN rows build the right tree; MySQL tree text fixture parses.
- Gate: `explain(…, analyze=true)` on `insert`, on a data-modifying CTE, and on `explain analyze delete` is refused; on `select` allowed; plain explain of a write is allowed and does not run it (assert row count unchanged on a SQLite table).
- UI (SQLite): explain a `select … where` shows a `SCAN`/`SEARCH` node and the warning; button disabled for `delete`; switching Results/Plan keeps both; explain with a two-statement buffer and no selection uses the caret's statement.
- Manual against real Postgres and MySQL 8: check parse of a join, a CTE, a sort spilling to disk, and MariaDB fallback.

## Risks / open questions
- MySQL's text tree format has changed between 8.0 minors; the parser must degrade to raw text rather than fail.
- Very large plans (hundreds of nodes) need collapsed-by-default children beyond depth ~6.
- Should `Explain` also work from a table tab's filter (explain the page query)? Not in v1.
