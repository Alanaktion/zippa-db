# TODO

The feature backlog. Shipped work is checked off and kept here for context; the
open items are roughly ordered by value for effort within each section. Design
notes for the larger open items live in [`.agents/plans/`](.agents/plans/);
known defects and risks in the shipped code are tracked in
[AUDIT.md](AUDIT.md).

## 1. Connections & security

* [x] Username/password authentication, with passwords in the OS keychain
* [x] One tag + colour per connection (e.g. red `Production`, green `Local`), shown on the card, the tab, and a strip across the session
* [x] Per-connection safety mode: read-only, confirm every write, staged (default), or auto-apply
* [x] Workspace state persistence: reopen the previous connections, tabs, and query buffers on startup
* [ ] SSH tunneling (password and private key / agent)
* [ ] SSL/TLS connection modes (`disable`, `prefer`, `require`, `verify-full`)
* [ ] Paste a connection URL (`postgres://user@host/db`) to fill the form; read `DATABASE_URL`, `~/.pgpass`, `~/.my.cnf`
* [ ] Connection health: detect a dropped connection, offer "Reconnect" rather than a raw driver error, keepalive
* [ ] Optional statement timeout per connection

## 2. Data grid & table view

* [x] Virtualized grid with auto-sized columns, striped rows, and always-on scrollbars
* [x] Paging, a row limit, click-to-sort, and a filter bar (including `IN` with a subquery)
* [x] Staged editing: edited cells, new rows, and deleted rows written together as `UPDATE`/`INSERT`/`DELETE`
* [x] Row selection by checkbox, drag, `Shift`/`Cmd`-click, and keyboard
* [x] Row panel beside the grid, one field per column
* [x] Value dialog for long, multi-line, JSON, or binary values
* [x] Follow a foreign key from a cell to the referenced row
* [ ] "Referenced by": the rows in other tables that point at this one, and composite foreign keys ([plan](.agents/plans/fk-referenced-by.md))
* [ ] Quick search across every column of a table
* [ ] Date/time picker for timestamp cells
* [ ] Undo/redo for staged grid edits
* [ ] Bulk edit: "Set column to…" on the picked rows
* [ ] Multi-column sorting
* [ ] Column stats: count, distinct, null %, min/max, top values
* [ ] Reorder columns by dragging a header (turned off today: the grid addresses cells by the result's column order)

## 3. SQL editor

* [x] SQL highlighting; run the statement under the caret, the selection, or the whole buffer
* [x] Result bar with one result per statement, rows affected/returned, and elapsed time
* [x] Cancel a running query
* [x] Scripts run in one transaction on Postgres and SQLite
* [x] Query plan viewer: `EXPLAIN` / `EXPLAIN ANALYZE` as a tree with cost, rows, timing, and warnings
* [x] Open and save `.sql` files
* [ ] Auto-complete for tables, columns, keywords, and schemas from live introspection
* [ ] Engine-specific highlighting (today one SQL grammar serves all three)
* [ ] Format SQL ([plan](.agents/plans/format-sql.md))
* [ ] A pinned connection per query tab, so `BEGIN`/`COMMIT`, `SET`, and temp tables persist between runs ([plan](.agents/plans/pinned-connections-transactions.md))
* [ ] Messages tab: Postgres `NOTICE`, MySQL warnings
* [ ] Query parameters (`:name`, `$1`) prompted for and bound
* [ ] A row-limit guard on unbounded `SELECT`s, with a "Fetched first N rows" banner
* [ ] Middle-mouse drag for a column (multi-line) selection, like Zed
* [ ] Find and replace, multi-cursor, comment toggle (check what `gpui-kit`'s editor already offers)

## 4. Schema browser & editor

* [x] Sidebar object list with a regex filter, one folder per routine kind
* [x] Schema search over tables, views, columns, indexes, routines, and triggers (`Cmd+Shift+O`)
* [x] Structure tab: edit columns, indexes, and foreign keys; preview the generated `ALTER`s (or SQLite rebuild) before they run
* [ ] Tree navigation: databases > schemas > tables / views
* [ ] Read-only DDL text, triggers, and check constraints in the structure tab ([plan](.agents/plans/structure-ddl-view.md))
* [ ] Search the text of view, routine, and trigger definitions
* [ ] Free-text column types in the structure tab (today a fixed per-engine list)
* [ ] View and routine definition editor

## 5. Import, export & productivity

* [x] Export a table or the picked rows to CSV, TSV, JSON, Markdown, or SQL `INSERT`
* [x] Copy rows or a column as TSV, CSV, JSON, Markdown, SQL `INSERT`, plain values, or an `IN` list
* [x] Import SQL dumps: streamed, gzip/bzip2/zstd, progress bar, stop / roll back / continue on error
* [x] Quick switcher over tabs, objects, databases, and actions (`Cmd+K`)
* [x] Keyboard shortcut list (`Ctrl+/`)
* [ ] Stream a large export instead of reading the whole table into memory first
* [ ] Import CSV into an existing table ([plan](.agents/plans/csv-import.md))
* [ ] Backup via `pg_dump` / `mysqldump`
* [ ] Local query history, searchable by date, database, or text ([plan](.agents/plans/query-history.md))
* [ ] Reusable SQL snippets ([plan](.agents/plans/sql-snippets.md))
* [ ] Customizable keyboard shortcuts
* [ ] Server activity view (`pg_stat_activity`, `SHOW PROCESSLIST`) with cancel/kill
* [ ] "Copy diagnostics" in the Help menu: version, OS, recent errors

## 6. Platform & release

* [x] Packaging with `cargo packager` (macOS `.app`/`.dmg`, Linux AppImage/`.deb`, Windows NSIS)
* [x] CI: `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on Linux
* [ ] macOS and Windows builds in CI
* [ ] Signed macOS and Windows builds, and an update check
* [ ] Full audit against a screen reader on each platform
* [ ] Dialog transitions that can be turned off (needs upstream support in `gpui-kit`)

## Not planned for now

* ER diagram generation — costly, and rarely used day to day.
* Cloud sync of connections or snippets — the config directory is plain JSON and can be synced by other means.
* A plugin system, non-SQL databases, and AI query generation — each is a large design decision.
