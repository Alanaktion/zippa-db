# ⚡ Zippa DB

> A fast, lightweight, cross-platform database client built with **Rust** and **GPUI**.

Zippa DB is a native desktop app for browsing, querying, and editing
**PostgreSQL**, **MySQL**, and **SQLite** databases. It draws with Zed's
GPU-accelerated UI framework, so large result sets stay smooth to scroll, and
it is built to be safe to point at a database that matters, with all of the
features you'd expect from a modern database client.

> **Status:** beta quality — usable day to day, still early.

---

## 🌟 What it does

* **Connections.** Save connections and open several at once, each in its own
  tab. Passwords live in the OS keychain, never on disk. An optional colour (say,
  red for production) marks a connection's tab and title bar, so you always
  know where you are. Your open connections, tabs, and query buffers come back on
  the next launch.
* **Safety modes.** Each connection is *read-only*, *confirm writes*, *staged*
  (the default: edits wait until you apply them), or *auto-apply*. Read-only is
  enforced by the server and by a client-side check of your SQL.
* **SQL editor.** Highlighted SQL, run the current statement, the selection, or
  the whole script, with one result tab per statement. `EXPLAIN` and
  `EXPLAIN ANALYZE` show up as a readable plan tree.
* **Tables.** Open a table to page, sort, and filter it. Edit cells, add rows,
  and mark rows for deletion; pending changes are marked in the grid and
  written together when you apply them.
* **Structure.** Edit a table's columns, indexes, and foreign keys, and preview
  the generated `ALTER TABLE` statements before they run.
* **Finding things.** Search the whole schema for a table, column, index,
  routine, or trigger, and jump anywhere with the quick switcher.
* **Import and export.** Export or copy results as CSV, TSV, JSON, Markdown, or
  SQL `INSERT`, and import a SQL dump (plain, gzip, bzip2, or zstd) with
  progress and a choice of what to do on error.
* **Server tools.** A console of every statement the app has sent, plus a
  process list, server variables, and a query digest on Postgres and MySQL, and
  a maintenance panel on SQLite.
* **Keyboard first.** Every command has a shortcut, shown in its tooltip and in
  the menu bar. Press `Ctrl+/` for the full searchable list. `Cmd` on macOS is
  `Ctrl` on Linux and Windows.
* **Settings.** Bundled and custom themes, light or dark, fonts, and page size,
  from the gear in the toolbar or `Cmd`/`Ctrl` + `,`.

Result sets are read into memory whole and the grid virtualizes only the
drawing, so a very large `SELECT` costs memory in proportion to its size. The
table view pages instead.

---

## 🚀 Getting started

You will need [Rust](https://www.rust-lang.org/) 1.95 or newer, plus:

* **macOS:** full Xcode (not just the Command Line Tools) with the Metal
  toolchain, since GPUI compiles Metal shaders at build time. If
  `xcrun --find metal` fails, run `xcodebuild -downloadComponent MetalToolchain`,
  and if `xcode-select -p` points at the Command Line Tools, either
  `sudo xcode-select -s /Applications/Xcode.app` or prefix commands with
  `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.
* **Linux:** run `./script/linux` to install the build dependencies for most
  distributions (`./script/linux test` adds `lld` for running the tests).
  Wayland, Vulkan, and D-Bus are needed at run time.
* **Windows:** Vulkan or Direct3D 12 drivers.

```bash
git clone https://github.com/Alanaktion/zippa-db.git
cd zippa-db
cargo run --release
```

A plain `cargo run` works too. Dependencies are optimized even in the dev
profile, but release is the smoothest.

### Where things are stored

In the OS config directory (`~/Library/Application Support/zippa-db` on macOS,
`~/.config/zippa-db` on Linux, `%APPDATA%\zippa-db` on Windows):

* `connections.json` holds connection metadata. Passwords go to the OS
  credential store instead.
* `workspace.json` holds the open connections and their tabs. Query buffers are
  stored as plaintext, so treat it accordingly if one holds a pasted secret.
* `settings.json` holds your settings.
* `themes/` takes your own theme files, which then appear in the theme pickers.

---

## 🏗 How it's built

| Layer | Technology |
| --- | --- |
| **UI framework** | [GPUI](https://www.gpui.rs/) (GPU-accelerated desktop framework) |
| **Components** | [GPUI Kit](https://gpui-kit.com) (table, code editor, dock, theming) |
| **Database access** | [SQLx](https://github.com/launchbadge/sqlx) (async SQL for Rust) |
| **Async runtime** | [Tokio](https://tokio.rs/), on a dedicated thread pool beside the UI |
| **Secrets** | OS keychain via [keyring](https://github.com/open-source-cooperative/keyring-rs) |

The code is split into two halves:

* **`src/db/`** is everything that talks to a database, with no UI. It has one
  common shape for the three engines, with each engine's pool setup, SQL, and
  value decoding in its own file, and the shared query, safety, import, export,
  and persistence code beside them. GPUI and SQLx each want their own async
  executor, so database work runs on a small Tokio runtime and hands its result
  back to the UI over a channel.
* **`src/ui/`** is a tree of GPUI views. The root holds a tab per connection,
  and each tab is either the connection launcher or an open session with a
  sidebar and a dock of query, table, and structure tabs. Views talk to their
  parents by emitting events rather than reaching into their state.

---

## 🧪 Development

CI runs these on every push:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

A few tests run against a live MySQL or Postgres server and are `#[ignore]`d
by default; each one's doc comment says which container it expects.

### Packaging

Precompiled binaries are built with
[`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager), which
reads `[package.metadata.packager]` in `Cargo.toml`:

```bash
cargo install cargo-packager --locked

cargo packager --release --formats app,dmg        # macOS
cargo packager --release --formats appimage,deb   # Linux (needs fuse/libfuse2)
cargo packager --release --formats nsis           # Windows
```

Packaged builds are unsigned for now, so macOS shows a Gatekeeper "unidentified
developer" prompt (needs approval in System Settings) and Windows shows a
SmartScreen warning.

---

## 🤝 Contributing

Contributions are welcome — open an issue or submit a pull request.

AI-coded contributions are welcome in this project, but please put in some
effort to ensure the code is minimal, maintainable, and correct.
