# ⚡ Zippa DB

> A blazing-fast, lightweight, cross-platform database management tool built with **Rust** and **GPUI**.

Zippa DB combines the raw performance of Zed's GPU-accelerated interface engine with the reliability of async Rust. Designed to compete with tools like TablePlus, Zippa DB aims for sub-millisecond tab switching, virtualized streaming for massive datasets, and native support for PostgreSQL, MySQL, and SQLite.

> **Status:** early. You can save connections, connect to PostgreSQL, MySQL, or SQLite, switch databases, browse tables and views in the sidebar, open them in tabs, run queries, and read the results. Everything else in [TODO.md](TODO.md) is still ahead.

---

## 🌟 Planned Features

* **GPU-Accelerated Data Grid:** Instant rendering and smooth scrolling over millions of rows with minimal memory usage.
* **Inline Editing with Staging:** Edit table cells in-memory, review your staged SQL `UPDATE` queries, and commit changes atomically.
* **Engine-Aware SQL Editor:** Query editor with auto-completion, multi-statement execution, and inline syntax highlighting.
* **Native Drivers:** Built on top of `SQLx` with connection pooling, SSH tunneling, and SSL/TLS support.
* **Cross-Platform:** Native look and feel across macOS, Linux, and Windows.

---

## 🛠 Tech Stack

| Layer | Technology |
| --- | --- |
| **UI Framework** | [GPUI](https://www.gpui.rs/) (GPU-accelerated desktop framework) |
| **Components** | [GPUI Kit](https://gpui-kit.com) (table, code editor, theming) |
| **Database Engine** | [SQLx](https://github.com/launchbadge/sqlx) (Async SQL for Rust) |
| **Async Runtime** | [Tokio](https://tokio.rs/), on a dedicated thread pool beside the UI |
| **Secrets** | OS keychain via [keyring](https://github.com/open-source-cooperative/keyring-rs) |

---

## 📁 Project Layout

```
src/
├── main.rs           # Entry point: opens the GPUI window
├── app.rs            # Root view: one tab per open connection
├── keymap.rs         # Key bindings (platform-aware via `secondary`)
├── db/
│   ├── mod.rs        # Module map and re-exports
│   ├── config.rs     # Engine, SafetyMode, ConnectionConfig
│   ├── connection.rs # The live pool, and the shared read/write paths
│   ├── sql.rs        # Quoting and bind placeholders for generated SQL
│   ├── statement.rs  # Splitting and classifying the user's own SQL
│   ├── postgres.rs   # Per-engine pool setup and value formatting
│   ├── mysql.rs
│   ├── sqlite.rs
│   ├── query.rs      # QueryResult
│   ├── export.rs     # Laying a result out as CSV, JSON, or SQL INSERT
│   ├── runtime.rs    # Tokio runtime bridging sqlx futures back to GPUI
│   └── store.rs      # connections.json + OS keychain
├── settings.rs       # Settings global, settings.json, theme/appearance
└── ui/
    ├── welcome/        # Connection manager (launcher + editor dialog)
    │   ├── mod.rs      # The launcher: searchable cards, most-recent first
    │   ├── card.rs     # One connection card
    │   └── editor.rs   # The new/edit connection dialog
    ├── session/        # Open connection: query tabs, editor, grid
    │   ├── mod.rs      # The view: tabs, running, files, panes
    │   ├── tab.rs      # What a tab holds and how its last run went
    │   └── sidebar.rs  # Object list and the filter over it
    ├── query_editor.rs # SQL editor (Cmd+Enter to run)
    ├── sql_file.rs     # Native open/save dialogs for .sql files
    ├── settings_window.rs # Settings window (Cmd+,)
    ├── value_dialog.rs # One cell's value, in full (Cmd+Shift+V)
    ├── filter_bar.rs   # The filter lines above a table view
    ├── table_view/     # Table opened from the sidebar
    │   ├── mod.rs      # The view: paging, sorting, applying edits
    │   └── sql.rs      # The statements it generates
    └── data_grid/      # Virtualized result grid
        ├── mod.rs      # The view and the commands its owner drives it with
        ├── delegate.rs # The result, plus everything staged on top of it
        ├── format.rs   # Laying one value out for reading
        └── layout.rs   # Sizing the columns
```

Connections are stored in `connections.json` under your OS config directory
(`~/Library/Application Support/zippa-db` on macOS). Passwords are never written
there — they go to the OS credential store, keyed by connection id.

Connections open in tabs along the top of the window: the `+` opens the
connection manager in a new one, connecting turns that tab into the session, and
disconnecting turns it back. Each connection keeps its own sidebar, query tabs,
and results, so switching between them picks up where you left off.

The window is remembered across restarts: `workspace.json` records which
connections were open, each one's tabs — query buffers as typed, and the tables
and structure views that were open — and which tab was in front, so the next
launch reconnects and restores them. Query text is stored there in plaintext,
like the connection metadata, so treat it accordingly if a buffer holds pasted
literals. A buffer is written at checkpoints (opening, closing, running, saving)
and on a normal quit, so text typed right before a crash may be lost.

Settings live beside it in `settings.json` and are written as you change them.
Drop theme files into a `themes/` directory next to it — each one a
`{"themes": [ … ]}` set in the component library's format — and they show up in
the theme pickers.

---

## 🚀 Getting Started

### Prerequisites

* [Rust](https://www.rust-lang.org/) 1.85+ (this crate uses edition 2024)
* OS dependencies:
  * **macOS:** Xcode (not just the Command Line Tools) with the Metal toolchain installed — GPUI compiles Metal shaders at build time. If `xcrun --find metal` fails, run `xcodebuild -downloadComponent MetalToolchain`.
  * **Linux:** `libxkbcommon`, plus Wayland or X11 dev libraries (`sudo apt install libxkbcommon-dev libwayland-dev`)
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

### Tests

```bash
cargo test
```

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

| Action | Shortcut |
| --- | --- |
| Run the selection, or the statement the caret is in | `Cmd`/`Ctrl` + `Enter` |
| Run every statement in the buffer | `Cmd`/`Ctrl` + `Shift` + `Enter` |
| Show the statement's execution plan without running it | `Cmd`/`Ctrl` + `E` |
| Show the plan and run the statement for actual times | `Cmd`/`Ctrl` + `Shift` + `E` |
| Give up on a running query | `Cmd`/`Ctrl` + `.` |
| New connection (or another connection tab) | `Cmd`/`Ctrl` + `N` |
| Close the active connection | `Cmd`/`Ctrl` + `Shift` + `W` |
| Next / previous connection | `Cmd`/`Ctrl` + `Shift` + `]` / `[` |
| New query tab | `Cmd`/`Ctrl` + `T` |
| Close the active tab | `Cmd`/`Ctrl` + `W` |
| Next / previous tab | `Ctrl` + `Tab` / `Ctrl` + `Shift` + `Tab` (or `Ctrl` + `PageDown` / `PageUp`) |
| Refresh the schema and the open table | `Cmd`/`Ctrl` + `R` |
| Open a SQL file | `Cmd`/`Ctrl` + `O` |
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
| Settings | `Cmd`/`Ctrl` + `,` (or the gear in the toolbar) |
| Close the settings window | `Cmd`/`Ctrl` + `W`, `Alt` + `F4`, or `Escape` |
| Keyboard shortcut list | `Ctrl` + `/` (or Help menu) |

---

## 🗺 Roadmap

* [x] Initial GPUI window and layout setup
* [x] Connection manager & driver abstractions (`PostgreSQL`, `MySQL`, `SQLite`)
* [x] Launcher-first welcome screen: searchable connection cards, a new/edit dialog, and one tag + colour per connection that follows it into the tab and session
* [x] Result grid: virtualized, dense, monospaced, auto-sized columns
* [x] Resizable panes, database switcher, and table/view list in the sidebar
* [x] Connection tabs: several databases open at once, each with its own session
* [x] Query tabs, each with its own editor and result set
* [x] Regex filter over the sidebar's table list
* [x] Dedicated table view: paging, row limit, click-to-sort columns, and a filter bar (including `IN` with a subquery)
* [ ] Data grid pagination, row limits, and specialized cell renderers
* [x] Staged cell editing in a table tab, with a per-connection safety mode: read-only, confirm every write, apply by hand, or write when the selection leaves the row
* [x] Drag across rows to select them, delete them from the row's menu, and fill in new rows in the grid itself — edited cells, new rows, and rows marked for deletion are coloured in the grid and written together when applied
* [x] Export a whole table or the rows picked out of it to CSV, JSON, or SQL `INSERT`, honouring the current filter and sort
* [x] Copy rows or a column from the grid as TSV, CSV, JSON, a Markdown table, SQL `INSERT`, plain values, or a SQL `IN` list
* [x] SQL editor with SQL syntax highlighting: `Cmd+Enter` runs the statement the caret is in, `Cmd+Shift+Enter` the whole buffer, `Cmd+.` gives up on a run
* [x] Query plan viewer: `Cmd+E` shows a statement's plan as a tree with costs, rows, timing, and warnings, and `Cmd+Shift+E` runs it for actual times
* [x] Open and save `.sql` files through the native file dialogs
* [x] Settings window: page size, editor and grid fonts, striped rows, always-on scrollbars, theme, light/dark
* [x] Query cancellation and multi-statement scripts, with a result tab per statement
* [ ] Auto-completion from live introspection
* [ ] SSH tunneling & SSL configuration interface
* [ ] Schema inspector & visual DDL builder

Full feature breakdown lives in [TODO.md](TODO.md).

---

## 🤝 Contributing

Contributions are welcome — open an issue or submit a pull request.

1. Fork the project
2. Create your feature branch (`git checkout -b feature/AmazingFeature`)
3. Commit your changes
4. Push the branch and open a pull request
