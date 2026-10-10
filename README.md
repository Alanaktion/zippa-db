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

- **Connections.** Saved connections in tabs, passwords in the OS keychain, per-connection color tags, SSL modes, SSH tunnels. Your workspace restores on launch.
- **Safety modes.** Read-only (enforced by the server *and* your SQL), confirm writes, staged edits, or auto-apply — per connection.
- **SQL editor.** Highlighting, table/column/keyword completion, run statement / selection / script, per-tab pinned connections so transactions survive across runs, `EXPLAIN` plan trees, and `:name` / `$name` query variables.
- **Tables.** Page, sort, and filter; stage cell edits, new rows, and deletions, then apply them together.
- **Structure.** Edit columns, indexes, and foreign keys with a generated-SQL preview.
- **Import & export.** Results out as CSV, TSV, JSON, Markdown, or SQL `INSERT`; SQL dumps in (plain, gzip, bzip2, zstd) with progress and per-error choices.
- **Server tools.** A console of every statement sent, process list, server variables, and query digest (Postgres/MySQL), plus SQLite maintenance.
- **Keyboard first.** Every command has a shortcut — `Ctrl`/`Cmd`+`/` lists them all.

## Getting started

Requires Rust 1.95+. macOS needs full Xcode (Metal toolchain); on Linux run `./script/linux` for build dependencies.

```bash
git clone https://github.com/Alanaktion/zippa-db.git
cd zippa-db
cargo run --release
```

Connections, tabs, and settings live in the OS config dir (`~/Library/Application Support/zippa-db`, `~/.config/zippa-db`, or `%APPDATA%\zippa-db`); passwords stay in the OS keychain.

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
