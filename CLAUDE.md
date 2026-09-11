# AGENTS.md

Zippa DB: cross-platform desktop database client (Postgres/MySQL/SQLite) built on GPUI (Zed's GPU-accelerated UI framework) via `gpui-kit`, with `sqlx` for database access. Early stage — see `TODO.md` for the full feature backlog and `README.md` for status/shortcuts.

## Commands

```bash
cargo run              # debug build; opt-level=1 for own code, 3 for deps (see Cargo.toml) — GPUI is unusably slow otherwise
cargo run --release    # smoothest experience
cargo test              # all tests
cargo test <name>       # single test by substring match
cargo test -p zippa-db db::tests::   # module-scoped
```

macOS build needs full Xcode (not just CLI tools) for the Metal toolchain — `xcrun --find metal` must succeed. If `xcode-select -p` points at CLI tools, either `sudo xcode-select -s /Applications/Xcode.app` or prefix commands with `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

There is no separate lint command configured beyond `cargo clippy` / `cargo fmt` (standard Rust tooling, not wired into any script here).

## Architecture

**Two async worlds, bridged by a channel.** GPUI has its own executor; `sqlx` needs a Tokio reactor. `src/db/runtime.rs` owns a lazily-initialized, dedicated 2-thread Tokio runtime. All database work is submitted via `runtime::spawn(future)`, which returns a `oneshot::Receiver` that a GPUI `cx.spawn` task awaits. Never call sqlx directly from a GPUI view — always go through this runtime. Under `#[cfg(test)]`, `spawn` instead runs the future to completion inline on the calling thread (GPUI's single-threaded test scheduler aborts if woken from another thread), so tests get deterministic results without touching the real background runtime.

**Engine abstraction (`src/db/`).** `mod.rs` defines `Engine` (Postgres/MySql/Sqlite), `ConnectionConfig` (serializable, no password), and `Connection` (holds a `Pool` enum wrapping the sqlx pool type plus the password in memory — never persisted). Each engine's specifics — pool setup, connection string building, SQL for listing databases/objects, and decoding a `sqlx::Row` cell to a display `String` — live in `postgres.rs` / `mysql.rs` / `sqlite.rs` behind a common shape (`connect`, `DATABASES_SQL`, `OBJECTS_SQL`, `cell`). `Connection::run_query` dispatches on the pool variant into a shared generic `fetch_all` that works over any `sqlx::Database`. To add a capability across engines, add the shared entry point in `mod.rs` and implement per-engine in the three sibling files.

**Persistence split.** `db/store.rs`: connection *metadata* goes to `connections.json` in the OS config dir; passwords go to the OS keychain via the `keyring` crate, keyed by connection `Uuid`. Never write a password into the JSON store.

**UI is a view tree of GPUI entities (`Entity<T: Render>`), wired with events, not callbacks.** `app::Workspace` is the root: it owns a tab per open connection, each holding either `ui::welcome::Welcome` (connection manager) or `ui::session::Session` (an open connection). `WelcomeEvent::Connected` turns the tab it came from into a session and `SessionEvent::Disconnected` turns it back, both via `cx.subscribe_in`; closing a tab drains that connection's pool. Tabs therefore nest: connections on the workspace bar, queries and tables on the session's own bar. `Session` owns tabs (`SessionTab`: either a `QueryEditor`+`DataGrid` pair or a `TableView` opened from the sidebar), the object sidebar (regex-filterable list of tables/views from `Connection::objects`), and resizable panes. Follow the same pattern for new screens/panels: a child entity that emits its own event enum, subscribed to by its parent rather than reaching into parent state directly.

**Keybindings are centralized in `src/keymap.rs`**, bound once at startup, using GPUI's `secondary` modifier (Cmd on macOS, Ctrl elsewhere) so all shortcuts are automatically platform-correct. Actions are declared with the `actions!` macro next to the view that handles them (e.g. `NewTab`/`CloseTab` in `session.rs`, `RunQuery` in `query_editor.rs`) and bound to specific GPUI context paths (e.g. `"QueryEditor > Input"`) — check existing bindings' context-path comments before adding new ones, since GPUI resolves the most specific matching context.

**Query results** flow through `db::query::QueryResult` (columns + rows of `Cell` + elapsed time) and render via `ui::data_grid::DataGrid`, a virtualized read-only grid. `ui::table_view::TableView` builds on top of it for the sidebar's "open table" flow, adding paging/limit/sort by generating and re-running SQL rather than manipulating an in-memory result set.

**Identifier quoting**: `db::quote_identifier` decides per-engine whether a name needs quoting (bare lowercase/digits/underscore is left alone) — used whenever generating SQL (e.g. table view paging) rather than the user's own editor buffer, which is run verbatim.
