# Changelog

All notable changes to Zippa DB are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [0.1.0] — unreleased

The first release: a native client for PostgreSQL, MySQL, and SQLite.

### Features

- **Connections:** a searchable launcher of saved connections, passwords in
  the OS keychain, an optional per-connection colour, and several connections
  open at once in tabs. The connection editor tests a connection without
  closing, and Connect saves first (its menu connects without saving).
  PostgreSQL and MySQL connections choose an SSL mode (disable, prefer — the
  default — require, verify CA, or verify full) and can name a CA certificate
  and a client certificate and key. They can also connect through an SSH
  tunnel, signing in with the SSH agent, a private key file, or a password,
  and checking the jump host against `~/.ssh/known_hosts`.
- **Connection health:** a dropped connection or an unresponsive server is
  told in words rather than as the driver's error, a statement gives up
  waiting for a free connection after 10 seconds, and Reconnect
  (`Cmd`/`Ctrl`+`Shift`+`R`, the File menu, or the database picker) opens a
  fresh connection while keeping every tab. Postgres and MySQL connections
  take an optional statement timeout.
- **Safety modes:** read-only (enforced on the server and client side),
  confirm writes, staged (the default), and auto-apply.
- **Workspace restore:** open connections, their tabs, and unsaved query
  buffers come back on the next launch.
- **SQL editor:** run the statement under the caret, the selection, or the
  whole buffer; one result per statement; cancel a run; open and save `.sql`
  files. A script runs in one transaction on every engine and pauses on each
  error to roll back, skip it, or skip every error; a script holding a
  statement no transaction can (`LOCK TABLES`, `VACUUM`, MySQL DDL, its own
  `BEGIN`/`COMMIT`) is flagged in the toolbar and asks before running without
  one. Run Script Ignoring Errors (`Cmd`/`Ctrl`+`Alt`+`Shift`+`Enter`) runs
  every statement with no transaction and lists the failures afterwards.
- **Query plans:** `EXPLAIN` and `EXPLAIN ANALYZE` as a tree with costs, rows,
  timing, and warnings.
- **Table view:** paging, row limit, sorting, a filter bar, a row panel, and
  foreign-key navigation.
- **Staged editing:** edit cells, add rows, and delete rows, written together
  as `UPDATE`/`INSERT`/`DELETE`, with every pending change marked in the grid.
- **Structure tab:** edit columns, indexes, and foreign keys, previewing the
  generated `ALTER TABLE` statements or SQLite table rebuild.
- **Search:** schema search over tables, views, columns, indexes, routines, and
  triggers, and a quick switcher over tabs, objects, databases, and commands.
- **Import and export:** export or copy as CSV, TSV, JSON, Markdown, or SQL
  `INSERT`; copy a column as values or an `IN` list; import SQL dumps (plain,
  gzip, bzip2, zstd) with a progress bar and an error policy.
- **Settings:** 22 bundled theme sets plus user themes, light/dark, fonts,
  page size, striped rows, always-on scrollbars, and `NULL` text handling.
- **Packaging:** `cargo packager` recipes for macOS, Linux, and Windows.

### Pre-release cleanup

- Split the largest views into focused submodules — `ui/session/` (tabs,
  running, files, metadata, state), `ui/data_grid/clipboard.rs`,
  `ui/table_view/` (export, footer), and `ui/schema_view/` (one file per
  section) — and moved every `_for_test` reach-in into a `test_support.rs`.
- Copies stop at the 100,000-row cap before cloning any rows, rather than
  cloning the whole result first.
- The schema catalog counts the entries past its 200,000-entry cap instead of
  building them.
- Schema search says "showing the first 500" only when more than 500 matched,
  and its group headings come from the kinds themselves.
- A header drag no longer reorders the grid's columns out from under its
  cells, edits, and copies.
- Accessibility: typed values in a new row are underlined, so a new row is not
  told by colour alone; the value dialog, the import dialog, and the row
  menu's "View value" show their shortcuts; the filter bar's controls have
  names; the sidebar buttons' accessible names match their visible labels.
- The session sidebar is split into two top tabs: **Schema** (the object list,
  with the filter, schema search, and SQL dump import on one line) and
  **Management** (the console, process list, server variables, and query digest
  as icon rows; SQLite gets the console and a maintenance tab of integrity
  checks, optimize, analyze, vacuum, and WAL checkpoint). Disconnect leaves
  the sidebar, with the File menu and `Cmd`/`Ctrl`+`D` carrying it instead.
- Postgres geometric types (`point`, `line`, `lseg`, `box`, `path`,
  `polygon`, `circle`), `hstore`, the built-in ranges, and `macaddr8` show
  their values and can be edited, rather than standing in as `<POINT>` and
  the like. A Postgres `numeric` keeps the scale it was stored with (`1.5`
  rather than `1.5000`).
- `Cmd`/`Ctrl`+`C` copies a value that was never read back as an empty cell
  and says so, the way `Copy as` already wrote it, rather than putting the
  `<3 bytes>`-style description on the clipboard.
- Declared the minimum Rust version (1.95) and package metadata in
  `Cargo.toml`.
- Brought `README.md`, `AGENTS.md`/`CLAUDE.md`, `TODO.md` (which now absorbs
  the former `IDEAS.md` and `IDEAS2.md`), and `AUDIT.md` in line with the
  code.
