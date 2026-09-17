# SQLite full column editing (rebuild procedure)

Only needed for SQLite retype / nullability / default / add-or-drop-primary-
key-or-foreign-key on an existing column, which plain `ALTER TABLE` cannot
express. Implements SQLite's own documented procedure
(sqlite.org "Making Other Kinds Of Table Schema Changes"):

1. `PRAGMA foreign_keys=OFF` (must run outside a transaction on the same
   connection right before the rest).
2. Begin a transaction (first real use of one — everything up to here in the
   app has run statement-by-statement with no transaction wrapper, per
   `Connection::run_script`'s own doc comment,
   `src/db/connection.rs:194-199` — this phase is the exception, and needs a
   `Connection` method that holds one connection for a multi-statement
   transaction rather than going back through the pool per statement, since
   the whole rebuild must be atomic).
3. `CREATE TABLE new_<name> (...)` with the edited column list.
4. `INSERT INTO new_<name> SELECT <mapped columns> FROM <name>` — a dropped
   column is left out of the `SELECT` list; a renamed column is aliased.
5. `DROP TABLE <name>`.
6. `ALTER TABLE new_<name> RENAME TO <name>`.
7. Recreate every index/trigger/view that pointed at the old table (read via
   `sqlite_master` `sql` column before step 3, replayed after step 6 — a
   generated index recreates cleanly from `IndexDef`; a trigger/view is
   opaque here, so its stored `CREATE ...` text is replayed verbatim).
8. `PRAGMA foreign_key_check` — abort (roll back) if it reports a violation
   the rebuild introduced.
9. Commit, then `PRAGMA foreign_keys=ON`.

This needs a new `Connection::run_transaction(statements: Vec<String>) ->
Result<()>` (or similar) since every existing write path in `connection.rs`
opens per-statement against the pool. Flag for confirmation before starting:
this is the riskiest phase in the plan (it moves every row of the table
through a scratch copy) and probably wants its own explicit go-ahead, plus a
decision on whether it's in scope at all for a first pass versus documenting
the limitation and leaving SQLite on the Phase 2 safe subset.
