<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/zippa-icon.svg">
  <img alt="Zippa DB logo" src="assets/zippa-icon-light.svg" width="120">
</picture>

# Zippa DB

> A fast, lightweight, native database client for **PostgreSQL**, **MySQL**, and **SQLite** — built with Rust and GPUI.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/readme-mockup.svg">
  <img alt="Zippa DB app window" src="assets/readme-mockup.svg">
</picture>

> **Status:** beta — usable day to day, still early.

## Highlights

- **Connections.** Saved connections in tabs, passwords in the OS keychain, per-connection color tags, SSL modes, SSH tunnels. Open a SQLite file that lives on an SSH host: it is copied down over SFTP, edited locally, and uploaded when you say so, with a warning if the server's copy changed in the meantime. Your workspace restores on launch.
- **Safety modes.** Read-only (enforced by the server *and* your SQL), confirm writes, staged edits, or auto-apply — per connection.
- **SQL editor.** Highlighting, table/column/keyword completion, run statement / selection / script, per-tab pinned connections so transactions survive across runs, `EXPLAIN` plan trees, and `:name` / `$name` query variables.
- **Tables.** Page, sort, filter, and quick-search; stage cell edits, new rows, and deletions, then apply them together.
- **Structure.** Edit columns, indexes, and foreign keys with a generated-SQL preview; switch any structure tab to its SQL panel for the table's or view's current definition as read-only text, with its check constraints and triggers, copyable or openable in a query tab; see every table, its columns, and the foreign keys between them as a pannable, zoomable ER diagram, exportable to SVG.
- **Import & export.** Results out as CSV, TSV, JSON, Markdown, or SQL `INSERT`; SQL dumps in (plain, gzip, bzip2, zstd) with progress and per-error choices.
- **Charts & dashboards.** Chart any query result as a line, bar, area, or stacked bar, and save named dashboards of SQL widgets (charts or grids) that share variables.
- **Backups.** Whole-database backups through `pg_dump` / `mysqldump`, optionally gzipped, with the password kept off the command line.
- **Agents panel.** An optional chat beside a connection that talks to an OpenAI-compatible endpoint you configure. Nothing is sent until you set one and send a message; what the model can see and do is chosen per connection (none, schema only, read-only SQL, CRUD, or full), reads run in a read-only session, and every write can require your approval. Schema and row data you let it read go to that endpoint.
- **Query history.** Every statement you run from a query tab, kept per connection on disk (or in memory only, or not at all), searchable, with an All connections view and a one-click reopen in a new tab — `Ctrl`/`Cmd`+`Shift`+`H`. Count and age limits are in Settings › History.
- **Server tools.** A console of every statement sent, connection information, process list, server variables, query digest, and user management (Postgres/MySQL), plus SQLite maintenance.
- **Keyboard first.** Every command is in the menu bar, nearly all with a shortcut — `Ctrl`+`/` lists them all.

## Getting started

Install pre-built packages from [the latest release](https://github.com/Alanaktion/zippa-db/releases/latest), available for macOS, Linux, and Windows.

Connections, tabs, settings, and query history live in the OS config dir (`~/Library/Application Support/zippa-db`, `~/.config/zippa-db`, or `%APPDATA%\zippa-db`); passwords stay in the OS keychain.

## Building from source

Requires Rust 1.95+. macOS needs full Xcode (Metal toolchain); on Linux run `./script/linux` for build dependencies.

```bash
git clone https://github.com/Alanaktion/zippa-db.git
cd zippa-db
cargo run --release
```

## Built with

[GPUI](https://www.gpui.rs/) · [GPUI Kit](https://gpui-kit.com) · [SQLx](https://github.com/launchbadge/sqlx) · [Tokio](https://tokio.rs/) · [keyring](https://github.com/open-source-cooperative/keyring-rs)

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Live database tests are `#[ignore]`d: `./script/test-db up`, then `cargo test -- --ignored live_`.

Packaged with [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager) (`app`/`dmg`, `appimage`/`deb`, `nsis`). Builds are unsigned for now, so expect a Gatekeeper/SmartScreen prompt.

## Contributing

Issues and pull requests welcome. AI-coded contributions are welcome too — please keep them minimal, maintainable, and correct.
