# Plans

Design notes for features that are not built yet, each linked from its line in
[TODO.md](../../TODO.md). A plan is removed once its feature ships; the code
and the architecture notes in [AGENTS.md](../../AGENTS.md) are then the record.

Some plans refer to work that has since shipped under its old plan name:

* **sql-import** — the SQL dump import: `src/db/import/` (runner, reader,
  splitter) and `src/ui/import_dialog.rs`. It holds one dedicated connection
  for the whole run (`Connection::import_dump`), which is the pattern the
  pinned-connection and CSV import plans build on.
* **sqlite-schema-editing** — the SQLite table rebuild in
  `src/ui/schema_view/rebuild.rs`.
* **workspace-state-persistence** — `src/workspace_state.rs`, which also
  saves unsaved query buffers.

Since these plans were written, `Connection::run_script` has started wrapping
a script in one transaction on Postgres and SQLite; the pinned-connection plan
still applies to *separate* runs in one tab.
