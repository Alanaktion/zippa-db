# ⚡ Zippa DB

> A fast, lightweight, cross-platform database client built with **Rust** and **GPUI**.

Zippa DB pairs Zed's GPU-accelerated UI framework with async Rust database
drivers to browse, query, and edit **PostgreSQL**, **MySQL**, and **SQLite**
from one native window.

> **Status:** 0.1 — usable day to day, still young. The list below is what
> ships; [TODO.md](TODO.md) is what is still ahead, and [AUDIT.md](AUDIT.md)
> lists the known gaps in what ships.

---

## 🌟 Features

* **Connections** — a searchable launcher of saved connections, most recent
  first. Passwords live in the OS keychain, never on disk. Each connection
  can carry a tag and colour (say, red `Production`) that follows it into its
  tab and across the top of its session.
* **Safety modes** — per connection: *read-only* (enforced by the server and
  by a client-side check on your SQL), *confirm writes*, *staged* (the
  default: edits wait until you apply them), or *auto-apply*.
* **Tabs and docking** — several connections open at once, each with its own
  sidebar and a dock of query, table, and structure tabs that split and
  reorder by dragging. The window's connections, tabs, and query buffers are
  restored on the next launch.
* **SQL editor** — SQL highlighting; run the statement under the caret, the
  selection, or the whole buffer (a script runs in one transaction on
  Postgres and SQLite); one result tab per statement; cancel a running query;
  open and save `.sql` files.
* **Query plans** — `EXPLAIN` and `EXPLAIN ANALYZE` as a readable tree with
  costs, rows, timing, and warnings.
* **Console** — every statement the connection has sent, the app's own reads
  and writes included, tagged apart from what a query tab actually ran.
* **Process list** — who is connected to the server and what they're running
  right now (Postgres and MySQL), pick rows and end them. SQLite has none.
* **Server variables** — every runtime setting (Postgres and MySQL), searchable
  and filterable to just what has changed from its compiled default. SQLite
  has none.
* **Query digest** — the slow and frequent statements the server has seen
  (`pg_stat_statements` on Postgres, `performance_schema` on MySQL), ranked
  by mean time. Says plainly when the instrumentation isn't installed or is
  off, rather than erroring. SQLite has none.
* **Table view** — paging, a row limit, click-to-sort, a filter bar
  (including `IN` with a subquery), a row panel showing the focused row as
  fields, and a jump from a foreign key to the row it references. A binary
  cell's value dialog re-reads the real bytes and shows an image preview or a
  hex dump, rather than just the `<N bytes>` the grid shows.
* **Staged editing** — type into cells, add rows, and mark rows for deletion;
  every pending change is marked in the grid (tint *and* underline or
  strike-through) and written together as `UPDATE`/`INSERT`/`DELETE`.
* **Structure tab** — edit a table's columns, indexes, and foreign keys and
  preview the generated `ALTER TABLE` statements (or, on SQLite, the table
  rebuild) before they run.
* **Schema search** — find any table, view, column, index, routine, or
  trigger (`Cmd+Shift+O`), and a quick switcher over tabs, objects,
  databases, and commands (`Cmd+K`).
* **Import & export** — export a table or the picked rows to CSV, TSV, JSON,
  Markdown, or SQL `INSERT`; copy rows or a column in the same formats, as
  plain values, or as an `IN` list; import a SQL dump (plain, gzip, bzip2, or
  zstd) with a progress bar and a stop / roll back / continue error policy.
* **Settings** — theme (22 bundled sets, plus your own) and light/dark,
  editor and grid fonts, page size, striped rows, always-on scrollbars, and
  whether typing `NULL` means SQL `NULL`.
* **Keyboard first** — every command has a shortcut, shown in its tooltip and
  in the menu bar, and a searchable list of them all (`Ctrl+/`).

Result sets are read into memory whole and the grid virtualizes only the
drawing, so a very large `SELECT` costs memory in proportion to its size; the
table view pages instead.

---

## 🛠 Tech Stack

| Layer | Technology |
| --- | --- |
| **UI Framework** | [GPUI](https://www.gpui.rs/) (GPU-accelerated desktop framework) |
| **Components** | [GPUI Kit](https://gpui-kit.com) (table, code editor, dock, theming) |
| **Database Engine** | [SQLx](https://github.com/launchbadge/sqlx) (Async SQL for Rust) |
| **Async Runtime** | [Tokio](https://tokio.rs/), on a dedicated thread pool beside the UI |
| **Secrets** | OS keychain via [keyring](https://github.com/open-source-cooperative/keyring-rs) |

---

## 📁 Project Layout

```
src/
├── main.rs              # Entry point: opens the GPUI window
├── app.rs               # Root view: one tab per open connection
├── workspace_state.rs   # workspace.json: open connections and their tabs
├── keymap.rs            # Key bindings (platform-aware via `secondary`)
├── menu.rs              # OS menu bar, from the same actions the keymap binds
├── settings.rs          # Settings global, settings.json, themes
├── db/                  # Database access — no UI
│   ├── config.rs        # Engine, SafetyMode, ConnectionConfig
│   ├── connection.rs    # The live pool, and the shared read/write paths
│   ├── postgres.rs      # Per-engine pool setup, SQL, and value decoding
│   ├── mysql.rs
│   ├── sqlite.rs
│   ├── catalog.rs       # The whole schema once per session, for search
│   ├── schema.rs        # One table's columns, indexes, and foreign keys
│   ├── sql.rs           # Quoting and bind placeholders for generated SQL
│   ├── statement.rs     # Splitting and classifying the user's own SQL
│   ├── plan.rs          # Reading EXPLAIN output into a tree
│   ├── query.rs         # QueryResult and cells
│   ├── query_log.rs     # Every statement a connection has sent, for the console
│   ├── export.rs        # CSV, TSV, JSON, Markdown, SQL INSERT
│   ├── import/          # SQL dump import: runner, reader, splitter
│   ├── runtime.rs       # Tokio runtime bridging sqlx futures back to GPUI
│   └── store.rs         # connections.json + OS keychain
└── ui/
    ├── welcome/         # Connection manager: launcher, card, editor dialog
    ├── session/         # An open connection
    │   ├── mod.rs       # The view: sidebar + dock, and the event hub
    │   ├── panel.rs     # One dock tab (query, table, or structure)
    │   ├── tab.rs       # What a tab holds and how its last run went
    │   ├── tabs.rs      # Opening, closing, and switching tabs
    │   ├── running.rs   # Running SQL and EXPLAIN, confirming, cancelling
    │   ├── files.rs     # .sql files and the import dialog
    │   ├── metadata.rs  # Databases, objects, the catalog, switching database
    │   ├── state.rs     # Snapshot and restore for workspace.json
    │   └── sidebar.rs   # Object list and the filter over it
    ├── data_grid/       # Virtualized result grid
    │   ├── mod.rs       # The view and the commands its owner drives it with
    │   ├── delegate.rs  # The result, plus everything staged on top of it
    │   ├── clipboard.rs # Copy, Copy as, and snapshots
    │   ├── format.rs    # Laying one value out for reading
    │   └── layout.rs    # Sizing the columns
    ├── table_view/      # A table opened from the sidebar
    │   ├── mod.rs       # The view: paging, sorting, applying edits
    │   ├── sql.rs       # The statements it generates
    │   ├── export.rs    # Export to a file
    │   ├── footer.rs    # The status line and its buttons
    │   └── row_panel.rs # The focused row as fields
    ├── schema_view/     # The structure tab
    │   ├── mod.rs       # The form, diffed against what was loaded
    │   ├── columns.rs, indexes.rs, foreign_keys.rs, layout.rs
    │   ├── sql.rs       # ALTER TABLE for Postgres and MySQL
    │   └── rebuild.rs   # SQLite's copy-and-swap rebuild
    ├── query_editor.rs  # SQL editor
    ├── plan_view.rs     # EXPLAIN tree
    ├── filter_bar.rs    # The filter lines above a table view
    ├── value_dialog.rs  # One cell's value, in full
    ├── console.rs       # Every statement this connection has sent
    ├── process_list.rs  # Who is connected and what they're doing (Postgres/MySQL)
    ├── server_variables.rs # Runtime settings, searchable (Postgres/MySQL)
    ├── query_digest.rs  # Slow/frequent statements, ranked (Postgres/MySQL)
    ├── import_dialog.rs # SQL dump import
    ├── quick_switcher.rs, schema_search.rs, shortcuts_dialog.rs
    ├── settings_window.rs, sql_file.rs
    └── tests/           # UI tests, one file per area
```

[AGENTS.md](AGENTS.md) explains how the pieces fit together.

### Where things are stored

Everything lives in the OS config directory (`~/Library/Application
Support/zippa-db` on macOS, `~/.config/zippa-db` on Linux, `%APPDATA%\zippa-db`
on Windows):

* `connections.json` — connection metadata. Passwords are never written here;
  they go to the OS credential store, keyed by connection id.
* `workspace.json` — which connections were open, each one's tabs (query
  buffers as typed, and the tables and structure views that were open), and
  which tab was in front. Query text is stored in plaintext, so treat it
  accordingly if a buffer holds pasted secrets. It is written at checkpoints
  (opening, closing, running, saving) and on a normal quit, so text typed
  right before a crash may be lost.
* `settings.json` — written as you change settings.
* `themes/` — drop theme files here, each a `{"themes": [ … ]}` set in the
  component library's format, and they show up in the theme pickers.

---

## 🚀 Getting Started

### Prerequisites

* [Rust](https://www.rust-lang.org/) 1.95 or newer (latest stable is what CI uses)
* OS dependencies:
  * **macOS:** Xcode (not just the Command Line Tools) with the Metal toolchain installed — GPUI compiles Metal shaders at build time. If `xcrun --find metal` fails, run `xcodebuild -downloadComponent MetalToolchain`.
  * **Linux:** run `./script/linux`, which installs the build dependencies for most distributions (on Debian/Ubuntu the essentials are `libxkbcommon-x11-dev`, `libwayland-dev`, `libfontconfig-dev`, `libssl-dev`, and `lld`).
  * **Windows:** Vulkan / Direct3D 12 drivers

### Building

```bash
git clone https://github.com/Alanaktion/zippa-db.git
cd zippa-db
cargo run
```

Dependencies are compiled with optimizations even in the dev profile
(`[profile.dev.package."*"]`), because GPUI's per-frame layout and text shaping
are unusably slow otherwise. For the smoothest experience, run
`cargo run --release`.

On macOS, if your active developer directory still points at the Command Line Tools (`xcode-select -p`), the Metal compiler won't be found. Either point it at Xcode (`sudo xcode-select -s /Applications/Xcode.app`) or override it per build:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo run
```

### Checks

CI runs the same three commands on every push:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

A few tests run against a live MySQL or Postgres server and are `#[ignore]`d
by default; each one's doc comment says which container it expects.

---

## 📦 Packaging

Precompiled binaries are built with [`cargo-packager`](https://github.com/crabnebula-dev/cargo-packager), which reads `[package.metadata.packager]` in `Cargo.toml` and produces the right installer for whichever platform it runs on:

```bash
cargo install cargo-packager --locked
```

```bash
# macOS: Zippa DB.app + a .dmg to distribute it in
cargo packager --release --formats app,dmg

# Linux: an AppImage and a .deb
cargo packager --release --formats appimage,deb

# Windows: an NSIS installer (run on a Windows host; not yet exercised here)
cargo packager --release --formats nsis
```

Building the AppImage needs `fuse`/`libfuse2` installed on the machine doing the packaging (separate from `script/linux`'s build-time dependencies, which are for compiling the app itself). Packaged builds are unsigned for now, so macOS shows a Gatekeeper "unidentified developer" prompt (right-click → Open works around it) and Windows shows a SmartScreen warning.

---

## ⌨️ Shortcuts

`Cmd` on macOS, `Ctrl` on Linux and Windows.

Most of these are also in the menu bar — File, Edit, Query, Table and View — where each item carries the key it answers to. An item that has nothing to act on is dimmed rather than hidden.

| Action | Shortcut |
| --- | --- |
| Run the selection, or the statement the caret is in | `Cmd`/`Ctrl` + `Enter` |
| Run every statement in the buffer | `Cmd`/`Ctrl` + `Shift` + `Enter` |
| Show the statement's execution plan without running it | `Cmd`/`Ctrl` + `E` |
| Show the plan and run the statement for actual times (Postgres and MySQL; SQLite has no `EXPLAIN ANALYZE`) | `Cmd`/`Ctrl` + `Shift` + `E` |
| Give up on a running query | `Cmd`/`Ctrl` + `.` |
| New connection (or another connection tab; on the connection manager it opens the editor dialog) | `Cmd`/`Ctrl` + `N` |
| Close the active connection | `Cmd`/`Ctrl` + `Shift` + `W` |
| Next / previous connection | `Cmd`/`Ctrl` + `Shift` + `]` / `[` |
| New query tab | `Cmd`/`Ctrl` + `T` |
| Close the active tab | `Cmd`/`Ctrl` + `W` |
| Next / previous tab | `Ctrl` + `Tab` / `Ctrl` + `Shift` + `Tab` (or `Ctrl` + `PageDown` / `PageUp`) |
| Refresh the schema and the open table | `Cmd`/`Ctrl` + `R` |
| Open the console of every statement sent | `Cmd`/`Ctrl` + `` ` `` |
| Open the process list (Postgres and MySQL) | `Cmd`/`Ctrl` + `Shift` + `P` |
| Open the server variables (Postgres and MySQL) | `Cmd`/`Ctrl` + `Shift` + `V` |
| Open the query digest (Postgres and MySQL) | `Cmd`/`Ctrl` + `Shift` + `D` |
| Open a SQL file | `Cmd`/`Ctrl` + `O` |
| Search the schema for a column, index, routine, or trigger | `Cmd`/`Ctrl` + `Shift` + `O` |
| Open the quick switcher over tabs, objects, databases and actions | `Cmd`/`Ctrl` + `K` |
| Import a SQL dump into the connection | `Cmd`/`Ctrl` + `Shift` + `I` |
| Move between cells | `↑` `↓` `←` `→`, `Home` / `End`, `PageUp` / `PageDown` |
| Copy the selected cell, or the selected rows | `Cmd`/`Ctrl` + `C` |
| Copy the rows with a header line, tab-separated | `Cmd`/`Ctrl` + `Shift` + `C` |
| Select the row the selection is on, or unselect it | `Space` |
| Extend the row selection up / down | `Shift` + `↑` / `↓` |
| Select every row | `Cmd`/`Ctrl` + `A` |
| Unselect every row | `Cmd`/`Ctrl` + `Shift` + `A`, or `Escape` |
| Edit the selected cell in a table tab | `Enter` |
| Set the selected cell to `NULL` | `Cmd`/`Ctrl` + `Shift` + `N` |
| Open the selected cell's value in a dialog | `Cmd`/`Ctrl` + `Shift` + `V` |
| Save the value dialog | `Cmd`/`Ctrl` + `Enter` |
| Add a row to fill in | `Cmd`/`Ctrl` + `Shift` + `I` |
| Mark the selected rows for deletion, or discard selected new rows | `Cmd`/`Ctrl` + `Backspace` |
| Take the deletion mark off again | `Cmd`/`Ctrl` + `Shift` + `Backspace` |
| Save the active tab, or write a table tab's staged edits | `Cmd`/`Ctrl` + `S` |
| Discard a table tab's staged edits | `Cmd`/`Ctrl` + `Z` |
| Save the active tab under a new name | `Cmd`/`Ctrl` + `Shift` + `S` |
| Toggle the row panel beside a table | `Cmd`/`Ctrl` + `\` |
| Settings | `Cmd`/`Ctrl` + `,` (or the gear in the toolbar) |
| Close the settings window | `Cmd`/`Ctrl` + `W`, `Alt` + `F4`, or `Escape` |
| Keyboard shortcut list | `Ctrl` + `/` (or Help menu) |
| Connect from the connection editor / close it | `Cmd`/`Ctrl` + `Enter` / `Escape` |

---

## 🤝 Contributing

Contributions are welcome — open an issue or submit a pull request.

1. Fork the project
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes
4. Push the branch and open a pull request
