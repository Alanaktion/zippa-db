# Live tests across server versions

## Context
`live-tests.yml` runs the `#[ignore]`d `live_*` tests against the single pair of servers in `compose.yaml`. The code special-cases version differences (MariaDB's `max_statement_time` and missing `--ssl-mode`, the `performance_schema` and `pg_stat_statements` checks), which only one version can exercise.

## Plan
1. Make the image tags in `compose.yaml` variables with today's values as defaults (`${POSTGRES_IMAGE:-...}`, `${MYSQL_IMAGE:-...}`), and have `script/test-db` pass them through.
2. In `live-tests.yml`, add a matrix: Postgres oldest supported and latest; MySQL 8.0 and latest 8.x/9.x; MariaDB LTS. MariaDB needs its own `ZIPPA_TEST_MYSQL_URL` and port.
3. Tests that only make sense on one flavour should skip themselves with a clear message (detect through `Connection::connection_info`'s version) rather than fail.
4. Keep the nightly "skip when main has no new commits" gate; run the full matrix there and only the default pair on push.
5. Record the supported-version range in `README.md` so the matrix has an owner.

## Risks
Matrix runtime and flakiness from container start-up; keep `fail-fast: false` so one flavour does not hide another.
