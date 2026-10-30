# Property tests and a write corpus for the SQL scanners

## Context
`db/statement.rs` (split, `at_cursor`, `first_write`), `db/import/splitter.rs`, `db/completion.rs`, and `db/ddl.rs` all tokenize arbitrary SQL by hand. The classifier is a safety boundary: `ReadOnly` and `ConfirmWrites` lean on it, so a miss runs a write unconfirmed. Hand-written cases only cover shapes someone thought of.

## Plan
1. **Write corpus** (`src/db/statement.rs` tests): a table of `(engine, sql, expected)` where every row that must count as a write is listed — `SELECT ... INTO`, `INTO OUTFILE`/`DUMPFILE`, `WITH ... INSERT/UPDATE/DELETE`, `EXPLAIN ANALYZE <write>`, `COPY ... FROM`, `CALL`, `DO`, `SELECT` of a function, comments/quotes hiding keywords, MySQL `#` and backslash escapes, Postgres `E'..'` and dollar quotes. Add reads that must stay reads. New classifier bugs add a row here first.
2. **Properties** with `proptest` (dev-dependency), over generated and mutated SQL:
   - no scanner (`split`, `first_write`, `with_limit`, `clamp_limit`, `completion::complete`, the import splitter) panics on any input, including invalid byte boundaries at the caret;
   - `split` ranges are in order, within the buffer, on char boundaries, and joining them with the gaps reproduces the input;
   - appending a comment or whitespace to a statement never changes its classification;
   - a statement classified as a read stays a read when wrapped in `( ... )` or prefixed with a comment.
3. Keep case counts small (a few hundred) so `cargo test` stays fast; commit `proptest-regressions/`.

## Not doing
Fuzzing with `cargo-fuzz` — revisit only if the properties find real bugs.
