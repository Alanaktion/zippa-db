# Snapshot tests for generated SQL

## Context
`ddl.rs`, `schema_view/sql.rs`, `schema_view/rebuild.rs`, `table_view/sql.rs`, and `export.rs` produce long multi-line strings. Hand-written `assert_eq!` against them is noisy to write and to update when wording or ordering changes intentionally.

## Plan
1. Add `insta` as a dev-dependency (plain text snapshots, no `serde` features needed).
2. Convert the longest generated-SQL assertions first: SQLite rebuild procedure, Postgres `render_postgres_table`, `ALTER TABLE` diffs per engine, export formats. Leave short assertions as `assert_eq!`.
3. Snapshots live in `snapshots/` beside each test module; review changes with `cargo insta review`. Add `cargo insta test --check` (or plain `cargo test`, which fails on a pending snapshot) to CI so no `.snap.new` is committed.
4. Name snapshots by engine and case so a diff reads as "Postgres add column, not null with default".
5. Document the workflow in AGENTS.md's Tests paragraph.

## Not doing
Snapshotting rendered UI.
