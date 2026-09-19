# TODO

## 1. Connection & Security

### Authentication & Security

* [x] Native username/password authentication (passwords stored in the OS keychain)
* [ ] SSH Tunneling (Password and Private Key / Agent support)
* [ ] SSL/TLS connection modes (`disable`, `prefer`, `require`, `verify-full`)

### Connection Management

* [x] Multi-environment tagging & color-coding (one tag + colour per connection; Red header for `Production`, Green for `Local`)
* [ ] Workspace state persistence (reopen previous tabs on startup)

## 2. Tabular Data Grid (Data Browser)

### Filtering, Sorting & Pagination

* [ ] Quick search bar (global string matching across columns)

### Specialized Cell Renderers

* [ ] Date/Time picker for timestamp fields

## 3. SQL Query Editor

### Text Editing

* [ ] Syntax highlighting tailored per database engine dialect
* [ ] Auto-complete (tables, columns, SQL keywords, schemas) powered by live introspection

### Result Sets & Output Diagnostics

* [x] Result status shows rows affected/returned and elapsed time; error handling; console shows query history via result bar for multiple statements (syntax highlighting and detailed warnings not yet included)
* [x] Query plan viewer: `EXPLAIN` / `EXPLAIN ANALYZE` as a readable tree with cost, rows, timing, and warnings, alongside the result grid

## 4. Database Schema Browser & Modeler

### Structure & Object Explorer

* [x] Left sidebar navigation tree: Databases > Schemas > Tables / Views

### View & Routine Management

* [ ] Stored procedure and function editor with argument syntax checking
* [ ] View definition viewer and editor

## 5. Utility & Productivity Features

### Import & Export Tools

* [x] Export selected rows or entire tables to `CSV`, `JSON`, and SQL `INSERT` statements
* [ ] Stream a large export instead of reading the whole table into memory first
* [ ] Import SQL dumps
* [ ] Import CSV data into existing table
* [ ] Database backup dump (`pg_dump` / `mysqldump` integration)

### Query History & Snippets

* [ ] Local execution history log (searchable by date, database, or query string)
* [ ] Reusable SQL code snippets library

## 6. Accessibility

* [ ] Full audit against a screen reader on each platform
