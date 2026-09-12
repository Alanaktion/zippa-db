# TODO

## 1. Connection & Security

### Database Driver Support

* [x] PostgreSQL (including CockroachDB / Redshift support)
* [x] MySQL / MariaDB
* [x] SQLite

### Authentication & Security

* [x] Native username/password authentication (passwords stored in the OS keychain)
* [ ] SSH Tunneling (Password and Private Key / Agent support)
* [ ] SSL/TLS connection modes (`disable`, `prefer`, `require`, `verify-full`)

### Connection Management

* [ ] Multi-environment tagging & color-coding (e.g., Red header for `Production`, Green for `Local`)
* [x] Several connections open at once, one tab each, with their own sessions
* [x] Per-connection safety mode: read-only (refuses writes, and opens the session read-only at the server), confirm every write, stage inline edits until applied, or apply them when the selection leaves the row
* [ ] Grouping & categorizing connection workspaces
* [ ] Workspace state persistence (reopen previous tabs on startup)

---

## 2. Tabular Data Grid (Data Browser)

### High-Performance Virtual Scrolling

* [ ] Fast rendering for 100k+ rows with low memory overhead (virtualized rendering is in; rows are still all held in memory as strings, untested at 100k)
* [x] Fixed header, scrollable body, and adjustable column widths

### Inline Data Editing (Buffer Model)

* [x] Staged edits (cell highlighting before committing), written by an `UPDATE` keyed on the primary key, or on `rowid` / `ctid` when there is none; views and MySQL tables without a key stay read-only
* [x] Batch `COMMIT` / `DISCARD` (`Cmd+S` / `Cmd+Z`); multi-cell selection and bulk editing still open
* [x] Refreshing, paging, sorting, and changing the row limit ask before they throw staged edits away
* [x] Insert rows in the grid (a row filled in by hand, written by an `INSERT` that leaves untyped columns to the server) and delete rows from the row's context menu, which always shows the statements first
* [x] Multi-row selection by dragging across rows, or shift-clicking to the end of a range
* [ ] Foreign key lookups directly from cells (click to jump to referenced row)

### Filtering, Sorting & Pagination

* [x] Single-column sorting: a header cycles descending, ascending, and back to the order the server sent; multi-column still open
* [ ] Quick search bar (global string matching across columns)
* [ ] Advanced filtering GUI (e.g., `WHERE status = 'active' AND created_at > ...`)
* [x] Configurable row limit & offset pagination (`LIMIT 100 OFFSET 0`) in the table view, starting from the page size in the settings

### Specialized Cell Renderers

* [ ] Custom modals for `JSON` / `JSONB` viewing & formatting
* [ ] Large text BLOB viewer with word-wrap
* [ ] Date/Time picker for timestamp fields
* [x] Null state toggles (`NULL` vs. empty string `""`): `Cmd+Shift+N` sets `NULL`, and a setting decides whether typing `NULL` means the text or the value

## 3. SQL Query Editor

### Text Editing Essentials

* [ ] Syntax highlighting tailored per database engine dialect
* [ ] Auto-complete (tables, columns, SQL keywords, schemas) powered by live introspection
* [ ] Line numbers, code folding, and auto-indentation
* [x] Open and save `.sql` files with the native file dialogs (no unsaved-changes tracking yet)

### Query Execution Engine

* [ ] Run single query under cursor or selected text block (`Cmd+Enter` runs the whole buffer today)
* [ ] Run entire script with multi-statement support
* [x] Multi-tab editor (unlimited concurrent query tabs)
* [ ] Cancellable query execution (kill running background tasks)

### Result Sets & Output Diagnostics

* [ ] Multi-result set tabs for queries returning multiple tables
* [ ] Execution statistics banner (affected rows, execution time in `ms`)
* [ ] Detailed error console with SQL error line highlighting

## 4. Database Schema Browser & Modeler

### Structure & Object Explorer

* [ ] Left sidebar navigation tree: Databases > Schemas > Tables / Views / Functions / Procedures / Sequences (tables and views listed; database switcher in place)
* [ ] Object searching / fuzzy filtering (`Cmd+K` quick switcher) — sidebar has a regex filter; the quick switcher is still open

### Table Schema Inspector & DDL Generator

* [ ] View and modify table structure (Columns, Types, Nullability, Default values)
* [ ] Index manager (Primary Keys, Unique indexes, Composite indexes)
* [ ] Foreign Key relationship builder
* [ ] Live SQL preview generator (shows the `ALTER TABLE` statement before executing changes)

### View & Routine Management

* [ ] Stored procedure and function editor with argument syntax checking
* [ ] View definition viewer and editor

## 5. Utility & Productivity Features

### Import & Export Tools

* [ ] Export selected rows or entire tables to `CSV`, `JSON`, and SQL `INSERT` statements
* [ ] Database backup dump (`pg_dump` / `mysqldump` integration) and SQL file restoration

### Query History & Snippets

* [ ] Local execution history log (searchable by date, database, or query string)
* [ ] Reusable SQL code snippets library

### App Customization

* [x] Dark and Light theme support (follows the OS, or pinned to one in the settings)
* [x] Settings window (`Cmd+,`): page size, editor and grid font, theme per mode
* [ ] Customizable keyboard shortcuts map
