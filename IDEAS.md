# Ideas

Rougher and less committed than [TODO.md](TODO.md): candidate features not yet
triaged into the backlog, written from the angle of a typical web developer's
day with a database client — inspecting data a framework produced, chasing a
slow endpoint, poking at a staging server they don't fully trust themselves
on. Ranked by impact for that reader. An idea that gets picked up moves to
TODO.md (with a design note in `.agents/plans/` if it's large); this file is
where it waits before that.

## 1. Binary editor (write half)

The read half shipped: a binary cell's value dialog now re-reads the real
bytes for a row a table view can address, sniffs common image formats
(PNG/JPEG/GIF/WebP/BMP) and shows a preview, or a hex dump otherwise
(`src/db/binary.rs`, `Connection::fetch_binary`, `value_dialog::open_binary`).
What's left is writing a new value back — a "Load from file…" action in the
value dialog to stage a binary value, parallel to the existing text box.

* The hard part: `Connection::typed_placeholder` binds every parameter as
  text (`AGENTS.md` § "Values are text, both ways") — there's no path today
  to bind raw bytes back. Needs either a base64/hex round-trip through the
  cast (Postgres: `decode($1, 'hex')::bytea`; MySQL: `UNHEX($1)`; SQLite
  already accepts a `X'...'` literal or blob binding directly) or a small
  `Cell::Binary` variant that `execute_with` binds natively per engine,
  bypassing the text path for that one column.
* Pairs with the audit's open item on missing Postgres `cell` arms — this is
  the same "driver type sqlx can decode but the app can't show" gap, just
  for the type that shows up most in a typical app schema.

## 2. Server admin & performance tools (rest of it)

The process list shipped (`ui/process_list.rs`, `Connection::processes`/
`kill_process`): `pg_stat_activity` / `information_schema.processlist` shown
in a `DataGrid` the same way the console shows the query log, pick rows and
"End Selected" to `pg_terminate_backend`/`KILL` them, gated off entirely for
SQLite (there's no server to ask). It always asks before ending a
connection — there's no staged-edit concept for this the way `ConfirmWrites`
has for a cell edit, so a confirmation dialog stands in regardless of safety
mode.

Server variables shipped too (`ui/server_variables.rs`,
`Connection::server_variables`): `pg_settings` / `SHOW VARIABLES`, searchable
by name with a client-side "changed only" toggle, same SQLite gate. One piece
of the original idea is still open:

* **Slow/frequent query digest** — `performance_schema.events_statements_summary_by_digest`
  on MySQL, `pg_stat_statements` on Postgres when it's installed (detect and
  say so plainly when it isn't, rather than erroring). Even a bare sortable
  table of digest/calls/mean time answers "what's actually slow" without
  leaving the app.

## 3. Chart integration for results and performance data

`gpui-kit` already ships chart components (`chart::{AreaChart, BarChart,
LineChart, PieChart, RadarChart}`, plus `plot::Plot` with `#[derive(IntoPlot)]`)
— this isn't a "build a charting library" project, it's wiring a result set
into one that exists. Candidate spots, roughly in order of how directly they
reuse work already planned:

* **Query plan costs** — `plan_view.rs` today prints each `PlanNode`'s cost
  and timing as text in a tree; a bar per node (by cost or actual time,
  Postgres/MySQL `EXPLAIN ANALYZE`) makes the expensive node jump out instead
  of requiring a read of every row.
* **Column stats** — TODO.md's planned "count, distinct, null %, min/max, top
  values" panel is naturally a bar chart of top values once that data exists;
  worth designing the two together rather than shipping the numbers first
  and bolting a chart on later.
* **Ad-hoc chart from a result** — pick two columns (typically a date/category
  and a number) in a query result or table view and get a quick line/bar
  chart, the way a spreadsheet's "quick chart" works. High value for a web
  developer eyeballing a time series or a group-by count without exporting
  to something else first.
* **Admin dashboards** — a natural home for the process-count/slow-query data
  from idea 2 once it exists.

## 4. Database user & permission management

`CREATE ROLE`/`CREATE USER`, `GRANT`/`REVOKE`, and a read-only view of who
can do what — useful for a web developer setting up a per-service account or
checking why a migration user can't `ALTER TABLE`. Real value, but the
highest-risk item on this list (it edits the server's own security, and the
GRANT/REVOKE grammar diverges more between Postgres and MySQL than anything
else in the app). Worth sequencing after the others:

* Ship read-only first — list roles/users and their grants, no editing —
  which is low-risk and still answers the question that comes up most
  ("what can this user actually do").
* Editing follows `schema_view`'s existing pattern: build the statement,
  preview it, run it through the same `SafetyMode` gate as everything else.
  `ConfirmWrites`/`Staged` matter more here than almost anywhere in the app.

## Smaller ideas worth keeping around

* **A safety nudge tied to the connection's own tag.** `ConnectionConfig`
  already carries a `tag`/`TagColor` (e.g. red "Production") and a
  `SafetyMode`. A connection tagged as a production-looking color that's set
  to `AutoApply` is exactly the footgun the tag color exists to warn about —
  a one-line banner or a confirmation-on-connect costs little and reuses
  data the app already persists.
* **A tree view for JSON/JSONB values.** `value_dialog`/`format_value`
  already lays JSON out over multiple lines with `serde_json`; a collapsible
  tree (keys foldable, values still editable as text) would help far more
  than plain indentation for the settings/metadata/feature-flag blobs that
  show up constantly in web app schemas.
