# Plans

Implementation notes for work that needs details in context before it is built.
The backlog itself is tracked as GitHub issues; a plan is not an index of it.
A plan is removed once its work ships; the code and the architecture notes in
[AGENTS.md](../../AGENTS.md) are then the record.

Where a plan builds on the SQL dump import, that is `src/db/import/` and
`src/ui/import_dialog.rs`: `Connection::import_dump` holds one dedicated
connection for the whole run, which is the pattern the pinned-connection and
CSV import plans reuse.
