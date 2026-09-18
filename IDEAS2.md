Ranked by value for effort, given the current architecture (sqlx, `Connection`, `statement.rs`, `TableView`).

**High value, practical**

1. **EXPLAIN / query plan viewer.** "Explain" and "Explain analyze" buttons on the editor. Run `EXPLAIN (FORMAT JSON)` on Postgres, `EXPLAIN FORMAT=JSON` on MySQL and `EXPLAIN QUERY PLAN` on SQLite. Show an indented tree with cost, rows and time, and highlight seq scans. This reuses the run path and needs no new infrastructure.
2. **Transaction mode in the editor.** Add manual `BEGIN`/`COMMIT`/`ROLLBACK` with a visible "Transaction open" status. Today each statement may hop pooled connections, so `SET` and temp tables don't persist, and `sql-import.md` already found this problem. Pinning one `PoolConnection` per session tab fixes both. It's the biggest correctness gap for a SQL GUI.
3. **Foreign-key navigation in the grid.** Cmd-click a FK cell to open the referenced row. A "Referenced by" list in the row panel gives child rows. The schema introspection in `schema.rs` probably has most of the FK data already.
4. **Schema-level search.** Search columns, indexes and routines across the whole database, not just table names. It could extend the quick switcher, and a `Cmd+Shift+F` search over object definitions would help too.
5. **Structure and DDL views.** A read-only "Structure" tab per table with columns, indexes, constraints, triggers and generated `CREATE TABLE` text with a copy button. `sqlite-schema-editing.md` covers SQLite editing. This is the read-only half for all three engines.
6. **Copy as…** Right-click on a selection: copy as CSV, JSON, Markdown, SQL `INSERT` or TSV. It's small, and the export plans may already cover the file version. Cheap clipboard variants make the tool feel finished.
7. **Format SQL** (`Cmd+Shift+F` or similar). Use the `sqlformat` crate, dialect-aware. Format the selection or the whole buffer.

**Medium value**

8. **Multiple result sets from routines.** Handle `CALL` or multi-select output, and the "Messages" tab (Postgres `NOTICE`, MySQL warnings). The TODO notes "detailed warnings not yet included".
9. **Query parameters.** `:name` or `$1` in the editor prompts for values, then binds them. This avoids string-pasting values, and it pairs with snippets.
10. **Editor tabs persist unsaved buffers.** Drafts survive quit and crash. This extends `workspace-state-persistence.md`, so include unsaved query text, not only open tabs.
11. **Find and replace in the editor, plus multi-cursor and comment toggle.** Check what `gpui-kit`'s `InputState` already provides before building.
12. **Column stats and data profiling.** A column-header menu with count, distinct, null %, min/max and top values, run as a generated query. It's useful for exploring unknown tables.
13. **Table/row diff.** Compare two result sets, or the same table in two connections. This is a stretch feature, but developers value it for staging against production.
14. **Server activity view.** `pg_stat_activity` and `SHOW PROCESSLIST` with kill/cancel. Read-only mode blocks kill.
15. **Bulk edit.** A "Set column to…" action on the selected rows or a filtered set, staged as an `UPDATE` like other edits.
16. **Large-value handling.** Lazy-load blobs and long text, with a hex view and an image preview in the value dialog. Add a "Save to file" action.

**Polish that makes it feel robust**

17. **Undo/redo for staged grid edits.** `Cmd+Z` inside the grid overlays.
18. **Result-set actions.** Pin a result, compare two runs, and re-run with `Cmd+R`. Auto-refresh every N seconds, with a visible indicator.
19. **Connection health.** Auto-reconnect on a dropped connection, and a keepalive. Show "Connection lost — Reconnect" instead of a raw sqlx error. Long-lived GUI sessions hit this constantly.
20. **Statement timeout and row-limit safety.** Add a default `LIMIT` guard on unbounded `SELECT` in the editor, with a "Fetched first 10,000 rows" banner and a load-more action. It protects memory. Add an optional statement timeout per connection.
21. **Connection URL paste.** Paste `postgres://user:pw@host/db` to fill the form. Also read `PGHOST`/`DATABASE_URL`, or import `~/.pgpass` and `~/.my.cnf`, as an import option.
22. **Command palette for everything.** Extend `Cmd+K` to every action, with its shortcut shown. Also make the shortcut dialog searchable.
23. **Crash and error log.** A "Copy diagnostics" action in Help with version, OS and last errors. It's cheap and cuts support time.
24. **Auto-update check and release packaging.** Signed macOS/Windows builds and a `.deb`/AppImage. Not a feature, but necessary for real users.

**Would skip for now**

- ER diagram generation. It looks good but is costly, and few people use it daily.
- Cloud sync of connections or snippets. Sync a plain JSON file with a "Reveal in Finder" button instead.
- Plugin system, non-SQL databases and AI query generation. Each is a large design decision and a distraction at this stage.

**Suggested order:** the pinned per-tab connection with transactions (#2), then EXPLAIN (#1), FK navigation (#3), Structure/DDL (#5), Copy as (#6), and connection health (#19). Those close the correctness gaps and give the most daily-use value. I can write plans for any of them in `.agents/plans/` if you want.
