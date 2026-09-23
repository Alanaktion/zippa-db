# Pinned connection per query tab, with transactions

## Context
`Connection::run_query`/`run_script` (`src/db/connection.rs`) run every statement through the sqlx pool, so consecutive statements in one editor tab can land on any of `POOL_SIZE = 5` connections. Consequences today: `BEGIN` in one statement and `COMMIT` in the next are on different connections, `SET search_path`/`USE`/temp tables do not persist between runs. (`run_script` now wraps one script in a transaction on Postgres and SQLite, so this is about *separate* runs in one tab.) The dump import already holds its own one-off connection (`Connection::import_dump`); this plan makes that a shared building block.

Decisions (from user): each query tab gets its own dedicated connection for its lifetime (psql-like session), and explicit `BEGIN`/`COMMIT`/`ROLLBACK` typed in the editor works. A manual-commit toggle and auto-savepoints are follow-ups.

## Design

### 1. Run over one connection — `src/db/connection.rs`
- Refactor `fetch_all` and `execute_with` to take `&mut DB::Connection` instead of `&sqlx::Pool<DB>`. The pool path becomes `let mut conn = pool.acquire().await?; fetch_all(&mut *conn, ...)`, so behaviour for generated reads is unchanged and there is one implementation. `describe` runs on the same connection after the row stream is dropped.
- New `pub struct PinnedConnection` (in `src/db/pinned.rs`, re-exported from `db/mod.rs`): `Arc<tokio::sync::Mutex<PinnedInner>>` where `PinnedInner` is an enum of `PoolConnection<Postgres|MySql|Sqlite>`, plus `engine`, backend id, and a `TxnState`.
- `Connection::checkout(&self) -> Result<PinnedConnection>`; `PinnedConnection::run_query(&self, conn: &Connection, sql)` and `run_script`. Both call `Connection::refuse_write` first so `ReadOnly` is enforced exactly as on the pool path. The pinned connection is opened on the same read-only-configured pool, so the server-side read-only setting also applies.
- `run_script` on a pinned connection runs every statement on that connection (this fixes the "hops connections" problem for scripts too). The editor always runs through the pin; `Connection::run_query`/`run_script` stay for generated SQL from `TableView`, the sidebar, and schema view.

### 2. Transaction state
- `enum TxnState { Idle, Open, Failed }`. Failed is Postgres's "current transaction is aborted, commands ignored until end of transaction block".
- Spike first (step 1 of sequencing): check whether sqlx 0.9 exposes the transaction status per connection (Postgres tracks it from `ReadyForQuery`). If it does, read it after every statement: exact for all cases. If not, derive state from the statement classifier (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`, `SAVEPOINT`, `RELEASE`) and errors; MySQL implicit commits from DDL make that inexact, so a MySQL fallback queries `@@in_transaction`-equivalent (check availability; `information_schema.innodb_trx` by `CONNECTION_ID()` otherwise). Decide after the spike; do not guess.
- `PinnedConnection::state()` is read by the UI after each run.

### 3. Classifier change — `src/db/statement.rs`
Transaction control (`BEGIN`, `START TRANSACTION`, `COMMIT`, `END`, `ROLLBACK`, `ABORT`, `SAVEPOINT`, `RELEASE [SAVEPOINT]`, `SET TRANSACTION`) writes no data and must not count as a write: it must work on `ReadOnly` connections, and must not raise a `ConfirmWrites` prompt on `BEGIN`. `COMMIT` with `ConfirmWrites` is the one judgment call: the writes were already confirmed when they ran, so treat it as neutral too. Update `first_write` tests.

### 4. Real cancel — same file
Aborting the Tokio task (today's `Cmd+.`) drops the future but the server keeps running the statement, and the connection is drained before reuse. With a pinned connection that matters more, so add `PinnedConnection::cancel()`:
- Postgres: remember `pg_backend_pid()` at checkout; cancel with `SELECT pg_cancel_backend($pid)` on a pooled connection.
- MySQL: remember `CONNECTION_ID()`; `KILL QUERY <id>` on a pooled connection.
- SQLite: `sqlite3_interrupt` via sqlx's locked handle (`lock_handle`) if available in 0.9; otherwise abort only.
After a cancel inside a transaction, refresh `TxnState` (Postgres will report Failed). Keep the abort handle as the fallback.

### 5. Pool budget
Pinned connections must not starve the shared users (sidebar, table views, schema view). Add `PINNED_MAX = 8` enforced by a `tokio::sync::Semaphore` in `Connection` and set `max_connections = POOL_SIZE + PINNED_MAX` in the three engine `connect` functions, so at least 5 stay free for shared use. Hitting the cap gives `Error: too many query tabs are holding connections (8). Close one to open another.` rather than a hang. Set `acquire_timeout` (10 s) so a genuinely exhausted pool errors instead of blocking the UI.
SQLite: file-based, a pinned connection with an open write transaction holds the write lock, so table-view edits in another tab fail with "database is locked". Check `sqlite.rs` for a busy timeout and add one (e.g. 5 s) if absent; the error hint should say a query tab holds an open transaction.

### 6. UI — `src/ui/session/{mod,tab}.rs`, `src/ui/query_editor.rs`
- `TabContent::Query` gets `pinned: Option<PinnedConnection>`, checked out lazily on the first run (so a tab that never runs costs nothing) and dropped when the tab closes.
- `Session::send` passes the pin; `set_running`, cancel, and results flow are unchanged apart from calling `PinnedConnection::cancel`.
- Status chip in the editor's status area: `Transaction open` (blue), `Transaction failed — roll back` (danger). Words always shown, never colour alone. Buttons (icon + accessibility label, tooltip with action) `Commit` and `Rollback` appear only while the state is not `Idle`; they run `COMMIT`/`ROLLBACK` on the pin.
- Guards: closing a tab, closing the connection tab, switching database (`with_database` makes a new pool, invalidating pins), disconnecting, and quitting the app all check for an open transaction and ask `Roll back and close?` in a dialog. Closing without a transaction is silent, as now.
- Tab strip shows a small dot plus the tooltip "Open transaction" on a tab with `Open`/`Failed` state so it cannot be forgotten in a background tab. Text alternative via the tooltip and the accessibility label.
- `Refresh` on the sidebar (a pool read) will not see uncommitted work from a pinned tab; mention in the status chip tooltip.

### 7. Sequencing
1. Spike: sqlx transaction-status API, sqlite `lock_handle`, busy timeout. Write findings at the top of this file.
2. Refactor `fetch_all`/`execute_with` to `&mut Connection`; all existing tests green with no behaviour change.
3. `PinnedConnection` + `checkout` + `run_query`/`run_script` on it; unit tests on SQLite.
4. Classifier update.
5. `TxnState` tracking.
6. Session/tab wiring, chip, buttons, close guards.
7. `cancel`, pool budget, docs.

## Critical files
- new: `src/db/pinned.rs`, `src/ui/tests/transactions.rs`
- edit: `src/db/{connection,statement,sqlite,postgres,mysql,mod}.rs`, `src/ui/session/{mod,tab,panel}.rs`, `src/ui/query_editor.rs`, `src/app.rs` (quit/close guards), `TODO.md`, `CLAUDE.md` ("Running SQL": editor runs on a per-tab pinned connection; pool for generated SQL)
- reuse: `runtime::spawn` + abort handle, `Connection::refuse_write`, `TempDatabase` fixture, `Status`/`notify_error`.
- `Connection::import_dump` should adopt `PinnedConnection` instead of its own held-connection code.

## Testing
- `db` tests (SQLite temp file): `BEGIN; INSERT; ROLLBACK` across separate `run_query` calls on one pin leaves no row; the same on the pool path (documenting why it is needed) does not; `PRAGMA foreign_keys=ON` persists across calls on a pin; temp table persists; script runs on one connection; `ReadOnly` refuses a write on a pin; `BEGIN`/`ROLLBACK` allowed on `ReadOnly`.
- Classifier tests for each transaction keyword.
- UI tests: chip appears after `BEGIN` and clears after `COMMIT`; closing a tab with an open transaction asks; `Cmd+.` cancel leaves the tab usable; 9th pinned tab reports the cap error; switching database with an open transaction asks.
- Postgres/MySQL behaviour (aborted state, `KILL QUERY`, `pg_cancel_backend`) cannot run in CI without servers: cover with a manual checklist and unit tests of state derivation from fixtures.
- Manual: `cargo run` against Postgres and MySQL; `BEGIN; UPDATE…;` in tab A, read from tab B and from the table view (should not see it), `ROLLBACK`; run `SELECT pg_sleep(30)` and cancel; confirm server-side query is gone in `pg_stat_activity`.

## Risks / open questions
- The `fetch_all` refactor touches every read; keep it a pure mechanical change in its own commit.
- MySQL implicit commits and `SET autocommit=0` make client-side state tracking approximate if the spike finds no server-side signal.
- A connection the server drops (idle timeout) while pinned needs a reconnect path; that belongs to the connection-health item and should surface as `Error: connection lost` until it is built, with the pin discarded so the next run checks out a fresh one.
- Pinned connections count against server `max_connections`: up to 13 per open connection tab.
