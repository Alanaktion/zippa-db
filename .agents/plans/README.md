# Plans

Design notes for features that are not built yet, each linked from its line in
[TODO.md](../../TODO.md). A plan is removed once its feature ships; the code
and the architecture notes in [AGENTS.md](../../AGENTS.md) are then the record.

Where a plan builds on the SQL dump import, that is `src/db/import/` and
`src/ui/import_dialog.rs`: `Connection::import_dump` holds one dedicated
connection for the whole run, which is the pattern the pinned-connection and
CSV import plans reuse.
