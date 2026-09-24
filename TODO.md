# TODO

The feature backlog: what is not built yet, roughly ordered by value for effort
within each section. What already ships is described in [README.md](README.md)
and [CHANGELOG.md](CHANGELOG.md). Design
notes for the larger open items live in [`.agents/plans/`](.agents/plans/);
known defects and risks in the shipped code are tracked in
[AUDIT.md](AUDIT.md).

## 1. Connections & security

* SSH tunneling (password and private key / agent)
* SSL/TLS connection modes (`disable`, `prefer`, `require`, `verify-full`)
* Paste a connection URL (`postgres://user@host/db`) to fill the form; read `DATABASE_URL`, `~/.pgpass`, `~/.my.cnf`
* Connection health: detect a dropped connection, offer "Reconnect" rather than a raw driver error, keepalive
* Optional statement timeout per connection

## 2. Data grid & table view

* "Referenced by": the rows in other tables that point at this one, and composite foreign keys ([plan](.agents/plans/fk-referenced-by.md))
* Quick search across every column of a table
* Date/time picker for timestamp cells
* Undo/redo for staged grid edits
* Bulk edit: "Set column to…" on the picked rows
* Multi-column sorting
* Column stats: count, distinct, null %, min/max, top values
* Reorder columns by dragging a header (turned off today: the grid addresses cells by the result's column order)
* Write a binary value from a file in the value dialog (the preview — image or hex — already reads the real bytes; see IDEAS.md)

## 3. SQL editor

* Auto-complete for tables, columns, keywords, and schemas from live introspection
* Engine-specific highlighting (today one SQL grammar serves all three)
* Format SQL ([plan](.agents/plans/format-sql.md))
* A pinned connection per query tab, so `BEGIN`/`COMMIT`, `SET`, and temp tables persist between runs ([plan](.agents/plans/pinned-connections-transactions.md))
* Messages tab: Postgres `NOTICE`, MySQL warnings
* Query parameters (`:name`, `$1`) prompted for and bound
* A row-limit guard on unbounded `SELECT`s, with a "Fetched first N rows" banner
* Middle-mouse drag for a column (multi-line) selection, like Zed
* Find and replace, multi-cursor, comment toggle (check what `gpui-kit`'s editor already offers)

## 4. Schema browser & editor

* Tree navigation: databases > schemas > tables / views
* Read-only DDL text, triggers, and check constraints in the structure tab ([plan](.agents/plans/structure-ddl-view.md))
* Search the text of view, routine, and trigger definitions
* Free-text column types in the structure tab (today a fixed per-engine list)
* View and routine definition editor

## 5. Import, export & productivity

* Stream a large export instead of reading the whole table into memory first
* Import CSV into an existing table ([plan](.agents/plans/csv-import.md))
* Backup via `pg_dump` / `mysqldump`
* Local query history, searchable by date, database, or text ([plan](.agents/plans/query-history.md))
* Reusable SQL snippets ([plan](.agents/plans/sql-snippets.md))
* Customizable keyboard shortcuts
* Server activity view (`pg_stat_activity`, `SHOW PROCESSLIST`) with cancel/kill
* "Copy diagnostics" in the Help menu: version, OS, recent errors

## 6. Platform & release

* macOS and Windows builds in CI
* Signed macOS and Windows builds, and an update check
* Full audit against a screen reader on each platform
* Dialog transitions that can be turned off (needs upstream support in `gpui-kit`)

## Not planned for now

* ER diagram generation — costly, and rarely used day to day.
* Cloud sync of connections or snippets — the config directory is plain JSON and can be synced by other means.
* A plugin system, non-SQL databases, and AI query generation — each is a large design decision.
