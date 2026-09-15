# TODO

## 1. Connection & Security

### Authentication & Security

* [x] Native username/password authentication (passwords stored in the OS keychain)
* [ ] SSH Tunneling (Password and Private Key / Agent support)
* [ ] SSL/TLS connection modes (`disable`, `prefer`, `require`, `verify-full`)

### Connection Management

* [ ] Multi-environment tagging & color-coding (e.g., Red header for `Production`, Green for `Local`)
* [x] Per-connection safety mode: read-only (refuses writes, and opens the session read-only at the server), confirm every write, stage inline edits until applied, or apply them when the selection leaves the row
* [ ] Workspace state persistence (reopen previous tabs on startup)

---

## 2. Tabular Data Grid (Data Browser)

### High-Performance Virtual Scrolling

* [ ] Fast rendering for 100k+ rows with low memory overhead (virtualized rendering is in; rows are still all held in memory as strings, untested at 100k)

### Inline Data Editing (Buffer Model)

* [ ] Foreign key lookups directly from cells (click to jump to referenced row)

### Filtering, Sorting & Pagination

* [ ] Quick search bar (global string matching across columns)

### Specialized Cell Renderers

* [ ] Date/Time picker for timestamp fields

## 3. SQL Query Editor

### Text Editing Essentials

* [ ] Syntax highlighting tailored per database engine dialect
* [ ] Auto-complete (tables, columns, SQL keywords, schemas) powered by live introspection
* [ ] Line numbers, code folding, and auto-indentation

### Result Sets & Output Diagnostics

* [ ] Detailed console with run queries including syntax highlighting, and their result status (error, warnings, number of rows returned/affected)

## 4. Database Schema Browser & Modeler

### Structure & Object Explorer

* [ ] Left sidebar navigation tree: Databases > Schemas > Tables / Views / Functions / Procedures / Sequences (tables and views listed; database switcher in place)

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


### Accessibility

* [ ] Keyboard access to the grid's row context menu
* [ ] A full audit against a screen reader on each platform
